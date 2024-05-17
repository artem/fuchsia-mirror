// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(unused_variables)]

use crate::{
    visitor::{BpfVisitor, DataWidth, ProgramCounter, Register, Source},
    EbpfError, MapSchema, BPF_MAX_INSTS, BPF_SIZE_MASK, BPF_STACK_SIZE, GENERAL_REGISTER_COUNT,
    REGISTER_COUNT,
};
use byteorder::{BigEndian, ByteOrder, LittleEndian, NativeEndian};
use fuchsia_sync::Mutex;
use linux_uapi::bpf_insn;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    ops::Deref,
    sync::Arc,
};
use zerocopy::AsBytes;

/// A trait to receive the log from the verifier.
pub trait VerifierLogger {
    /// Log a line. The line is always a correct encoded ASCII string.
    fn log(&mut self, line: &[u8]);
}

/// A `VerifierLogger` that drops all its content.
pub struct NullVerifierLogger;

impl VerifierLogger for NullVerifierLogger {
    fn log(&mut self, line: &[u8]) {
        debug_assert!(line.is_ascii());
    }
}

/// An identifier for a memory buffer accessible by an ebpf program. The identifiers are built as a
/// chain of unique identifier so that a buffer can contain multiple pointers to the same type and
/// the verifier can distinguish between the different instances.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MemoryId {
    id: u64,
    parent: Option<Box<MemoryId>>,
}

impl From<u64> for MemoryId {
    fn from(id: u64) -> Self {
        Self { id, parent: None }
    }
}

impl MemoryId {
    /// Build a new id such that `other` is prepended to the chain of parent of `self`.
    fn prepended(&self, other: MemoryId) -> Self {
        match &self.parent {
            None => MemoryId { id: self.id, parent: Some(Box::new(other)) },
            Some(parent) => {
                MemoryId { id: self.id, parent: Some(Box::new(parent.prepended(other))) }
            }
        }
    }
}

/// The target type of a pointer type in a struct.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldType {
    /// The offset at which the pointer is loacted.
    pub offset: u64,
    /// The type of the pointed memory. The verifier only supports `PtrToArray` and `PtrToEndArray`
    /// for now.
    pub field_type: Box<Type>,
}

impl FieldType {
    /// Whether the pointer represented by this field intercept the memory situated at `offset` and
    /// of width `width`.
    fn intercept(&self, offset: u64, width: DataWidth) -> bool {
        std::cmp::max(self.offset, offset)
            < std::cmp::min(
                self.offset + DataWidth::U64.bytes() as u64,
                offset + width.bytes() as u64,
            )
    }
}

/// A mapping for a field in a struct where the original ebpf program knows a different offset and
/// data size than the one it receives from the kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldMapping {
    /// The offset of the field as known by the original ebpf program.
    pub source_offset: i16,
    /// The actual offset of the field in the data provided by the kernel.
    pub target_offset: i16,
    /// Whether the epbf program consider the data to be 32 bits, while the actual data is 64 bits.
    pub is_32_to_64: bool,
}

impl FieldMapping {
    /// Returns a new `FieldMapping` where the source and target fields have the same width.
    pub fn new_offset_mapping(source_offset: i16, target_offset: i16) -> Self {
        Self { source_offset, target_offset, is_32_to_64: false }
    }

    /// Returns a new `FieldMapping` where the source field is 32 bits while the target field is 64
    /// bits.
    pub fn new_size_mapping(source_offset: i16, target_offset: i16) -> Self {
        Self { source_offset, target_offset, is_32_to_64: true }
    }
}

/// The offset and width of a field in a struct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Field {
    offset: i16,
    width: DataWidth,
}

impl Field {
    fn new(offset: i16, width: DataWidth) -> Self {
        Self { offset, width }
    }

    fn offset_as_u64(&self) -> u64 {
        i64::from(self.offset) as u64
    }
}

/// Helper trait to find the actual field to use when accessing a field of a struct. Returns `None`
/// if the field is not mapped.
trait MappingVec {
    fn find_mapping(
        &self,
        context: &ComputationContext,
        field: Field,
    ) -> Result<Option<Field>, String>;
}

impl MappingVec for Vec<FieldMapping> {
    fn find_mapping(
        &self,
        context: &ComputationContext,
        field: Field,
    ) -> Result<Option<Field>, String> {
        if let Some(mapping) = self.iter().find(|m| m.source_offset == field.offset) {
            if mapping.is_32_to_64 {
                if field.width == DataWidth::U32 {
                    Ok(Some(Field::new(mapping.target_offset, DataWidth::U64)))
                } else {
                    Err(format!("incorrect memory access width at pc {}", context.pc))
                }
            } else {
                Ok(Some(Field::new(mapping.target_offset, field.width)))
            }
        } else {
            Ok(None)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MemoryParameterSize {
    /// The memory buffer have the given size.
    Value(u64),
    /// The memory buffer size is given by the parameter in the given index.
    Reference { index: u8 },
}

impl MemoryParameterSize {
    fn size(&self, context: &ComputationContext) -> Result<u64, String> {
        match self {
            Self::Value(size) => Ok(*size),
            Self::Reference { index } => {
                let size_type = context.reg(index + 1)?;
                match size_type {
                    Type::ScalarValue { value, unknown_mask: 0, .. } => Ok(value),
                    _ => Err(format!("cannot know buffer size at pc {}", context.pc)),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// A number.
    ScalarValue {
        /// The value. Its interpresentation depends on `unknown_mask` and `unwritten_mask`.
        value: u64,
        /// A bit mask of unknown bits. A bit in `value` is valid (and can be used by the verifier)
        /// if the equivalent mask in unknown_mask is 0.
        unknown_mask: u64,
        /// A bit mask of unwritten bits. A bit in `value` is written (and can be sent back to
        /// userspace) if the equivalent mask in unknown_mask is 0. `unknown_mask` must always be a
        /// subset of `unwritten_mask`.
        unwritten_mask: u64,
    },
    /// A pointer to a map object.
    ConstPtrToMap { id: u64, schema: MapSchema },
    /// A pointer into the stack.
    PtrToStack { offset: StackOffset },
    /// A pointer to the kernel memory. The full buffer is `buffer_size` bytes long. The pointer is
    /// situated at `offset` from the start of the buffer.
    PtrToMemory {
        id: MemoryId,
        offset: u64,
        buffer_size: u64,
        /// The list of fields in the buffer that are known pointer to known types.
        fields: Vec<FieldType>,
        /// The list of mappings in the buffer. The verifier must rewrite the actual ebpf to ensure
        /// the right offset and operand are use to access the mapped fields.
        mappings: Vec<FieldMapping>,
    },
    /// A pointer to the kernel memory. The full buffer size is determined by an instance of
    /// `PtrToEndArray` with the same `id`. The pointer is situadted at `offset` from the start of
    /// the buffer.
    PtrToArray { id: MemoryId, offset: u64 },
    /// A pointer to the kernel memory that represents the first non accessible byte of a
    /// `PtrToArray` with the same `id`.
    PtrToEndArray { id: MemoryId },
    /// A pointer that might be null and must be validated before being referenced.
    NullOr { id: MemoryId, inner: Box<Type> },
    /// A function parameter that must be a `ScalarValue` when called.
    ScalarValueParameter,
    /// A function parameter that must be a `ConstPtrToMap` when called.
    ConstPtrToMapParameter,
    /// A function parameter that must be a key of a map.
    MapKeyParameter {
        /// The index in the arguments list that contains a `ConstPtrToMap` for the map this key is
        /// associated with.
        map_ptr_index: u8,
    },
    /// A function parameter that must be a value of a map.
    MapValueParameter {
        /// The index in the arguments list that contains a `ConstPtrToMap` for the map this key is
        /// associated with.
        map_ptr_index: u8,
    },
    /// A function parameter that must be a pointer to memory.
    MemoryParameter {
        /// The index in the arguments list that contains a scalar value containing the size of the
        /// memory.
        size: MemoryParameterSize,
        /// Whether this memory is read by the function.
        input: bool,
        /// Whether this memory is written by the function.
        output: bool,
    },
    /// A function return value that is the same type as a parameter.
    AliasParameter {
        /// The index in the argument list of the parameter that has the type of this return value.
        parameter_index: u8,
    },
    /// A function return value that is either null, or the given type.
    NullOrParameter(Box<Type>),
    /// A function parameter that must be a pointer to memory with the given id.
    StructParameter { id: MemoryId },
}

/// Defines a partial ordering on `Type` instances, capturing the notion of how "broad"
/// a type is in terms of the set of potential values it represents.
///
/// The ordering is defined such that `t1 > t2` if a proof that an eBPF program terminates
/// in a state where a register or memory location has type `t1` is also a proof that
/// the program terminates in a state where that location has type `t2`.
///
/// In other words, a "broader" type represents a larger set of possible values, and
/// proving termination with a broader type implies termination with any narrower type.
///
/// Examples:
/// * `Type::ScalarValue { unknown_mask: 0, .. }` (a known scalar value) is less than
///   `Type::ScalarValue { unknown_mask: u64::MAX, .. }` (an unknown scalar value).
impl PartialOrd for Type {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        fn mask_is_larger(m1: u64, m2: u64) -> bool {
            m1 & m2 == m2 && m1 | m2 == m1
        }

        // If the values are equals, return the known result.
        if self == other {
            return Some(Ordering::Equal);
        }

        // If one value is not initialized, the types are ordered.
        if self == &NOT_INIT {
            return Some(Ordering::Greater);
        }
        if other == &NOT_INIT {
            return Some(Ordering::Less);
        }

        // Otherwise, only scalars are comparables.
        match (self, other) {
            (
                Self::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Self::ScalarValue {
                    value: value2,
                    unknown_mask: unknown_mask2,
                    unwritten_mask: unwritten_mask2,
                },
            ) => {
                if mask_is_larger(*unwritten_mask1, *unwritten_mask2)
                    && mask_is_larger(*unknown_mask1, *unknown_mask2)
                    && value1 & !unknown_mask1 == value2 & !unknown_mask1
                {
                    return Some(Ordering::Greater);
                }
                if mask_is_larger(*unwritten_mask2, *unwritten_mask1)
                    && mask_is_larger(*unknown_mask2, *unknown_mask1)
                    && value1 & !unknown_mask2 == value2 & !unknown_mask2
                {
                    return Some(Ordering::Less);
                }
                None
            }
            _ => None,
        }
    }
}

const NOT_INIT: Type = Type::ScalarValue {
    value: 0,
    unknown_mask: u64::max_value(),
    unwritten_mask: u64::max_value(),
};

impl From<u64> for Type {
    fn from(value: u64) -> Self {
        Self::ScalarValue { value, unknown_mask: 0, unwritten_mask: 0 }
    }
}

impl Default for Type {
    /// A new instance of `Type` where no bit has been written yet.
    fn default() -> Self {
        NOT_INIT.clone()
    }
}

impl Type {
    /// A new instance of `Type` where the data is usable by userspace, but the value is unknown
    /// for the verifier.
    pub const fn unknown_written_scalar_value() -> Self {
        Self::ScalarValue { value: 0, unknown_mask: u64::max_value(), unwritten_mask: 0 }
    }

    /// The mask associated with a data of size `width`.
    fn mask(width: DataWidth) -> u64 {
        if width == DataWidth::U64 {
            u64::max_value()
        } else {
            (1 << width.bits()) - 1
        }
    }

    /// Given the given conditional jump `jump_type` having been tasken, constraint the type of
    /// `type1` and `type2`.
    fn constraint(
        context: &mut ComputationContext,
        jump_type: JumpType,
        jump_width: JumpWidth,
        type1: Self,
        type2: Self,
    ) -> (Self, Self) {
        match (jump_width, jump_type, &type1, &type2) {
            (
                JumpWidth::W64,
                JumpType::Eq,
                Self::ScalarValue { value: value1, unknown_mask: known1, unwritten_mask: 0 },
                Self::ScalarValue { value: value2, unknown_mask: known2, unwritten_mask: 0 },
            ) => {
                let v = Self::ScalarValue {
                    value: value1 | value2,
                    unknown_mask: known1 & known2,
                    unwritten_mask: 0,
                };
                (v.clone(), v)
            }
            (
                JumpWidth::W32,
                JumpType::Eq,
                Self::ScalarValue { value: value1, unknown_mask: known1, unwritten_mask: 0 },
                Self::ScalarValue { value: value2, unknown_mask: known2, unwritten_mask: 0 },
            ) => {
                let v1 = Self::ScalarValue {
                    value: value1 | (value2 & (u32::MAX as u64)),
                    unknown_mask: known1 & (known2 | ((u32::MAX as u64) << 32)),
                    unwritten_mask: 0,
                };
                let v2 = Self::ScalarValue {
                    value: value2 | (value1 & (u32::MAX as u64)),
                    unknown_mask: known2 & (known1 | ((u32::MAX as u64) << 32)),
                    unwritten_mask: 0,
                };
                (v1, v2)
            }
            (
                JumpWidth::W64,
                JumpType::Eq,
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
                Self::NullOr { id, .. },
            )
            | (
                JumpWidth::W64,
                JumpType::Eq,
                Self::NullOr { id, .. },
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) => {
                context.set_null(id, true);
                let zero = Type::from(0);
                (zero.clone(), zero)
            }
            (
                JumpWidth::W64,
                jump_type,
                Self::NullOr { id, inner },
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) if jump_type.is_strict() => {
                context.set_null(id, false);
                (*inner.clone(), type2)
            }
            (
                JumpWidth::W64,
                jump_type,
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
                Self::NullOr { id, inner },
            ) if jump_type.is_strict() => {
                context.set_null(id, false);
                (type1, *inner.clone())
            }

            (
                JumpWidth::W64,
                JumpType::Eq,
                Type::PtrToArray { id: id1, offset },
                Type::PtrToEndArray { id: id2 },
            )
            | (
                JumpWidth::W64,
                JumpType::Le,
                Type::PtrToArray { id: id1, offset },
                Type::PtrToEndArray { id: id2 },
            )
            | (
                JumpWidth::W64,
                JumpType::Ge,
                Type::PtrToEndArray { id: id1 },
                Type::PtrToArray { id: id2, offset },
            ) if id1 == id2 => {
                context.update_array_bounds(id1.clone(), *offset);
                (type1, type2)
            }
            (
                JumpWidth::W64,
                JumpType::Lt,
                Type::PtrToArray { id: id1, offset },
                Type::PtrToEndArray { id: id2 },
            )
            | (
                JumpWidth::W64,
                JumpType::Gt,
                Type::PtrToEndArray { id: id1 },
                Type::PtrToArray { id: id2, offset },
            ) if id1 == id2 => {
                context.update_array_bounds(id1.clone(), *offset + 1);
                (type1, type2)
            }
            (JumpWidth::W64, JumpType::Eq, _, _) => (type1.clone(), type1),
            _ => (type1, type2),
        }
    }

    /// If this `Type` is an instance of NullOr with the given `null_id`, replace it wither either
    /// 0 or the subtype depending on `is_null`
    fn set_null(&mut self, null_id: &MemoryId, is_null: bool) {
        match self {
            Type::NullOr { id, inner } if id == null_id => {
                if is_null {
                    *self = Type::from(0);
                } else {
                    *self = *inner.clone();
                }
            }
            _ => {}
        }
    }

    /// Partially Compares two iterators of comparable items.
    ///
    /// This function iterates through both input iterators simultaneously and compares the corresponding elements.
    /// The comparison continues until:
    /// 1. Both iterators are exhausted and all elements were considered equal, in which case it returns `Some(Ordering::Equal)`.
    /// 2. All pairs of corresponding elements that are not equal have the same ordering (`Ordering::Less` or `Ordering::Greater`), in which case it returns `Some(Ordering)` reflecting that consistent ordering.
    /// 3. One iterator is exhausted before the other, or any comparison between elements yields `None`, or not all non-equal pairs have the same ordering, in which case it returns `None`.
    fn compare_list<'a>(
        mut l1: impl Iterator<Item = &'a Self>,
        mut l2: impl Iterator<Item = &'a Self>,
    ) -> Option<Ordering> {
        let mut result = Ordering::Equal;
        loop {
            match (l1.next(), l2.next()) {
                (None, None) => return Some(result),
                (_, None) | (None, _) => return None,
                (Some(v1), Some(v2)) => {
                    result = associate_orderings(result, v1.partial_cmp(v2)?)?;
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct FunctionSignature {
    pub args: Vec<Type>,
    pub return_value: Type,
    pub invalidate_array_bounds: bool,
}

#[derive(Debug, Default)]
pub struct CallingContext {
    /// For all pc that represents the load of a map address, keep track of the schema of the
    /// associated map.
    map_references: HashMap<ProgramCounter, MapSchema>,
    /// The registered external functions.
    functions: HashMap<u32, FunctionSignature>,
    /// The args of the program.
    args: Vec<Type>,
}

impl CallingContext {
    pub fn register_map_reference(&mut self, pc: ProgramCounter, schema: MapSchema) {
        self.map_references.insert(pc, schema);
    }
    pub fn register_function(&mut self, index: u32, signature: FunctionSignature) {
        self.functions.insert(index, signature);
    }
    pub fn set_args(&mut self, args: &[Type]) {
        assert!(args.len() <= 5);
        self.args = args.to_vec();
    }
}

/// Verify the given code depending on the type of the parameters and the registered external
/// functions. This method will rewrite the code to ensure mapped fields are correctly handled.
/// Returns the actual code to run.
pub fn verify(
    mut code: Vec<bpf_insn>,
    calling_context: CallingContext,
    logger: &mut dyn VerifierLogger,
) -> Result<Vec<bpf_insn>, EbpfError> {
    if code.len() > BPF_MAX_INSTS {
        return error_and_log(logger, "ebpf program too long");
    }

    let mut context = ComputationContext::default();
    for (i, t) in calling_context.args.iter().enumerate() {
        // The parameter registers are r1 to r5.
        context.set_reg((i + 1) as u8, t.clone()).map_err(EbpfError::ProgramLoadError)?;
    }
    let states = vec![context];
    let mut verification_context = VerificationContext {
        calling_context,
        logger,
        states,
        code: &code,
        counter: 0,
        iteration: 0,
        terminating_contexts: Default::default(),
        transformations: Default::default(),
    };
    while let Some(mut context) = verification_context.states.pop() {
        if let Some(terminating_contexts) =
            verification_context.terminating_contexts.get(&context.pc)
        {
            // Check whether there exist a context that terminate and prove that this context does
            // also terminate.
            if let Some(ending_context) =
                terminating_contexts.iter().find(|c| c.computation_context >= context)
            {
                // One such context has been found, this proves the current context terminates.
                // If the context has a parent, register the data dependencies and try to terminate
                // it.
                if let Some(parent) = context.parent.take() {
                    parent.dependencies.lock().push(ending_context.dependencies.clone());
                    if let Some(parent) = Arc::into_inner(parent) {
                        parent
                            .terminate(&mut verification_context)
                            .map_err(EbpfError::ProgramLoadError)?;
                    }
                }
                continue;
            }
        }
        if verification_context.iteration > 10 * BPF_MAX_INSTS {
            return error_and_log(verification_context.logger, "bpf byte code does not terminate");
        }
        if context.pc >= verification_context.code.len() {
            return error_and_log(verification_context.logger, "pc out of bounds");
        }
        let visit_result = context.visit(&mut verification_context, &code[context.pc..]);
        match visit_result {
            Err(message) => {
                return error_and_log(verification_context.logger, message);
            }
            _ => {}
        }
        verification_context.iteration += 1;
    }
    // Once the code is verified, applied the transformations.
    for transformation in verification_context.transformations.into_iter() {
        if let (pc, Some(field)) = transformation {
            let instruction = &mut code[pc];
            instruction.off = field.offset;
            instruction.code &= !BPF_SIZE_MASK;
            instruction.code |= field.width.instruction_bits();
        }
    }
    Ok(code)
}

struct VerificationContext<'a> {
    /// The type information for the program arguments and the registered functions.
    calling_context: CallingContext,
    /// The logger to use.
    logger: &'a mut dyn VerifierLogger,
    /// The `ComputationContext` yet to be validated.
    states: Vec<ComputationContext>,
    /// The program being analyzed.
    code: &'a [bpf_insn],
    /// A counter used to generated unique ids for memory buffers and maps.
    counter: u64,
    /// The current iteration of the verifier. Used to ensure termination by limiting the number of
    /// iteration before bailing out.
    iteration: usize,
    /// Keep track of the context that terminates at a given pc. The list of context will all be
    /// incomparables as each time a bigger context is computed, the smaller ones are removed from
    /// the list.
    terminating_contexts: BTreeMap<ProgramCounter, Vec<TerminatingContext>>,
    /// The current list of transformation to apply. This is also used to ensure that a given
    /// instruction that acts on a mapped field always requires the same transformation. If this is
    /// not the case, the verifier will reject the program.
    transformations: HashMap<ProgramCounter, Option<Field>>,
}

impl<'a> VerificationContext<'a> {
    fn next_id(&mut self) -> u64 {
        let id = self.counter;
        self.counter += 1;
        id
    }

    /// Register the given transformation (represented as the actual `Field` to access) for the
    /// given `pc`. This method will fail if an incompatible transformation is already registered
    /// at the given `pc`. In particular, instruction that requires no transformation are also
    /// registered so that they conflict if the same instruction requires a transformation in
    /// another context.
    fn register_transformation(
        &mut self,
        pc: ProgramCounter,
        transformation: Option<Field>,
    ) -> Result<(), String> {
        match self.transformations.entry(pc) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(transformation);
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                if *entry.get() != transformation {
                    return Err(format!("Unable to consistently apply mapping at pc: {}", pc));
                }
            }
        }
        Ok(())
    }
}

/// An offset inside the stack. The offset is from the end of the stack.
/// downward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StackOffset(u64);

impl Default for StackOffset {
    fn default() -> Self {
        Self(BPF_STACK_SIZE as u64)
    }
}

impl StackOffset {
    /// Whether the current offset is valid.
    fn is_valid(&self) -> bool {
        self.0 <= (BPF_STACK_SIZE as u64)
    }

    /// The value of the register.
    fn reg(&self) -> u64 {
        self.0
    }

    /// The offset into the equivalent byte array.
    fn offset(&self) -> usize {
        self.0 as usize
    }

    /// The index into the stack array this offset points to.
    fn array_index(&self) -> usize {
        self.offset() / std::mem::size_of::<u64>()
    }

    /// The offset inside the aligned u64 in the stack.
    fn sub_index(&self) -> usize {
        self.offset() % std::mem::size_of::<u64>()
    }
}

impl std::ops::Add<u64> for StackOffset {
    type Output = Self;

    fn add(self, rhs: u64) -> Self {
        Self(self.reg().overflowing_add(rhs).0)
    }
}

impl std::ops::AddAssign<u64> for StackOffset {
    fn add_assign(&mut self, rhs: u64) {
        self.0 = self.reg().overflowing_add(rhs).0
    }
}

impl std::ops::Sub<u64> for StackOffset {
    type Output = Self;

    fn sub(self, rhs: u64) -> Self {
        Self(self.reg().overflowing_sub(rhs).0)
    }
}

impl std::ops::SubAssign<u64> for StackOffset {
    fn sub_assign(&mut self, rhs: u64) {
        self.0 = self.reg().overflowing_sub(rhs).0
    }
}

const STACK_MAX_INDEX: usize = BPF_STACK_SIZE / std::mem::size_of::<u64>();

/// The state of the stack
#[derive(Clone, Debug, Default, PartialEq)]
struct Stack {
    data: HashMap<usize, Type>,
}

impl Stack {
    /// Replace all instances of the NullOr type with the given `null_id` to either 0 or the
    /// subtype depending on `is_null`
    fn set_null(&mut self, null_id: &MemoryId, is_null: bool) {
        for (_, t) in self.data.iter_mut() {
            t.set_null(null_id, is_null);
        }
    }

    fn get(&self, index: usize) -> &Type {
        self.data.get(&index).unwrap_or(&NOT_INIT)
    }

    fn set(&mut self, index: usize, t: Type) {
        if t == NOT_INIT {
            self.data.remove(&index);
        } else {
            self.data.insert(index, t);
        }
    }

    fn extract_sub_value(value: u64, offset: usize, byte_count: usize) -> u64 {
        NativeEndian::read_uint(&value.as_bytes()[offset..], byte_count)
    }

    fn insert_sub_value(mut original: u64, value: u64, width: DataWidth, offset: usize) -> u64 {
        let byte_count = width.bytes();
        let original_buf = original.as_bytes_mut();
        let value_buf = value.as_bytes();
        for i in 0..byte_count {
            original_buf[i + offset] = value_buf[i];
        }
        original
    }

    fn write_data_ptr(
        &mut self,
        pc: ProgramCounter,
        mut offset: StackOffset,
        bytes: u64,
    ) -> Result<(), String> {
        for i in 0..bytes {
            self.store(pc, offset, Type::unknown_written_scalar_value(), DataWidth::U8)?;
            offset += 1;
        }
        Ok(())
    }

    fn can_read_data_ptr(&self, mut offset: StackOffset, bytes: u64) -> bool {
        let can_read = |t: &Type, start_offset: usize, mut end_offset: usize| -> bool {
            match t {
                Type::ScalarValue { unwritten_mask, .. } => {
                    if start_offset == 0 && end_offset == 0 {
                        return *unwritten_mask == 0;
                    }
                    if end_offset == 0 {
                        end_offset = std::mem::size_of::<u64>();
                    }
                    debug_assert!(end_offset > start_offset);
                    Self::extract_sub_value(
                        *unwritten_mask,
                        start_offset,
                        end_offset - start_offset,
                    ) == 0
                }
                _ => false,
            }
        };
        if bytes == 0 {
            return true;
        }
        let mut end_offset = offset + bytes;
        if (end_offset - 1).array_index() as usize >= STACK_MAX_INDEX {
            return false;
        }
        // Handle the case where all the data is contained in a single u64.
        if offset.array_index() == (end_offset - 1).array_index() {
            return can_read(
                &self.get(offset.array_index()),
                offset.sub_index(),
                end_offset.sub_index(),
            );
        }

        // Handle the first element, that might be partial
        if offset.sub_index() != 0 {
            if !can_read(
                &self.get(offset.array_index()),
                offset.sub_index(),
                std::mem::size_of::<u64>(),
            ) {
                return false;
            }
            offset += (std::mem::size_of::<u64>() - offset.sub_index()) as u64;
        }

        // Handle the last element, that might be partial
        if end_offset.sub_index() != 0 {
            if !can_read(&self.get(end_offset.array_index()), 0, end_offset.sub_index()) {
                return false;
            }
            end_offset -= end_offset.sub_index() as u64;
        }

        // Handle the any full type between beginning and end.
        for i in offset.array_index()..end_offset.array_index() {
            if !can_read(&self.get(i), 0, std::mem::size_of::<u64>()) {
                return false;
            }
        }

        true
    }

    fn store(
        &mut self,
        pc: ProgramCounter,
        offset: StackOffset,
        value: Type,
        width: DataWidth,
    ) -> Result<(), String> {
        if offset.array_index() >= STACK_MAX_INDEX {
            return Err(format!("out of bound store at pc {}", pc));
        }
        if offset.sub_index() % width.bytes() != 0 {
            return Err(format!("misaligned access at pc {}", pc));
        }

        let index = offset.array_index();
        if width == DataWidth::U64 {
            self.set(index, value);
        } else {
            match value {
                Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                    let (old_value, old_unknown_mask, old_unwritten_mask) = match self.get(index) {
                        Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                            (*value, *unknown_mask, *unwritten_mask)
                        }
                        _ => {
                            // The value in the stack is not a scalar. Let consider it an scalar
                            // value with no written bits.
                            let Type::ScalarValue { value, unknown_mask, unwritten_mask } =
                                NOT_INIT
                            else {
                                unreachable!();
                            };
                            (value, unknown_mask, unwritten_mask)
                        }
                    };
                    let sub_index = offset.sub_index();
                    let value = Self::insert_sub_value(old_value, value, width, sub_index);
                    let unknown_mask =
                        Self::insert_sub_value(old_unknown_mask, unknown_mask, width, sub_index);
                    let unwritten_mask = Self::insert_sub_value(
                        old_unwritten_mask,
                        unwritten_mask,
                        width,
                        sub_index,
                    );
                    self.set(index, Type::ScalarValue { value, unknown_mask, unwritten_mask });
                }
                _ => {
                    return Err(format!(
                        "cannot store part of a non scalar value on the stack at pc {}",
                        pc
                    ));
                }
            }
        }
        Ok(())
    }

    fn load(
        &self,
        context: &ComputationContext,
        offset: StackOffset,
        width: DataWidth,
    ) -> Result<Type, String> {
        if offset.array_index() >= STACK_MAX_INDEX {
            return Err(format!("out of bound load at pc {}", context.pc));
        }
        if offset.sub_index() % width.bytes() != 0 {
            return Err(format!("misaligned access at pc {}", context.pc));
        }

        let index = offset.array_index();
        let loaded_type = self.get(index).clone();
        if width == DataWidth::U64 {
            Ok(loaded_type)
        } else {
            match loaded_type {
                Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                    let sub_index = offset.sub_index();
                    let value = Self::extract_sub_value(value, sub_index, width.bytes());
                    let unknown_mask =
                        Self::extract_sub_value(unknown_mask, sub_index, width.bytes());
                    let unwritten_mask =
                        Self::extract_sub_value(unwritten_mask, sub_index, width.bytes());
                    Ok(Type::ScalarValue { value, unknown_mask, unwritten_mask })
                }
                _ => Err(format!("incorrect load of {} bytes at pc {}", width.bytes(), context.pc)),
            }
        }
    }
}

/// Two types are ordered with `t1` > `t2` if a proof that a program in a state `t1` finish is also
/// a proof that a program in a state `t2` finish.
impl PartialOrd for Stack {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let mut result = Ordering::Equal;
        let mut data_iter1 = self.data.iter().peekable();
        let mut data_iter2 = other.data.iter().peekable();
        loop {
            let k1 = data_iter1.peek().map(|(k, _)| *k);
            let k2 = data_iter2.peek().map(|(k, _)| *k);
            let k = match (k1, k2) {
                (None, None) => return Some(result),
                (Some(k), None) => {
                    data_iter1.next();
                    *k
                }
                (None, Some(k)) => {
                    data_iter2.next();
                    *k
                }
                (Some(k1), Some(k2)) => {
                    if k1 <= k2 {
                        data_iter1.next();
                    }
                    if k2 <= k1 {
                        data_iter2.next();
                    }
                    *std::cmp::min(k1, k2)
                }
            };
            result = associate_orderings(result, self.get(k).partial_cmp(other.get(k))?)?;
        }
    }
}

macro_rules! bpf_log {
    ($context:ident, $verification_context:ident, $($msg:tt)*) => {
        let prefix = format!("{}: ({:02x})", $context.pc, $verification_context.code[$context.pc].code);
        let suffix = format!($($msg)*);
        $verification_context.logger.log(format!("{prefix} {suffix}").as_bytes());
    }
}

/// The state of the computation as known by the verifier at a given point in time.
#[derive(Debug, Default)]
struct ComputationContext {
    /// The program counter.
    pc: ProgramCounter,
    /// Register 0 to 9.
    registers: [Type; GENERAL_REGISTER_COUNT as usize],
    /// The state of the stack.
    stack: Stack,
    /// The dynamically known bounds of buffers indexed by their ids.
    array_bounds: BTreeMap<MemoryId, u64>,
    /// The previous context in the computation.
    parent: Option<Arc<ComputationContext>>,
    /// The data dependencies of this context. This is used to broaden a known ending context to
    /// help cutting computation branches.
    dependencies: Mutex<Vec<DataDependencies>>,
}

impl Clone for ComputationContext {
    fn clone(&self) -> Self {
        Self {
            pc: self.pc,
            registers: self.registers.clone(),
            stack: self.stack.clone(),
            array_bounds: self.array_bounds.clone(),
            parent: self.parent.clone(),
            // dependencies are erased as they must always be used on the same instance of the
            // context.
            dependencies: Default::default(),
        }
    }
}

/// parent and dependencies are ignored for the comparison, as they do not matter for termination.
impl PartialEq for ComputationContext {
    fn eq(&self, other: &Self) -> bool {
        self.pc == other.pc
            && self.registers == other.registers
            && self.stack == other.stack
            && self.array_bounds == other.array_bounds
    }
}

impl ComputationContext {
    /// Replace all instances of the NullOr type with the given `null_id` to either 0 or the
    /// subtype depending on `is_null`
    fn set_null(&mut self, null_id: &MemoryId, is_null: bool) {
        for i in 0..self.registers.len() {
            self.registers[i].set_null(null_id, is_null);
        }
        self.stack.set_null(null_id, is_null);
    }

    fn reg(&self, index: Register) -> Result<Type, String> {
        if index >= REGISTER_COUNT {
            return Err(format!("R{index} is invalid at pc {}", self.pc));
        }
        if index < GENERAL_REGISTER_COUNT {
            Ok(self.registers[index as usize].clone())
        } else {
            Ok(Type::PtrToStack { offset: StackOffset::default() })
        }
    }

    fn set_reg(&mut self, index: Register, reg_type: Type) -> Result<(), String> {
        if index >= GENERAL_REGISTER_COUNT {
            return Err(format!("R{index} is invalid at pc {}", self.pc));
        }
        self.registers[index as usize] = reg_type;
        Ok(())
    }

    fn update_array_bounds(&mut self, id: MemoryId, new_bound: u64) {
        self.array_bounds
            .entry(id)
            .and_modify(|v| *v = std::cmp::max(*v, new_bound))
            .or_insert(new_bound);
    }

    fn get_map_schema(&self, argument: u8) -> Result<MapSchema, String> {
        match self.reg(argument + 1)? {
            Type::ConstPtrToMap { schema, .. } => Ok(schema),
            _ => Err(format!("No map found at argument {argument} at pc {}", self.pc)),
        }
    }

    fn next(&self) -> Result<Self, String> {
        let parent = Some(Arc::new(self.clone()));
        self.jump_with_offset(0, parent)
    }

    /// Returns a new `ComputationContext` where the pc has jump by `offset + 1`. In particular,
    /// the next instruction is reached with `jump_with_offset(0)`.
    fn jump_with_offset(&self, offset: i16, parent: Option<Arc<Self>>) -> Result<Self, String> {
        let pc = self
            .pc
            .checked_add_signed((offset + 1).into())
            .ok_or_else(|| format!("jump outside of program at pc {}", self.pc))?;
        let result = Self {
            pc,
            registers: self.registers.clone(),
            stack: self.stack.clone(),
            array_bounds: self.array_bounds.clone(),
            parent,
            dependencies: Default::default(),
        };
        Ok(result)
    }

    fn check_field_access(
        &self,
        dst_offset: u64,
        dst_buffer_size: u64,
        field: Field,
    ) -> Result<(), String> {
        self.check_memory_access(
            dst_offset,
            dst_buffer_size,
            field.offset_as_u64(),
            field.width.bytes(),
        )
    }

    fn check_memory_access(
        &self,
        dst_offset: u64,
        dst_buffer_size: u64,
        instruction_offset: u64,
        width: usize,
    ) -> Result<(), String> {
        let final_offset = dst_offset.overflowing_add(instruction_offset).0;
        if final_offset
            .checked_add(width as u64)
            .ok_or_else(|| format!("out of bound access at pc {}", self.pc))?
            > dst_buffer_size
        {
            return Err(format!("out of bound access at pc {}", self.pc));
        }
        Ok(())
    }

    /// If `field` is mapped in `addr`, returns the actual field to use, if not returns None.
    fn apply_mapping(&self, addr: &Type, field: Field) -> Result<Option<Field>, String> {
        if let Type::PtrToMemory { offset: 0, mappings, .. } = addr {
            if let Some(field) = mappings.find_mapping(self, field)? {
                return Ok(Some(field));
            }
        }
        Ok(None)
    }

    /// If `field` is mapped in `addr`, returns the actual field to use, if not returns `field`.
    /// This method will also register any required transformation if needed, or register that none
    /// is needed.
    fn apply_and_register_mapping(
        &mut self,
        context: &mut VerificationContext<'_>,
        addr: &Type,
        field: Field,
    ) -> Result<Field, String> {
        let mapped_field = self.apply_mapping(addr, field)?;
        context.register_transformation(self.pc, mapped_field)?;
        Ok(mapped_field.unwrap_or(field))
    }

    fn store_memory(&mut self, addr: &Type, field: Field, value: Type) -> Result<(), String> {
        match *addr {
            Type::PtrToStack { offset } => {
                self.stack.store(self.pc, offset + field.offset_as_u64(), value, field.width)?;
            }
            Type::PtrToMemory { offset, buffer_size, ref fields, .. } => {
                self.check_field_access(offset, buffer_size, field)?;
                // Do not allow writing on or over a pointer.
                if fields.iter().any(|f| {
                    f.intercept(offset.overflowing_add(field.offset_as_u64()).0, field.width)
                }) {
                    return Err(format!("incorrect store at pc {}", self.pc));
                }
                match value {
                    Type::ScalarValue { unwritten_mask: 0, .. } => {}
                    // Private data should not be leaked.
                    _ => return Err(format!("incorrect store at pc {}", self.pc)),
                }
            }
            Type::PtrToArray { ref id, offset } => {
                self.check_field_access(offset, *self.array_bounds.get(&id).unwrap_or(&0), field)?;
                match value {
                    Type::ScalarValue { unwritten_mask: 0, .. } => {}
                    // Private data should not be leaked.
                    _ => return Err(format!("incorrect store at pc {}", self.pc)),
                }
            }
            _ => return Err(format!("incorrect store at pc {}", self.pc)),
        }
        Ok(())
    }

    fn load_memory(&self, addr: Type, field: Field) -> Result<Type, String> {
        Ok(match addr {
            Type::PtrToStack { offset } => {
                let stack_offset = offset + field.offset_as_u64();
                self.stack.load(self, stack_offset, field.width)?
            }
            Type::PtrToMemory { id, offset, buffer_size, fields, .. } => {
                self.check_field_access(offset, buffer_size, field)?;
                let memory_offset = offset.overflowing_add(field.offset_as_u64()).0;
                // If the read is for a full pointer and the offset correspond to a pointer, use
                // the `field_type` to specify the returned type.
                if field.width == DataWidth::U64 {
                    if let Some(field) = fields.iter().find(|f| f.offset == memory_offset) {
                        match field.field_type.deref() {
                            Type::PtrToArray { id: array_id, .. } => {
                                return Ok(Type::PtrToArray {
                                    id: array_id.prepended(id),
                                    offset: 0,
                                });
                            }
                            Type::PtrToEndArray { id: array_id } => {
                                return Ok(Type::PtrToEndArray { id: array_id.prepended(id) });
                            }
                            _ => panic!("Unexpected field_type: {field:?}"),
                        }
                    }
                }
                // Otherwise, reading on or over a pointer returns an illegal value.
                if fields.iter().any(|f| f.intercept(memory_offset, field.width)) {
                    Type::default()
                } else {
                    // Finally, return an unknown valid value.
                    Type::unknown_written_scalar_value()
                }
            }
            Type::PtrToArray { id, offset } => {
                self.check_field_access(offset, *self.array_bounds.get(&id).unwrap_or(&0), field)?;
                Type::unknown_written_scalar_value()
            }
            _ => return Err(format!("incorrect load at pc {}", self.pc)),
        })
    }

    fn resolve_return_value(
        &self,
        verification_context: &mut VerificationContext<'_>,
        return_value: &Type,
    ) -> Result<Type, String> {
        match return_value {
            Type::AliasParameter { parameter_index } => self.reg(parameter_index + 1),
            Type::NullOrParameter(t) => {
                let id = verification_context.next_id();
                Ok(Type::NullOr {
                    id: id.into(),
                    inner: Box::new(self.resolve_return_value(verification_context, t)?),
                })
            }
            Type::MapValueParameter { map_ptr_index } => {
                let schema = self.get_map_schema(*map_ptr_index)?;
                let id = verification_context.next_id();
                Ok(Type::PtrToMemory {
                    id: id.into(),
                    offset: 0,
                    buffer_size: schema.value_size as u64,
                    fields: Default::default(),
                    mappings: Default::default(),
                })
            }
            Type::MemoryParameter { size, .. } => {
                let buffer_size = size.size(self)?;
                let id = verification_context.next_id();
                Ok(Type::PtrToMemory {
                    id: id.into(),
                    offset: 0,
                    buffer_size,
                    fields: Default::default(),
                    mappings: Default::default(),
                })
            }
            t => Ok(t.clone()),
        }
    }

    fn compute_source(&self, src: Source) -> Result<Type, String> {
        match src {
            Source::Reg(reg) => self.reg(reg),
            Source::Value(v) => Ok(v.into()),
        }
    }

    fn apply_computation(
        op1: Type,
        op2: Type,
        alu_type: AluType,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<Type, String> {
        let result: Type = match (alu_type, op1, op2) {
            (
                _,
                Type::ScalarValue { value: value1, unknown_mask: 0, .. },
                Type::ScalarValue { value: value2, unknown_mask: 0, .. },
            ) => op(value1, value2).into(),
            (
                AluType::Bitwise,
                Type::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Type::ScalarValue {
                    value: value2,
                    unknown_mask: unknown_mask2,
                    unwritten_mask: unwritten_mask2,
                },
            ) => {
                let unknown_mask = unknown_mask1 | unknown_mask2;
                let unwritten_mask = unwritten_mask1 | unwritten_mask2;
                let value = op(value1, value2) & !unknown_mask;
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            (
                AluType::Shift,
                Type::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Type::ScalarValue { value: value2, unknown_mask: 0, .. },
            ) => {
                let value = op(value1, value2);
                let unknown_mask = op(unknown_mask1, value2);
                let unwritten_mask = op(unwritten_mask1, value2);
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            (
                AluType::Arsh,
                Type::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Type::ScalarValue { value: value2, unknown_mask: 0, .. },
            ) => {
                let unknown_mask = unknown_mask1.overflowing_shr(value2 as u32).0;
                let unwritten_mask = unwritten_mask1.overflowing_shr(value2 as u32).0;
                let value = op(value1, value2) & !unknown_mask;
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            (
                alu_type,
                Type::PtrToStack { offset: x },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) if alu_type.is_ptr_compatible() => {
                Type::PtrToStack { offset: run_on_stack_offset(x, |x| op(x, y)) }
            }
            (
                alu_type,
                Type::PtrToMemory { id, offset: x, buffer_size, fields, mappings },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) if alu_type.is_ptr_compatible() => {
                let offset = op(x, y);
                Type::PtrToMemory {
                    id: id.clone(),
                    offset,
                    buffer_size: buffer_size,
                    fields: fields.clone(),
                    mappings: mappings.clone(),
                }
            }
            (
                alu_type,
                Type::PtrToArray { id, offset: x },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) if alu_type.is_ptr_compatible() => {
                let offset = op(x, y);
                Type::PtrToArray { id: id.clone(), offset }
            }
            (
                AluType::Sub,
                Type::PtrToMemory { id: id1, offset: x1, .. },
                Type::PtrToMemory { id: id2, offset: x2, .. },
            )
            | (
                AluType::Sub,
                Type::PtrToArray { id: id1, offset: x1 },
                Type::PtrToArray { id: id2, offset: x2 },
            ) if id1 == id2 => Type::from(op(x1, x2)),
            (AluType::Sub, Type::PtrToStack { offset: x1 }, Type::PtrToStack { offset: x2 }) => {
                Type::from(op(x1.reg(), x2.reg()))
            }
            (
                AluType::Sub,
                Type::PtrToArray { id: id1, .. },
                Type::PtrToEndArray { id: id2, .. },
            )
            | (
                AluType::Sub,
                Type::PtrToEndArray { id: id1, .. },
                Type::PtrToArray { id: id2, .. },
            ) if id1 == id2 => Type::unknown_written_scalar_value(),
            (
                _,
                Type::ScalarValue { unwritten_mask: 0, .. },
                Type::ScalarValue { unwritten_mask: 0, .. },
            ) => Type::unknown_written_scalar_value(),
            _ => Type::default(),
        };
        Ok(result)
    }

    fn alu(
        &mut self,
        op_name: Option<&str>,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        src: Source,
        alu_type: AluType,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<(), String> {
        if let Some(op_name) = op_name {
            bpf_log!(
                self,
                verification_context,
                "{op_name} {}, {}",
                display_register(dst),
                display_source(src)
            );
        }
        let op1 = self.reg(dst)?;
        let op2 = self.compute_source(src)?;
        let result = Self::apply_computation(op1.clone(), op2, alu_type, op)?;
        let mut next = self.next()?;
        next.set_reg(dst, result)?;
        verification_context.states.push(next);
        Ok(())
    }

    fn log_atomic_operation(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) {
        bpf_log!(
            self,
            verification_context,
            "lock {}{} [{}{}], {}",
            if fetch { "fetch " } else { "" },
            op_name,
            display_register(dst),
            print_offset(offset),
            display_register(src),
        );
    }

    fn raw_atomic_operation(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        width: DataWidth,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
        op: impl FnOnce(Type, Type) -> Result<Type, String>,
    ) -> Result<(), String> {
        self.log_atomic_operation(op_name, verification_context, fetch, dst, offset, src);
        let addr = self.reg(dst)?.clone();
        let field = self.apply_and_register_mapping(
            verification_context,
            &addr,
            Field::new(offset, width),
        )?;
        let value = self.reg(src)?;
        let loaded_type = self.load_memory(addr.clone(), field)?;
        let result = op(loaded_type.clone(), value.clone())?;
        let mut next = self.next()?;
        next.store_memory(&addr, field, result)?;
        if fetch {
            next.set_reg(src, loaded_type)?;
        }
        verification_context.states.push(next);
        Ok(())
    }

    fn atomic_operation(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        width: DataWidth,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
        alu_type: AluType,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<(), String> {
        self.raw_atomic_operation(
            op_name,
            verification_context,
            width,
            fetch,
            dst,
            offset,
            src,
            |v1: Type, v2: Type| Self::apply_computation(v1, v2, alu_type, op),
        )
    }

    fn raw_atomic_cmpxchg(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        offset: i16,
        src: Register,
        jump_width: JumpWidth,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<(), String> {
        self.log_atomic_operation(op_name, verification_context, true, dst, offset, src);
        let width = match jump_width {
            JumpWidth::W32 => DataWidth::U32,
            JumpWidth::W64 => DataWidth::U64,
        };
        let addr = self.reg(dst)?.clone();
        let field = self.apply_and_register_mapping(
            verification_context,
            &addr,
            Field::new(offset, width),
        )?;
        let dst = self.load_memory(addr.clone(), field)?;
        let value = self.reg(src)?;
        let r0 = self.reg(0)?;
        let branch = self.compute_branch(jump_width, &dst, &r0, op)?;
        // r0 = dst
        if branch.unwrap_or(true) {
            let mut next = self.next()?;
            let (dst, r0) =
                Type::constraint(&mut next, JumpType::Eq, jump_width, dst.clone(), r0.clone());
            next.set_reg(0, dst)?;
            next.store_memory(&addr, field, value.clone())?;
            verification_context.states.push(next);
        }
        // r0 != dst
        if !branch.unwrap_or(false) {
            let mut next = self.next()?;
            let (dst, r0) =
                Type::constraint(&mut next, JumpType::Ne, jump_width, dst.clone(), r0.clone());
            next.set_reg(0, dst.clone())?;
            next.store_memory(&addr, field, dst)?;
            verification_context.states.push(next);
        }

        Ok(())
    }
    fn endianness<BO: ByteOrder>(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        bpf_log!(self, verification_context, "{op_name}{} {}", width.bits(), display_register(dst),);
        let bit_op = |value: u64| match width {
            DataWidth::U16 => BO::read_u16((value as u16).as_bytes()) as u64,
            DataWidth::U32 => BO::read_u32((value as u32).as_bytes()) as u64,
            DataWidth::U64 => BO::read_u64(value.as_bytes()),
            _ => {
                panic!("Unexpected bit width for endianness operation");
            }
        };
        let value = self.reg(dst)?;
        let new_value = match value {
            Type::ScalarValue { value, unknown_mask, unwritten_mask } => Type::ScalarValue {
                value: bit_op(value),
                unknown_mask: bit_op(unknown_mask),
                unwritten_mask: bit_op(unwritten_mask),
            },
            _ => Type::default(),
        };
        let mut next = self.next()?;
        next.set_reg(dst, new_value)?;
        verification_context.states.push(next);
        Ok(())
    }

    fn compute_branch(
        &self,
        jump_width: JumpWidth,
        op1: &Type,
        op2: &Type,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<Option<bool>, String> {
        match (jump_width, op1, op2) {
            (
                _,
                Type::ScalarValue { value: x, unknown_mask: 0, .. },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) => Ok(Some(op(*x, *y))),

            (
                _,
                Type::ScalarValue { unwritten_mask: 0, .. },
                Type::ScalarValue { unwritten_mask: 0, .. },
            )
            | (
                JumpWidth::W64,
                Type::ScalarValue { value: 0, unknown_mask: 0, .. },
                Type::NullOr { .. },
            )
            | (
                JumpWidth::W64,
                Type::NullOr { .. },
                Type::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) => Ok(None),

            (JumpWidth::W64, Type::PtrToStack { offset: x }, Type::PtrToStack { offset: y }) => {
                Ok(Some(op(x.reg(), y.reg())))
            }

            (
                JumpWidth::W64,
                Type::PtrToMemory { id: id1, offset: x, .. },
                Type::PtrToMemory { id: id2, offset: y, .. },
            ) if *id1 == *id2 => Ok(Some(op(*x, *y))),

            (
                JumpWidth::W64,
                Type::PtrToArray { id: id1, offset: x, .. },
                Type::PtrToArray { id: id2, offset: y, .. },
            ) if *id1 == *id2 => Ok(Some(op(*x, *y))),

            (JumpWidth::W64, Type::PtrToArray { id: id1, .. }, Type::PtrToEndArray { id: id2 })
            | (JumpWidth::W64, Type::PtrToEndArray { id: id1 }, Type::PtrToArray { id: id2, .. })
                if *id1 == *id2 =>
            {
                Ok(None)
            }

            _ => Err(format!("non permitted comparison at pc {}", self.pc)),
        }
    }

    fn conditional_jump(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        src: Source,
        offset: i16,
        jump_type: JumpType,
        jump_width: JumpWidth,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<(), String> {
        bpf_log!(
            self,
            verification_context,
            "{op_name} {}, {}, {}",
            display_register(dst),
            display_source(src),
            if offset == 0 { format!("0") } else { print_offset(offset) },
        );
        let op1 = self.reg(dst)?;
        let op2 = self.compute_source(src.clone())?;
        let apply_constraints_and_register = |mut next: Self,
                                              jump_type: JumpType|
         -> Result<Self, String> {
            if jump_type != JumpType::Unknown {
                let (new_op1, new_op2) =
                    Type::constraint(&mut next, jump_type, jump_width, op1.clone(), op2.clone());
                if dst < REGISTER_COUNT {
                    next.set_reg(dst, new_op1)?;
                }
                match src {
                    Source::Reg(r) => {
                        next.set_reg(r, new_op2)?;
                    }
                    _ => {
                        // Nothing to do
                    }
                }
            }
            Ok(next)
        };
        let branch = self.compute_branch(jump_width, &op1, &op2, op)?;
        let parent = Some(Arc::new(self.clone()));
        if branch.unwrap_or(true) {
            // Do the jump
            verification_context.states.push(apply_constraints_and_register(
                self.jump_with_offset(offset, parent.clone())?,
                jump_type,
            )?);
        }
        if !branch.unwrap_or(false) {
            // Skip the jump
            verification_context.states.push(apply_constraints_and_register(
                self.jump_with_offset(0, parent)?,
                jump_type.invert(),
            )?);
        }
        Ok(())
    }

    /// Handles the termination of a `ComputationContext`, performing branch cutting optimization.
    ///
    /// This method is called when it has been proven that the current context terminates (e.g.,
    /// reaches an `exit` instruction).
    ///
    /// The following steps are performed:
    /// 1. **Dependency Calculation:** The data dependencies of the context are computed based on
    ///    the dependencies of its terminated children and the instruction at the current PC.
    /// 2. **Context Broadening:** The context's state is broadened by clearing registers and stack
    ///    slots that are *not* in the calculated data dependencies. This optimization assumes that
    ///    data not used by the terminated branch is irrelevant for future execution paths.
    /// 3. **Termination Registration:** The broadened context is added to the set of terminating
    ///    contexts if it is not less than any existing terminating context at the same PC.
    /// 4. **Parent Termination:** If all the children of the current context have terminated,
    ///    its parent context is recursively terminated.
    fn terminate(self, verification_context: &mut VerificationContext<'_>) -> Result<(), String> {
        let mut next = Some(self);
        // Because of the potential length of the parent chain, do not use recursion.
        while let Some(mut current) = next.take() {
            // Take the parent to process it at the end and not keep it in the terminating
            // contexts.
            let parent = current.parent.take();

            // 1. Compute the dependencies of the context using the dependencies of its children
            //    and the actual operation.
            let mut dependencies = DataDependencies::default();
            for dependency in current.dependencies.get_mut().iter() {
                dependencies.merge(dependency);
            }

            dependencies.visit(
                &mut DataDependenciesVisitorContext {
                    calling_context: &verification_context.calling_context,
                    computation_context: &current,
                },
                &verification_context.code[current.pc..],
            )?;

            // 2. Clear the state depending on the dependencies states
            for register in 0..GENERAL_REGISTER_COUNT {
                if !dependencies.registers.contains(&register) {
                    current.set_reg(register, Default::default())?;
                }
            }
            current.stack.data.retain(|k, _| dependencies.stack.contains(k));

            // 3. Add the cleared state to the set of `terminating_contexts`
            let terminating_contexts =
                verification_context.terminating_contexts.entry(current.pc).or_default();
            let mut is_dominated = false;
            terminating_contexts.retain(|c| match c.computation_context.partial_cmp(&current) {
                Some(Ordering::Less) => false,
                Some(Ordering::Equal) | Some(Ordering::Greater) => {
                    // If the list contains a context greater or equal to the current one, it
                    // should not be added.
                    is_dominated = true;
                    true
                }
                _ => true,
            });
            if !is_dominated {
                terminating_contexts.push(TerminatingContext {
                    computation_context: current,
                    dependencies: dependencies.clone(),
                });
            }

            // 4. Register the computed dependencies in our parent, and terminate it if all
            //    dependencies has been computed.
            if let Some(parent) = parent {
                parent.dependencies.lock().push(dependencies);
                // To check whether all dependencies have been computed, rely on the fact that the Arc
                // count of the parent keep track of how many dependencies are left.
                next = Arc::into_inner(parent);
            }
        }
        Ok(())
    }
}

impl Drop for ComputationContext {
    fn drop(&mut self) {
        let mut next = self.parent.take().and_then(Arc::into_inner);
        // Because of the potential length of the parent chain, do not use recursion.
        while let Some(mut current) = next {
            next = current.parent.take().and_then(Arc::into_inner);
        }
    }
}

/// Two types are ordered with `t1` > `t2` if a proof that a program in a state `t1` finish is also
/// a proof that a program in a state `t2` finish.
impl PartialOrd for ComputationContext {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.pc != other.pc {
            return None;
        }
        let mut result = self.stack.partial_cmp(&other.stack)?;
        result = associate_orderings(
            result,
            Type::compare_list(self.registers.iter(), other.registers.iter())?,
        )?;
        let mut array_bound_iter1 = self.array_bounds.iter().peekable();
        let mut array_bound_iter2 = other.array_bounds.iter().peekable();
        let result = loop {
            match (array_bound_iter1.peek().cloned(), array_bound_iter2.peek().cloned()) {
                (None, None) => break result,
                (None, _) => break associate_orderings(result, Ordering::Greater)?,
                (_, None) => break associate_orderings(result, Ordering::Less)?,
                (Some((k1, v1)), Some((k2, v2))) => match k1.cmp(k2) {
                    Ordering::Equal => {
                        array_bound_iter1.next();
                        array_bound_iter2.next();
                        result = associate_orderings(result, v2.cmp(v1))?;
                    }
                    v @ Ordering::Less => {
                        array_bound_iter1.next();
                        result = associate_orderings(result, v)?;
                    }
                    v @ Ordering::Greater => {
                        array_bound_iter2.next();
                        result = associate_orderings(result, v)?;
                    }
                },
            }
        };
        Some(result)
    }
}

/// Represents the read data dependencies of an eBPF program branch.
///
/// This struct tracks which registers and stack positions are *read* by the
/// instructions within a branch of the eBPF program.  This information is used
/// during branch cutting optimization to broaden terminating contexts.
///
/// The verifier assumes that data not read by a terminated branch is irrelevant
/// for future execution paths and can be safely cleared.
#[derive(Clone, Debug, Default)]
struct DataDependencies {
    /// The set of registers read by the children of a context.
    registers: HashSet<Register>,
    /// The stack positions read by the children of a context.
    stack: HashSet<usize>,
}

impl DataDependencies {
    fn merge(&mut self, other: &DataDependencies) {
        self.registers.extend(other.registers.iter());
        self.stack.extend(other.stack.iter());
    }

    fn alu(&mut self, dst: Register, src: Source) -> Result<(), String> {
        // Only do something if the dst is read, otherwise the computation doesn't matter.
        if self.registers.contains(&dst) {
            if let Source::Reg(src) = src {
                self.registers.insert(src);
            }
        }
        Ok(())
    }

    fn jmp(&mut self, dst: Register, src: Source) -> Result<(), String> {
        self.registers.insert(dst);
        if let Source::Reg(src) = src {
            self.registers.insert(src);
        }
        Ok(())
    }

    fn atomic(
        &mut self,
        context: &ComputationContext,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
        is_cmpxchg: bool,
    ) -> Result<(), String> {
        let mut is_read = false;
        if is_cmpxchg && self.registers.contains(&0) {
            is_read = true;
        }
        if fetch && self.registers.contains(&src) {
            is_read = true;
        }
        let addr = context.reg(dst)?.clone();
        if let Type::PtrToStack { offset: stack_offset } = addr {
            let field = Field::new(offset, width);
            let final_field = context.apply_mapping(&addr, field)?.unwrap_or(field);
            let stack_offset = stack_offset + final_field.offset_as_u64();
            if is_read || self.stack.contains(&stack_offset.array_index()) {
                is_read = true;
                self.stack.insert(stack_offset.array_index());
            }
        }
        if is_read {
            self.registers.insert(0);
            self.registers.insert(src);
        }
        self.registers.insert(dst);
        Ok(())
    }
}

struct DataDependenciesVisitorContext<'a> {
    calling_context: &'a CallingContext,
    computation_context: &'a ComputationContext,
}

impl BpfVisitor for DataDependencies {
    type Context<'a> = DataDependenciesVisitorContext<'a>;

    fn add<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn add64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn and<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn and64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn arsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn arsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn div<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn div64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn lsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn lsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn r#mod<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn mod64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn mul<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn mul64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn or<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn or64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn rsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn rsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn sub<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn sub64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn xor<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn xor64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }

    fn mov<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        if src == Source::Reg(dst) || !self.registers.contains(&dst) {
            return Ok(());
        }
        if let Source::Reg(src) = src {
            self.registers.insert(src);
        }
        self.registers.remove(&dst);
        Ok(())
    }
    fn mov64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.mov(context, dst, src)
    }

    fn neg<'a>(&mut self, _context: &mut Self::Context<'a>, _dst: Register) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }
    fn neg64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        _dst: Register,
    ) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }

    fn be<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        _dst: Register,
        _width: DataWidth,
    ) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }
    fn le<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        _dst: Register,
        _width: DataWidth,
    ) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }

    fn call_external<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        index: u32,
    ) -> Result<(), String> {
        let Some(signature) = context.calling_context.functions.get(&index).cloned() else {
            return Err(format!("unknown external function {}", index));
        };
        self.registers.remove(&0);
        for register in 0..signature.args.len() {
            self.registers.insert((register + 1) as Register);
        }
        Ok(())
    }

    fn exit<'a>(&mut self, _context: &mut Self::Context<'a>) -> Result<(), String> {
        // This read r0 unconditionally.
        self.registers.insert(0);
        Ok(())
    }

    fn jump<'a>(&mut self, _context: &mut Self::Context<'a>, _offset: i16) -> Result<(), String> {
        // This doesn't do anything with values.
        Ok(())
    }

    fn jeq<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jeq64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jne<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jne64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jge<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jge64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jgt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jgt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jle<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jle64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jlt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jlt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsge<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsge64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsgt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsgt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsle<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsle64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jslt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jslt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jset<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jset64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }

    fn atomic_add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_xchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_xchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_cmpxchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, true, dst, offset, src, DataWidth::U32, true)
    }

    fn atomic_cmpxchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, true, dst, offset, src, DataWidth::U64, true)
    }

    fn load<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        let context = &context.computation_context;
        if self.registers.contains(&dst) {
            let addr = context.reg(src)?.clone();
            if let Type::PtrToStack { offset: stack_offset } = addr {
                let field = Field::new(offset, width);
                let final_field = context.apply_mapping(&addr, field)?.unwrap_or(field);
                let stack_offset = stack_offset + final_field.offset_as_u64();
                self.stack.insert(stack_offset.array_index());
            }
        }
        self.registers.insert(src);
        Ok(())
    }

    fn load64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        _value: u64,
        _jump_offset: i16,
    ) -> Result<(), String> {
        self.registers.remove(&dst);
        Ok(())
    }

    fn store<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Source,
        width: DataWidth,
    ) -> Result<(), String> {
        let context = &context.computation_context;
        let addr = context.reg(dst)?.clone();
        if let Type::PtrToStack { offset: stack_offset } = addr {
            let field = Field::new(offset, width);
            let final_field = context.apply_mapping(&addr, field)?.unwrap_or(field);
            let stack_offset = stack_offset + final_field.offset_as_u64();
            if self.stack.remove(&stack_offset.array_index()) {
                if let Source::Reg(src) = src {
                    self.registers.insert(src);
                }
            }
        } else {
            if let Source::Reg(src) = src {
                self.registers.insert(src);
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
struct TerminatingContext {
    computation_context: ComputationContext,
    dependencies: DataDependencies,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AluType {
    Plain,
    Bitwise,
    Shift,
    Arsh,
    Sub,
    Add,
}

impl AluType {
    /// Can this operation be done one a pointer and a scalar.
    fn is_ptr_compatible(&self) -> bool {
        match self {
            Self::Sub | Self::Add => true,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JumpWidth {
    W32,
    W64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JumpType {
    Eq,
    Ge,
    Gt,
    Le,
    LooseComparaison,
    Lt,
    Ne,
    StrictComparaison,
    Unknown,
}

impl JumpType {
    fn invert(&self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ge => Self::Lt,
            Self::Gt => Self::Le,
            Self::Le => Self::Gt,
            Self::LooseComparaison => Self::StrictComparaison,
            Self::Lt => Self::Ge,
            Self::Ne => Self::Eq,
            Self::StrictComparaison => Self::LooseComparaison,
            Self::Unknown => Self::Unknown,
        }
    }

    fn is_strict(&self) -> bool {
        match self {
            Self::Gt | Self::Lt | Self::Ne | Self::StrictComparaison => true,
            _ => false,
        }
    }
}

fn display_register(register: Register) -> String {
    format!("%r{register}")
}

fn display_source(src: Source) -> String {
    match src {
        Source::Reg(r) => display_register(r),
        Source::Value(v) => format!("0x{v:x}"),
    }
}

impl BpfVisitor for ComputationContext {
    type Context<'a> = VerificationContext<'a>;

    fn add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("add32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| x.overflowing_add(y).0)
        })
    }
    fn add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("add"), context, dst, src, AluType::Add, |x, y| x.overflowing_add(y).0)
    }
    fn and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("and32"), context, dst, src, AluType::Bitwise, |x, y| {
            alu32(x, y, |x, y| x & y)
        })
    }
    fn and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("and"), context, dst, src, AluType::Bitwise, |x, y| x & y)
    }
    fn arsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("arsh32"), context, dst, src, AluType::Arsh, |x, y| {
            alu32(x, y, |x, y| {
                let x = x as i32;
                x.overflowing_shr(y).0 as u32
            })
        })
    }
    fn arsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("arsh"), context, dst, src, AluType::Arsh, |x, y| {
            let x = x as i64;
            x.overflowing_shr(y as u32).0 as u64
        })
    }
    fn div<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("div32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| if y == 0 { 0 } else { x / y })
        })
    }
    fn div64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(
            Some("div"),
            context,
            dst,
            src,
            AluType::Plain,
            |x, y| if y == 0 { 0 } else { x / y },
        )
    }
    fn lsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("lsh32"), context, dst, src, AluType::Shift, |x, y| {
            alu32(x, y, |x, y| x.overflowing_shl(y).0)
        })
    }
    fn lsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("lsh"), context, dst, src, AluType::Shift, |x, y| {
            x.overflowing_shl(y as u32).0
        })
    }
    fn r#mod<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("mod32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| if y == 0 { x } else { x % y })
        })
    }
    fn mod64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(
            Some("mod"),
            context,
            dst,
            src,
            AluType::Plain,
            |x, y| if y == 0 { x } else { x % y },
        )
    }
    fn mov<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        bpf_log!(self, context, "mov32 {}, {}", display_register(dst), display_source(src));
        let src = self.compute_source(src)?;
        let value = match src {
            Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                let value = (value as u32) as u64;
                let unknown_mask = (unknown_mask as u32) as u64;
                let unwritten_mask = (unwritten_mask as u32) as u64;
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            _ => Type::default(),
        };
        let mut next = self.next()?;
        next.set_reg(dst, value)?;
        context.states.push(next);
        Ok(())
    }
    fn mov64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        bpf_log!(self, context, "mov {}, {}", display_register(dst), display_source(src));
        let src = self.compute_source(src)?;
        let mut next = self.next()?;
        next.set_reg(dst, src)?;
        context.states.push(next);
        Ok(())
    }
    fn mul<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("mul32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| x.overflowing_mul(y).0)
        })
    }
    fn mul64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("mul"), context, dst, src, AluType::Plain, |x, y| x.overflowing_mul(y).0)
    }
    fn or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("or32"), context, dst, src, AluType::Bitwise, |x, y| {
            alu32(x, y, |x, y| x | y)
        })
    }
    fn or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("or"), context, dst, src, AluType::Bitwise, |x, y| x | y)
    }
    fn rsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("rsh32"), context, dst, src, AluType::Shift, |x, y| {
            alu32(x, y, |x, y| x.overflowing_shr(y).0)
        })
    }
    fn rsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("rsh"), context, dst, src, AluType::Shift, |x, y| {
            x.overflowing_shr(y as u32).0
        })
    }
    fn sub<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("sub32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| x.overflowing_sub(y).0)
        })
    }
    fn sub64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("sub"), context, dst, src, AluType::Sub, |x, y| x.overflowing_sub(y).0)
    }
    fn xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("xor32"), context, dst, src, AluType::Bitwise, |x, y| {
            alu32(x, y, |x, y| x ^ y)
        })
    }
    fn xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("xor"), context, dst, src, AluType::Bitwise, |x, y| x ^ y)
    }

    fn neg<'a>(&mut self, context: &mut Self::Context<'a>, dst: Register) -> Result<(), String> {
        bpf_log!(self, context, "neg32 {}", display_register(dst));
        self.alu(None, context, dst, Source::Value(0), AluType::Plain, |x, y| {
            alu32(x, y, |x, _y| {
                let x = x as i32;
                let x = -x;
                x as u32
            })
        })
    }
    fn neg64<'a>(&mut self, context: &mut Self::Context<'a>, dst: Register) -> Result<(), String> {
        bpf_log!(self, context, "neg {}", display_register(dst));
        self.alu(None, context, dst, Source::Value(0), AluType::Plain, |x, _y| {
            let x = x as i64;
            let x = -x;
            x as u64
        })
    }

    fn be<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        self.endianness::<BigEndian>("be", context, dst, width)
    }

    fn le<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        self.endianness::<LittleEndian>("le", context, dst, width)
    }

    fn call_external<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        index: u32,
    ) -> Result<(), String> {
        bpf_log!(self, context, "call 0x{:x}", index);
        let Some(signature) = context.calling_context.functions.get(&index).cloned() else {
            return Err(format!("unknown external function {} at pc {}", index, self.pc));
        };
        debug_assert!(signature.args.len() <= 5);
        let mut next = self.next()?;
        for (index, arg) in signature.args.iter().enumerate() {
            let reg = (index + 1) as u8;
            match (arg, self.reg(reg)?) {
                (Type::ScalarValueParameter, Type::ScalarValue { unwritten_mask: 0, .. })
                | (Type::ConstPtrToMapParameter, Type::ConstPtrToMap { .. }) => Ok(()),
                (
                    Type::MapKeyParameter { map_ptr_index },
                    Type::PtrToMemory { offset, buffer_size, .. },
                ) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    self.check_memory_access(offset, buffer_size, 0, schema.key_size as usize)
                }
                (Type::MapKeyParameter { map_ptr_index }, Type::PtrToStack { offset }) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    if !self.stack.can_read_data_ptr(offset, schema.key_size as u64) {
                        Err(format!("cannot read key buffer from the stack at pc {}", self.pc))
                    } else {
                        Ok(())
                    }
                }
                (
                    Type::MapValueParameter { map_ptr_index },
                    Type::PtrToMemory { offset, buffer_size, .. },
                ) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    self.check_memory_access(offset, buffer_size, 0, schema.value_size as usize)
                }
                (Type::MapValueParameter { map_ptr_index }, Type::PtrToStack { offset }) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    if !self.stack.can_read_data_ptr(offset, schema.value_size as u64) {
                        Err(format!("cannot read value buffer from the stack at pc {}", self.pc))
                    } else {
                        Ok(())
                    }
                }
                (
                    Type::MemoryParameter { size, .. },
                    Type::PtrToMemory { offset, buffer_size, .. },
                ) => {
                    let expected_size = size.size(self)?;
                    if expected_size <= buffer_size - offset {
                        Ok(())
                    } else {
                        Err(format!("out of bound read at pc {}", self.pc))
                    }
                }

                (Type::MemoryParameter { size, input, output }, Type::PtrToStack { offset }) => {
                    let size = size.size(self)?;
                    let buffer_end = offset + size;
                    if !buffer_end.is_valid() {
                        Err(format!("out of bound access at pc {}", self.pc))
                    } else {
                        if *output {
                            next.stack.write_data_ptr(self.pc, offset, size)?;
                        }
                        if !input || self.stack.can_read_data_ptr(offset, size) {
                            Ok(())
                        } else {
                            Err(format!("out of bound read at pc {}", self.pc))
                        }
                    }
                }

                (
                    Type::StructParameter { id: id1 },
                    Type::PtrToMemory { id: id2, offset: 0, .. },
                ) if *id1 == id2 => Ok(()),

                _ => Err(format!("incorrect parameter for index {index} at pc {}", self.pc)),
            }?;
        }
        // Parameters have been validated, specify the return value on return.
        if signature.invalidate_array_bounds {
            next.array_bounds.clear();
        }
        let value = self.resolve_return_value(context, &signature.return_value)?;
        next.set_reg(0, value)?;
        for i in 1..=5 {
            next.set_reg(i, Type::default())?;
        }
        context.states.push(next);
        Ok(())
    }

    fn exit<'a>(&mut self, context: &mut Self::Context<'a>) -> Result<(), String> {
        bpf_log!(self, context, "exit");
        if !matches!(self.reg(0)?, Type::ScalarValue { unwritten_mask: 0, .. }) {
            return Err(format!("register 0 is incorrect at exit time at pc {}", self.pc));
        }
        let this = self.clone();
        this.terminate(context)?;
        // Nothing to do, the program terminated with a valid scalar value.
        Ok(())
    }

    fn jump<'a>(&mut self, context: &mut Self::Context<'a>, offset: i16) -> Result<(), String> {
        bpf_log!(self, context, "ja {}", offset);
        let parent = Some(Arc::new(self.clone()));
        context.states.push(self.jump_with_offset(offset, parent)?);
        Ok(())
    }

    fn jeq<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jeq32",
            context,
            dst,
            src,
            offset,
            JumpType::Eq,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x == y),
        )
    }
    fn jeq64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jeq",
            context,
            dst,
            src,
            offset,
            JumpType::Eq,
            JumpWidth::W64,
            |x, y| x == y,
        )
    }
    fn jne<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jne32",
            context,
            dst,
            src,
            offset,
            JumpType::Ne,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x != y),
        )
    }
    fn jne64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jne",
            context,
            dst,
            src,
            offset,
            JumpType::Ne,
            JumpWidth::W64,
            |x, y| x != y,
        )
    }
    fn jge<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jge32",
            context,
            dst,
            src,
            offset,
            JumpType::Ge,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x >= y),
        )
    }
    fn jge64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jge",
            context,
            dst,
            src,
            offset,
            JumpType::Ge,
            JumpWidth::W64,
            |x, y| x >= y,
        )
    }
    fn jgt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jgt32",
            context,
            dst,
            src,
            offset,
            JumpType::Gt,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x > y),
        )
    }
    fn jgt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jgt",
            context,
            dst,
            src,
            offset,
            JumpType::Gt,
            JumpWidth::W64,
            |x, y| x > y,
        )
    }
    fn jle<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jle32",
            context,
            dst,
            src,
            offset,
            JumpType::Le,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x <= y),
        )
    }
    fn jle64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jle",
            context,
            dst,
            src,
            offset,
            JumpType::Le,
            JumpWidth::W64,
            |x, y| x <= y,
        )
    }
    fn jlt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jlt32",
            context,
            dst,
            src,
            offset,
            JumpType::Lt,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x < y),
        )
    }
    fn jlt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jlt",
            context,
            dst,
            src,
            offset,
            JumpType::Lt,
            JumpWidth::W64,
            |x, y| x < y,
        )
    }
    fn jsge<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsge32",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x >= y),
        )
    }
    fn jsge64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsge",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x >= y),
        )
    }
    fn jsgt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsgt32",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x > y),
        )
    }
    fn jsgt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsgt",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x > y),
        )
    }
    fn jsle<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsle32",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x <= y),
        )
    }
    fn jsle64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsle",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x <= y),
        )
    }
    fn jslt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jslt32",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x < y),
        )
    }
    fn jslt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jslt",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x < y),
        )
    }
    fn jset<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jset32",
            context,
            dst,
            src,
            offset,
            JumpType::Unknown,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x & y != 0),
        )
    }
    fn jset64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jset",
            context,
            dst,
            src,
            offset,
            JumpType::Unknown,
            JumpWidth::W64,
            |x, y| x & y != 0,
        )
    }

    fn atomic_add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "add32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Add,
            |x, y| alu32(x, y, |x, y| x + y),
        )
    }

    fn atomic_add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "add",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Add,
            |x, y| x + y,
        )
    }

    fn atomic_and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "and32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| alu32(x, y, |x, y| x & y),
        )
    }

    fn atomic_and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "and",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| x & y,
        )
    }

    fn atomic_or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "or32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| alu32(x, y, |x, y| x | y),
        )
    }

    fn atomic_or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "or",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| x | y,
        )
    }

    fn atomic_xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "xor32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| alu32(x, y, |x, y| x ^ y),
        )
    }

    fn atomic_xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "xor",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| x ^ y,
        )
    }

    fn atomic_xchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "xchg32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Plain,
            |_, x| x,
        )
    }

    fn atomic_xchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.raw_atomic_operation(
            "xchg",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            |_, x| Ok(x),
        )
    }

    fn atomic_cmpxchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.raw_atomic_cmpxchg("cmpxchg32", context, dst, offset, src, JumpWidth::W32, |x, y| {
            comp32(x, y, |x, y| x == y)
        })
    }

    fn atomic_cmpxchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.raw_atomic_cmpxchg("cmpxchg", context, dst, offset, src, JumpWidth::W64, |x, y| x == y)
    }

    fn load<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        bpf_log!(
            self,
            context,
            "ldx{} {}, [{}{}]",
            width.str(),
            display_register(dst),
            display_register(src),
            print_offset(offset)
        );
        let addr = self.reg(src)?.clone();
        let field = self.apply_and_register_mapping(context, &addr, Field::new(offset, width))?;
        let loaded_type = self.load_memory(addr, field)?;
        let mut next = self.next()?;
        next.set_reg(dst, loaded_type)?;
        context.states.push(next);
        Ok(())
    }

    fn load64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        value: u64,
        jump_offset: i16,
    ) -> Result<(), String> {
        bpf_log!(self, context, "lddw {}, 0x{:x}", display_register(dst), value);
        let value =
            if let Some(schema) = context.calling_context.map_references.get(&self.pc).cloned() {
                let id = context.next_id();
                Type::ConstPtrToMap { id, schema }
            } else {
                Type::from(value)
            };
        let parent = Some(Arc::new(self.clone()));
        let mut next = self.jump_with_offset(jump_offset, parent)?;
        next.set_reg(dst, value.into())?;
        context.states.push(next);
        Ok(())
    }

    fn store<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Source,
        width: DataWidth,
    ) -> Result<(), String> {
        let value = match src {
            Source::Reg(r) => {
                bpf_log!(
                    self,
                    context,
                    "stx{} [{}{}], {}",
                    width.str(),
                    display_register(dst),
                    print_offset(offset),
                    display_register(r),
                );
                self.reg(r)?.clone()
            }
            Source::Value(v) => {
                bpf_log!(
                    self,
                    context,
                    "st{} [{}{}], 0x{:x}",
                    width.str(),
                    display_register(dst),
                    print_offset(offset),
                    v,
                );
                Type::from(v & Type::mask(width))
            }
        };
        let mut next = self.next()?;
        let addr = self.reg(dst)?.clone();
        let field = self.apply_and_register_mapping(context, &addr, Field::new(offset, width))?;
        next.store_memory(&addr, field, value)?;
        context.states.push(next);
        Ok(())
    }
}

fn alu32(x: u64, y: u64, op: impl FnOnce(u32, u32) -> u32) -> u64 {
    op(x as u32, y as u32) as u64
}

fn comp32(x: u64, y: u64, op: impl FnOnce(u32, u32) -> bool) -> bool {
    op(x as u32, y as u32)
}

fn scomp64(x: u64, y: u64, op: impl FnOnce(i64, i64) -> bool) -> bool {
    op(x as i64, y as i64)
}

fn scomp32(x: u64, y: u64, op: impl FnOnce(i32, i32) -> bool) -> bool {
    op(x as i32, y as i32)
}

fn print_offset(offset: i16) -> String {
    if offset == 0 {
        String::new()
    } else if offset > 0 {
        format!("+{offset}")
    } else {
        format!("{offset}")
    }
}

fn run_on_stack_offset<F>(v: StackOffset, f: F) -> StackOffset
where
    F: FnOnce(u64) -> u64,
{
    StackOffset(f(v.reg()))
}

fn error_and_log<T>(
    logger: &mut dyn VerifierLogger,
    msg: impl std::string::ToString,
) -> Result<T, EbpfError> {
    let msg = msg.to_string();
    logger.log(msg.as_bytes());
    return Err(EbpfError::ProgramLoadError(msg));
}

fn associate_orderings(o1: Ordering, o2: Ordering) -> Option<Ordering> {
    match (o1, o2) {
        (o1, o2) if o1 == o2 => Some(o1),
        (o, Ordering::Equal) | (Ordering::Equal, o) => Some(o),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_ordering() {
        let t0 = Type::from(0);
        let t1 = Type::from(1);
        let random = Type::AliasParameter { parameter_index: 8 };
        let unknown_written = Type::unknown_written_scalar_value();
        let unwritten = Type::default();

        assert_eq!(t0.partial_cmp(&t0), Some(Ordering::Equal));
        assert_eq!(t0.partial_cmp(&t1), None);
        assert_eq!(t0.partial_cmp(&random), None);
        assert_eq!(t0.partial_cmp(&unknown_written), Some(Ordering::Less));
        assert_eq!(t0.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(t1.partial_cmp(&t0), None);
        assert_eq!(t1.partial_cmp(&t1), Some(Ordering::Equal));
        assert_eq!(t1.partial_cmp(&random), None);
        assert_eq!(t1.partial_cmp(&unknown_written), Some(Ordering::Less));
        assert_eq!(t1.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(random.partial_cmp(&t0), None);
        assert_eq!(random.partial_cmp(&t1), None);
        assert_eq!(random.partial_cmp(&random), Some(Ordering::Equal));
        assert_eq!(random.partial_cmp(&unknown_written), None);
        assert_eq!(random.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(unknown_written.partial_cmp(&t0), Some(Ordering::Greater));
        assert_eq!(unknown_written.partial_cmp(&t1), Some(Ordering::Greater));
        assert_eq!(unknown_written.partial_cmp(&random), None);
        assert_eq!(unknown_written.partial_cmp(&unknown_written), Some(Ordering::Equal));
        assert_eq!(unknown_written.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(unwritten.partial_cmp(&t0), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&t1), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&random), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&unknown_written), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&unwritten), Some(Ordering::Equal));
    }

    #[test]
    fn test_stack_ordering() {
        let mut s1 = Stack::default();
        let mut s2 = Stack::default();

        assert_eq!(s1.partial_cmp(&s2), Some(Ordering::Equal));
        s1.set(0, 0.into());
        assert_eq!(s1.partial_cmp(&s2), Some(Ordering::Less));
        assert_eq!(s2.partial_cmp(&s1), Some(Ordering::Greater));
        s2.set(1, 1.into());
        assert_eq!(s1.partial_cmp(&s2), None);
        assert_eq!(s2.partial_cmp(&s1), None);
    }

    #[test]
    fn test_context_ordering() {
        let mut c1 = ComputationContext::default();
        let mut c2 = ComputationContext::default();

        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Equal));

        c1.array_bounds.insert(1.into(), 5);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Less));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Greater));

        c2.array_bounds.insert(1.into(), 7);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Greater));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Less));

        c1.array_bounds.insert(2.into(), 9);
        assert_eq!(c1.partial_cmp(&c2), None);
        assert_eq!(c2.partial_cmp(&c1), None);

        c2.array_bounds.insert(2.into(), 9);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Greater));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Less));

        c2.array_bounds.insert(3.into(), 12);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Greater));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Less));

        c1.pc = 8;
        assert_eq!(c1.partial_cmp(&c2), None);
        assert_eq!(c2.partial_cmp(&c1), None);
    }
}
