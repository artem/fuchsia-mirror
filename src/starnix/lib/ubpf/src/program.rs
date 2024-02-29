// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    converter::cbpf_to_ebpf,
    ubpf::{ubpf_create, ubpf_destroy, ubpf_exec, ubpf_load, ubpf_register, ubpf_vm},
    verifier::{
        verify, CallingContext, FunctionSignature, NullVerifierLogger, Type, VerifierLogger,
    },
    MapSchema, UbpfError,
    UbpfError::*,
};
use linux_uapi::{bpf_insn, c_void, sock_filter};

// This file contains wrapper logic to build programs and execute
// them in the ubpf VM.

/// An alias for the UbpfVm. This allows not to reference the actual implementation in external
/// code.
pub type EbpfProgram = UbpfVm;

/// An alias for the UbpfVmBuilder. This allows not to reference the actual implementation in
/// external code.
pub type EbpfProgramBuilder = UbpfVmBuilder;

#[derive(Debug)]
pub struct UbpfVmBuilder {
    vm: *mut ubpf_vm,
    calling_context: CallingContext,
}

pub struct BpfHelper {
    pub index: u32,
    pub name: &'static str,
    pub function_pointer: *mut std::os::raw::c_void,
    pub signature: FunctionSignature,
}

// SAFETY
//
// The helper pointer is a constant function pointer so is safe to use across thread.
unsafe impl Send for BpfHelper {}
unsafe impl Sync for BpfHelper {}

impl UbpfVmBuilder {
    pub fn new() -> Result<Self, UbpfError> {
        let vm = unsafe { ubpf_create() };
        if vm == std::ptr::null_mut() {
            return Err(VmInitialization);
        }
        Ok(UbpfVmBuilder { vm, calling_context: Default::default() })
    }

    pub fn register_map_reference(&mut self, pc: usize, schema: MapSchema) {
        self.calling_context.register_map_reference(pc, schema);
    }

    pub fn set_args(&mut self, args: &[Type]) {
        self.calling_context.set_args(args);
    }

    // This function signature will need more parameters eventually. The client needs to be able to
    // supply a real callback and it's type. The callback will be needed to actually call the
    // callback. The type will be needed for the verifier.
    pub fn register(&mut self, helper: &BpfHelper) -> Result<(), UbpfError> {
        let success = unsafe {
            ubpf_register(
                self.vm,
                helper.index,
                helper.name.as_ptr() as *const std::os::raw::c_char,
                helper.function_pointer,
            )
        };
        if success != 0 {
            return Err(VmRegisterError(
                format!(
                    "Unable to register callback {} ({}) with error {}",
                    helper.index, helper.name, success
                )
                .to_string(),
            ));
        }
        self.calling_context.register_function(helper.index, helper.signature.clone());
        Ok(())
    }

    pub fn load(
        self,
        mut code: Vec<bpf_insn>,
        logger: &mut dyn VerifierLogger,
    ) -> Result<UbpfVm, UbpfError> {
        verify(&code, self.calling_context, logger)?;
        unsafe {
            let mut errmsg = std::ptr::null_mut();
            let success = ubpf_load(
                self.vm,
                code.as_mut_ptr() as *mut c_void,
                (code.len() * std::mem::size_of::<bpf_insn>()) as u32,
                &mut errmsg,
            );

            if !errmsg.is_null() {
                let msg = std::ffi::CStr::from_ptr(errmsg)
                    .to_str()
                    .unwrap_or("Decoding error for error string")
                    .to_string();
                libc::free(errmsg as *mut c_void);
                return Err(ProgramLoadError(msg));
            }
            if success != 0 {
                return Err(VmLoadError(
                    format!("Unable to load program with error {}", success).to_string(),
                ));
            }
        }

        Ok(UbpfVm { opaque_vm: self.vm, _code: code })
    }
}

/// An abstraction over the ubpf VM.
#[derive(Debug)]
pub struct UbpfVm {
    // The bpf vm used to run the program.  ubpf enforces that there is one program per vm.
    // We make the assumption that ubpf_vms are immutable after loading the code into them.
    // This implies that this API does not support reloading code.
    opaque_vm: *mut ubpf_vm,

    _code: Vec<bpf_insn>,
}

unsafe impl Send for UbpfVm {}
unsafe impl Sync for UbpfVm {}

impl UbpfVm {
    /// Executes the current program on the provided data.  Warning: If
    /// this program was a cbpf program, and it uses BPF_MEM, the
    /// scratch memory must be provided by the caller to this
    /// function.  The translated CBPF program will use the last 16
    /// words of |data|.
    pub fn run<T>(&self, data: &mut T) -> Result<u64, i32> {
        let data_size: usize = std::mem::size_of::<T>();
        let mut bpf_return_value: u64 = 0;
        let status = unsafe {
            ubpf_exec(
                self.opaque_vm as *mut ubpf_vm,
                data as *mut T as *mut c_void,
                data_size,
                &mut bpf_return_value,
            )
        };

        if status != 0 {
            return Err(status);
        }

        Ok(bpf_return_value)
    }

    /// This method instantiates an UbpfVm given a cbpf original.
    pub fn from_cbpf<T>(bpf_code: &[sock_filter]) -> Result<Self, UbpfError> {
        let code = cbpf_to_ebpf(bpf_code)?;
        let buffer_size = std::mem::size_of::<T>() as u64;
        let mut builder = UbpfVmBuilder::new()?;
        builder.set_args(&[
            Type::default(),
            Type::PtrToMemory { id: 0, offset: 0, buffer_size },
            Type::from(buffer_size),
        ]);
        builder.load(code, &mut NullVerifierLogger)
    }
}

impl Drop for UbpfVm {
    fn drop(&mut self) {
        unsafe {
            let tbd = self.opaque_vm;
            self.opaque_vm = std::ptr::null_mut();

            ubpf_destroy(tbd as *mut ubpf_vm);
        }
    }
}

#[cfg(test)]
mod test {
    use crate::EbpfProgram;
    use linux_uapi::*;

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

    fn with_prg_assert_result(prg: &EbpfProgram, mut data: seccomp_data, result: u32, msg: &str) {
        let return_value = prg.run(&mut data).expect("Error executing the program");
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
}
