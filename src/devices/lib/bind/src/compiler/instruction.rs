// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Bind rules instructions

use crate::compiler::Symbol;
use num_derive::FromPrimitive;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct DeviceProperty {
    pub key: u32,
    pub value: u32,
}

impl fmt::Display for DeviceProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#06x} = {:#010x}", self.key, self.value)
    }
}

impl From<fidl_fuchsia_driver_legacy::DeviceProperty> for DeviceProperty {
    fn from(property: fidl_fuchsia_driver_legacy::DeviceProperty) -> Self {
        DeviceProperty { key: property.id as u32, value: property.value }
    }
}

/// For all conditions (except Always), the operands to the condition
/// are (parameter_b, value) pairs in the final encoding.
#[derive(Clone, PartialEq, Eq)]
pub enum Condition {
    Always,
    Equal(Symbol, Symbol),
    NotEqual(Symbol, Symbol),
}

#[derive(Clone)]
pub enum Instruction {
    Abort(Condition),
    Match(Condition),
    Goto(Condition, u32),
    Label(u32),
}

#[derive(Clone, FromPrimitive, PartialEq)]
pub enum RawAstLocation {
    Invalid = 0,
    ConditionStatement,
    AcceptStatementValue,
    AcceptStatementFailure,
    IfCondition,
    FalseStatement,
}

#[derive(Clone)]
pub struct InstructionDebug {
    pub line: u32,
    pub ast_location: RawAstLocation,
    pub extra: u32,
}

impl InstructionDebug {
    pub fn none() -> Self {
        InstructionDebug { line: 0, ast_location: RawAstLocation::Invalid, extra: 0 }
    }
}

pub struct InstructionInfo {
    pub instruction: Instruction,
    pub debug: InstructionDebug,
}

impl InstructionInfo {
    pub fn new(instruction: Instruction) -> Self {
        InstructionInfo { instruction, debug: InstructionDebug::none() }
    }
}
