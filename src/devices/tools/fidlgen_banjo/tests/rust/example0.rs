// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// WARNING: THIS FILE IS MACHINE GENERATED. DO NOT EDIT.
// Generated from the banjo.examples.example0 banjo file

#![cfg_attr(rustfmt, rustfmt_skip)]
#![allow(unused_imports, non_camel_case_types)]




#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Bar {
    pub f: *mut Foo,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Foo {
    pub b: Bar,
}





