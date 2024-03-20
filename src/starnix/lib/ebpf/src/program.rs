// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    converter::cbpf_to_ebpf,
    executor::{execute, execute_with_arguments},
    verifier::{
        verify, CallingContext, FunctionSignature, NullVerifierLogger, Type, VerifierLogger,
    },
    EbpfError, MapSchema, MemoryId,
};
use linux_uapi::{bpf_insn, sock_filter};
use std::{collections::HashMap, fmt::Formatter, sync::Arc};
use zerocopy::{AsBytes, FromBytes, NoCell};

/// A counter that allows to generate new ids for parameters. The namespace is the same as for id
/// generated for types while verifying an ebpf program, but it is started a u64::MAX / 2 and so is
/// guaranteed to never collide because the number of instruction of an ebpf program are bounded.
static BPF_TYPE_IDENTIFIER_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX / 2);

pub fn new_bpf_type_identifier() -> MemoryId {
    BPF_TYPE_IDENTIFIER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed).into()
}

pub trait EpbfRunContext {
    type Context<'a>;
}

impl EpbfRunContext for () {
    type Context<'a> = ();
}

#[derive(Clone, Copy, Debug)]
pub struct BpfValue(u64);

static_assertions::const_assert_eq!(
    std::mem::size_of::<BpfValue>(),
    std::mem::size_of::<*const u8>()
);

impl Default for BpfValue {
    fn default() -> Self {
        Self::from(0)
    }
}

impl From<i32> for BpfValue {
    fn from(v: i32) -> Self {
        Self((v as u32) as u64)
    }
}

impl From<u8> for BpfValue {
    fn from(v: u8) -> Self {
        Self::from(v as u64)
    }
}

impl From<u16> for BpfValue {
    fn from(v: u16) -> Self {
        Self::from(v as u64)
    }
}

impl From<u32> for BpfValue {
    fn from(v: u32) -> Self {
        Self::from(v as u64)
    }
}
impl From<u64> for BpfValue {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<usize> for BpfValue {
    fn from(v: usize) -> Self {
        Self(v as u64)
    }
}

impl<T> From<*const T> for BpfValue {
    fn from(v: *const T) -> Self {
        Self(v as u64)
    }
}

impl<T> From<*mut T> for BpfValue {
    fn from(v: *mut T) -> Self {
        Self(v as u64)
    }
}

impl BpfValue {
    pub fn as_u8(&self) -> u8 {
        self.0 as u8
    }

    pub fn as_u16(&self) -> u16 {
        self.0 as u16
    }

    pub fn as_u32(&self) -> u32 {
        self.0 as u32
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }

    pub fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }
}

pub struct EbpfHelper<C: EpbfRunContext> {
    pub index: u32,
    pub name: &'static str,
    pub function_pointer: Arc<
        dyn Fn(&mut C::Context<'_>, BpfValue, BpfValue, BpfValue, BpfValue, BpfValue) -> BpfValue
            + Send
            + Sync,
    >,
    pub signature: FunctionSignature,
}

impl<C: EpbfRunContext> Clone for EbpfHelper<C> {
    fn clone(&self) -> Self {
        Self {
            index: self.index,
            name: self.name,
            function_pointer: Arc::clone(&self.function_pointer),
            signature: self.signature.clone(),
        }
    }
}

impl<C: EpbfRunContext> std::fmt::Debug for EbpfHelper<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("EbpfHelper")
            .field("index", &self.index)
            .field("name", &self.name)
            .field("signature", &self.signature)
            .finish()
    }
}

pub struct EbpfProgramBuilder<C: EpbfRunContext> {
    helpers: HashMap<u32, EbpfHelper<C>>,
    calling_context: CallingContext,
}

impl<C: EpbfRunContext> Default for EbpfProgramBuilder<C> {
    fn default() -> Self {
        Self { helpers: Default::default(), calling_context: Default::default() }
    }
}

impl<C: EpbfRunContext> std::fmt::Debug for EbpfProgramBuilder<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("EbpfProgramBuilder")
            .field("helpers", &self.helpers)
            .field("calling_context", &self.calling_context)
            .finish()
    }
}

impl<C: EpbfRunContext> EbpfProgramBuilder<C> {
    pub fn register_map_reference(&mut self, pc: usize, schema: MapSchema) {
        self.calling_context.register_map_reference(pc, schema);
    }

    pub fn set_args(&mut self, args: &[Type]) {
        self.calling_context.set_args(args);
    }

    // This function signature will need more parameters eventually. The client needs to be able to
    // supply a real callback and it's type. The callback will be needed to actually call the
    // callback. The type will be needed for the verifier.
    pub fn register(&mut self, helper: &EbpfHelper<C>) -> Result<(), EbpfError> {
        self.helpers.insert(helper.index, helper.clone());
        self.calling_context.register_function(helper.index, helper.signature.clone());
        Ok(())
    }

    pub fn load(
        self,
        code: Vec<bpf_insn>,
        logger: &mut dyn VerifierLogger,
    ) -> Result<EbpfProgram<C>, EbpfError> {
        let code = verify(code, self.calling_context, logger)?;
        Ok(EbpfProgram { code, helpers: self.helpers })
    }
}

/// An abstraction over a ebpf program and its registered helper functions.
#[derive(Debug)]
pub struct EbpfProgram<C: EpbfRunContext> {
    pub code: Vec<bpf_insn>,
    pub helpers: HashMap<u32, EbpfHelper<C>>,
}

impl<C: EpbfRunContext> EbpfProgram<C> {
    /// Executes the current program on the provided data.  Warning: If
    /// this program was a cbpf program, and it uses BPF_MEM, the
    /// scratch memory must be provided by the caller to this
    /// function.  The translated CBPF program will use the last 16
    /// words of |data|.
    pub fn run<T: AsBytes + FromBytes + NoCell>(
        &self,
        run_context: &mut C::Context<'_>,
        data: &mut T,
    ) -> u64 {
        self.run_with_slice(run_context, data.as_bytes_mut())
    }

    /// Executes the current program on the provided data.  Warning: If
    /// this program was a cbpf program, and it uses BPF_MEM, the
    /// scratch memory must be provided by the caller to this
    /// function.  The translated CBPF program will use the last 16
    /// words of |data|.
    pub fn run_with_slice(&self, run_context: &mut C::Context<'_>, data: &mut [u8]) -> u64 {
        execute(self, run_context, data)
    }

    pub fn run_with_arguments(&self, run_context: &mut C::Context<'_>, arguments: &[u64]) -> u64 {
        execute_with_arguments(self, run_context, arguments)
    }
}

impl EbpfProgram<()> {
    /// This method instantiates an EbpfProgram given a cbpf original.
    pub fn from_cbpf<T>(bpf_code: &[sock_filter]) -> Result<Self, EbpfError> {
        let code = cbpf_to_ebpf(bpf_code)?;
        let buffer_size = std::mem::size_of::<T>() as u64;
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[
            Type::PtrToMemory {
                id: new_bpf_type_identifier(),
                offset: 0,
                buffer_size,
                fields: Default::default(),
                mappings: Default::default(),
            },
            Type::from(buffer_size),
        ]);
        builder.load(code, &mut NullVerifierLogger)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{conformance::test::parse_asm, FieldMapping, FieldType};
    use linux_uapi::*;
    use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

    const BPF_ALU_ADD_K: u16 = (BPF_ALU | BPF_ADD | BPF_K) as u16;
    const BPF_ALU_SUB_K: u16 = (BPF_ALU | BPF_SUB | BPF_K) as u16;
    const BPF_ALU_MUL_K: u16 = (BPF_ALU | BPF_MUL | BPF_K) as u16;
    const BPF_ALU_DIV_K: u16 = (BPF_ALU | BPF_DIV | BPF_K) as u16;
    const BPF_ALU_AND_K: u16 = (BPF_ALU | BPF_AND | BPF_K) as u16;
    const BPF_ALU_OR_K: u16 = (BPF_ALU | BPF_OR | BPF_K) as u16;
    const BPF_ALU_XOR_K: u16 = (BPF_ALU | BPF_XOR | BPF_K) as u16;
    const BPF_ALU_LSH_K: u16 = (BPF_ALU | BPF_LSH | BPF_K) as u16;
    const BPF_ALU_RSH_K: u16 = (BPF_ALU | BPF_RSH | BPF_K) as u16;

    const BPF_ALU_OR_X: u16 = (BPF_ALU | BPF_OR | BPF_X) as u16;

    const BPF_LD_W_ABS: u16 = (BPF_LD | BPF_ABS | BPF_W) as u16;
    const BPF_LD_W_MEM: u16 = (BPF_LD | BPF_MEM | BPF_W) as u16;
    const BPF_JEQ_K: u16 = (BPF_JMP | BPF_JEQ | BPF_K) as u16;
    const BPF_JSET_K: u16 = (BPF_JMP | BPF_JSET | BPF_K) as u16;
    const BPF_RET_K: u16 = (BPF_RET | BPF_K) as u16;
    const BPF_RET_A: u16 = (BPF_RET | BPF_A) as u16;
    const BPF_ST_REG: u16 = BPF_ST as u16;
    const BPF_MISC_TAX: u16 = (BPF_MISC | BPF_TAX) as u16;

    fn with_prg_assert_result(
        prg: &EbpfProgram<()>,
        mut data: seccomp_data,
        result: u32,
        msg: &str,
    ) {
        let return_value = prg.run(&mut (), &mut data);
        assert_eq!(return_value, result as u64, "{}: filter return value is {}", msg, return_value);
    }

    #[test]
    fn test_filter_with_dw_load() {
        let test_prg = [
            // Check data.arch
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 4 },
            sock_filter { code: BPF_JEQ_K, jt: 1, jf: 0, k: AUDIT_ARCH_X86_64 },
            // Return 1 if arch is wrong
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: 1 },
            // Load data.nr (the syscall number)
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 0 },
            // Always allow 41
            sock_filter { code: BPF_JEQ_K, jt: 0, jf: 1, k: 41 },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_ALLOW },
            // Don't allow 115
            sock_filter { code: BPF_JEQ_K, jt: 0, jf: 1, k: 115 },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_TRAP },
            // For other syscalls, check the args
            // A common hack to deal with 64-bit numbers in BPF: deal
            // with 32 bits at a time.
            // First, Load arg0's most significant 32 bits in M[0]
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 16 },
            sock_filter { code: BPF_ST_REG, jt: 0, jf: 0, k: 0 },
            // Load arg0's least significant 32 bits into M[1]
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 20 },
            sock_filter { code: BPF_ST_REG, jt: 0, jf: 0, k: 1 },
            // JSET is A & k.  Check the first 32 bits.  If the test
            // is successful, jump, otherwise, check the next 32 bits.
            sock_filter { code: BPF_LD_W_MEM, jt: 0, jf: 0, k: 0 },
            sock_filter { code: BPF_JSET_K, jt: 2, jf: 0, k: 4294967295 },
            sock_filter { code: BPF_LD_W_MEM, jt: 0, jf: 0, k: 1 },
            sock_filter { code: BPF_JSET_K, jt: 0, jf: 1, k: 4294967292 },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_TRAP },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_ALLOW },
        ];

        let prg = EbpfProgram::from_cbpf::<seccomp_data>(&test_prg).expect("Error parsing program");

        with_prg_assert_result(
            &prg,
            seccomp_data { arch: AUDIT_ARCH_AARCH64, ..Default::default() },
            1,
            "Did not reject incorrect arch",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data { arch: AUDIT_ARCH_X86_64, nr: 41, ..Default::default() },
            SECCOMP_RET_ALLOW,
            "Did not pass simple RET_ALLOW",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data {
                arch: AUDIT_ARCH_X86_64,
                nr: 100,
                args: [0xFF00000000, 0, 0, 0, 0, 0],
                ..Default::default()
            },
            SECCOMP_RET_TRAP,
            "Did not treat load of first 32 bits correctly",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data {
                arch: AUDIT_ARCH_X86_64,
                nr: 100,
                args: [0x4, 0, 0, 0, 0, 0],
                ..Default::default()
            },
            SECCOMP_RET_TRAP,
            "Did not correctly reject load of second 32 bits",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data {
                arch: AUDIT_ARCH_X86_64,
                nr: 100,
                args: [0x0, 0, 0, 0, 0, 0],
                ..Default::default()
            },
            SECCOMP_RET_ALLOW,
            "Did not correctly accept load of second 32 bits",
        );
    }

    #[test]
    fn test_alu_insns() {
        {
            let test_prg = [
                // Load data.nr (the syscall number)
                sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 0 }, // = 1, 11
                // Do some math.
                sock_filter { code: BPF_ALU_ADD_K, jt: 0, jf: 0, k: 3 }, // = 4, 14
                sock_filter { code: BPF_ALU_SUB_K, jt: 0, jf: 0, k: 2 }, // = 2, 12
                sock_filter { code: BPF_MISC_TAX, jt: 0, jf: 0, k: 0 },  // 2, 12 -> X
                sock_filter { code: BPF_ALU_MUL_K, jt: 0, jf: 0, k: 8 }, // = 16, 96
                sock_filter { code: BPF_ALU_DIV_K, jt: 0, jf: 0, k: 2 }, // = 8, 48
                sock_filter { code: BPF_ALU_AND_K, jt: 0, jf: 0, k: 15 }, // = 8, 0
                sock_filter { code: BPF_ALU_OR_K, jt: 0, jf: 0, k: 16 }, // = 24, 16
                sock_filter { code: BPF_ALU_XOR_K, jt: 0, jf: 0, k: 7 }, // = 31, 23
                sock_filter { code: BPF_ALU_LSH_K, jt: 0, jf: 0, k: 2 }, // = 124, 92
                sock_filter { code: BPF_ALU_OR_X, jt: 0, jf: 0, k: 1 },  // = 127, 92
                sock_filter { code: BPF_ALU_RSH_K, jt: 0, jf: 0, k: 1 }, // = 63, 46
                sock_filter { code: BPF_RET_A, jt: 0, jf: 0, k: 0 },
            ];

            let prg =
                EbpfProgram::from_cbpf::<seccomp_data>(&test_prg).expect("Error parsing program");

            with_prg_assert_result(
                &prg,
                seccomp_data { nr: 1, ..Default::default() },
                63,
                "BPF math does not work",
            );

            with_prg_assert_result(
                &prg,
                seccomp_data { nr: 11, ..Default::default() },
                46,
                "BPF math does not work",
            );
        }

        {
            // Negative numbers simple check
            let test_prg = [
                // Load data.nr (the syscall number)
                sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 0 }, // = -1
                sock_filter { code: BPF_ALU_SUB_K, jt: 0, jf: 0, k: 2 }, // = -3
                sock_filter { code: BPF_RET_A, jt: 0, jf: 0, k: 0 },
            ];

            let prg =
                EbpfProgram::from_cbpf::<seccomp_data>(&test_prg).expect("Error parsing program");

            with_prg_assert_result(
                &prg,
                seccomp_data { nr: -1, ..Default::default() },
                u32::MAX - 2,
                "BPF math does not work",
            );
        }
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone, AsBytes, FromBytes, NoCell, FromZeros)]
    struct ProgramArgument {
        /// Pointer to an array of u64
        pub data: u64,
        /// End of the array.
        pub data_end: u64,
    }

    impl ProgramArgument {
        fn get_type() -> Type {
            Self::get_type_with_mappings(Default::default())
        }

        fn get_type_with_mappings(mappings: Vec<FieldMapping>) -> Type {
            let struct_size = std::mem::size_of::<ProgramArgument>() as u64;
            let array_id = new_bpf_type_identifier();
            Type::PtrToMemory {
                id: new_bpf_type_identifier(),
                offset: 0,
                buffer_size: struct_size,
                fields: vec![
                    FieldType {
                        offset: 0,
                        field_type: Box::new(Type::PtrToArray { id: array_id.clone(), offset: 0 }),
                    },
                    FieldType {
                        offset: 8,
                        field_type: Box::new(Type::PtrToEndArray { id: array_id }),
                    },
                ],
                mappings,
            }
        }
    }

    #[test]
    fn test_data_end() {
        let program = r#"
        mov %r0, 0
        ldxdw %r2, [%r1+8]
        ldxdw %r1, [%r1]
        # ensure data contains at least 8 bytes
        mov %r3, %r1
        add %r3, 0x8
        jgt %r3, %r2, +1
        # read 8 bytes from data
        ldxdw %r0, [%r1]
        exit
        "#;
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let v: u64 = 42;
        let v_ptr = (&v as *const u64) as u64;
        let mut data =
            ProgramArgument { data: v_ptr, data_end: v_ptr + std::mem::size_of::<u64>() as u64 };
        assert_eq!(program.run(&mut (), &mut data), v);
    }

    #[test]
    fn test_past_data_end() {
        let program = r#"
        mov %r0, 0
        ldxdw %r2, [%r1+8]
        ldxdw %r1, [%r1]
        # ensure data contains at least 4 bytes
        mov %r3, %r1
        add %r3, 0x4
        jgt %r3, %r2, +1
        # read 8 bytes from data
        ldxdw %r0, [%r1]
        exit
        "#;
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        builder.load(code, &mut NullVerifierLogger).expect_err("incorrect program");
    }

    #[test]
    fn test_mapping() {
        let program = r#"
        mov %r0, 0
        # Load data and data_end as 32 bits pointers
        ldxw %r2, [%r1+4]
        ldxw %r1, [%r1]
        # ensure data contains at least 8 bytes
        mov %r3, %r1
        add %r3, 0x8
        jgt %r3, %r2, +1
        # read 8 bytes from data
        ldxdw %r0, [%r1]
        exit
        "#;
        let code = parse_asm(program);

        let mut mappings = vec![];
        mappings.push(FieldMapping::new_size_mapping(0, 0));
        mappings.push(FieldMapping::new_size_mapping(4, 8));
        let argument = ProgramArgument::get_type_with_mappings(mappings);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[argument]);
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let v: u64 = 42;
        let v_ptr = (&v as *const u64) as u64;
        let mut data =
            ProgramArgument { data: v_ptr, data_end: v_ptr + std::mem::size_of::<u64>() as u64 };
        assert_eq!(program.run(&mut (), &mut data), v);
    }

    #[test]
    fn test_ptr_diff() {
        let program = r#"
          mov %r0, %r1
          add %r0, 0x2
          # Substract 2 ptr to memory
          sub %r0, %r1

          mov %r2, %r10
          add %r2, 0x3
          # Substract 2 ptr to stack
          sub %r2, %r10
          add %r0, %r2

          ldxdw %r2, [%r1+8]
          ldxdw %r1, [%r1]
          # Substract ptr to array and ptr to array end
          sub %r2, %r1
          add %r0, %r2

          mov %r2, %r1
          add %r2, 0x4
          # Substract 2 ptr to array
          sub %r2, %r1
          add %r0, %r2

          exit
        "#;
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let v: u64 = 42;
        let v_ptr = (&v as *const u64) as u64;
        let mut data =
            ProgramArgument { data: v_ptr, data_end: v_ptr + std::mem::size_of::<u64>() as u64 };
        assert_eq!(program.run(&mut (), &mut data), 17);
    }
}
