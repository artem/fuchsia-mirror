// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]

use crate::{signals::SigSet, uapi::sigset_t, uref};
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

/// The type used by the kernel for the time in seconds in the stat struct.
pub type stat_time_t = u64;

impl From<sigset_t> for SigSet {
    fn from(value: sigset_t) -> Self {
        SigSet(value)
    }
}

impl From<SigSet> for sigset_t {
    fn from(val: SigSet) -> Self {
        val.0
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeros, NoCell)]
pub struct user_fpregs_struct {
    pub cwd: std::os::raw::c_ushort,
    pub swd: std::os::raw::c_ushort,
    pub ftw: std::os::raw::c_ushort,
    pub fop: std::os::raw::c_ushort,
    pub rip: std::os::raw::c_ulong,
    pub rdp: std::os::raw::c_ulong,
    pub mxcsr: std::os::raw::c_uint,
    pub mxcr_mask: std::os::raw::c_uint,
    pub st_space: [std::os::raw::c_uint; 32usize],
    pub xmm_space: [std::os::raw::c_uint; 64usize],
    pub padding: [std::os::raw::c_uint; 24usize],
}

impl Default for user_fpregs_struct {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeros, NoCell)]
pub struct user_regs_struct {
    pub r15: std::os::raw::c_ulong,
    pub r14: std::os::raw::c_ulong,
    pub r13: std::os::raw::c_ulong,
    pub r12: std::os::raw::c_ulong,
    pub rbp: std::os::raw::c_ulong,
    pub rbx: std::os::raw::c_ulong,
    pub r11: std::os::raw::c_ulong,
    pub r10: std::os::raw::c_ulong,
    pub r9: std::os::raw::c_ulong,
    pub r8: std::os::raw::c_ulong,
    pub rax: std::os::raw::c_ulong,
    pub rcx: std::os::raw::c_ulong,
    pub rdx: std::os::raw::c_ulong,
    pub rsi: std::os::raw::c_ulong,
    pub rdi: std::os::raw::c_ulong,
    pub orig_rax: std::os::raw::c_ulong,
    pub rip: std::os::raw::c_ulong,
    pub cs: std::os::raw::c_ulong,
    pub eflags: std::os::raw::c_ulong,
    pub rsp: std::os::raw::c_ulong,
    pub ss: std::os::raw::c_ulong,
    pub fs_base: std::os::raw::c_ulong,
    pub gs_base: std::os::raw::c_ulong,
    pub ds: std::os::raw::c_ulong,
    pub es: std::os::raw::c_ulong,
    pub fs: std::os::raw::c_ulong,
    pub gs: std::os::raw::c_ulong,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeros, NoCell)]
pub struct user {
    pub regs: user_regs_struct,
    pub u_fpvalid: std::os::raw::c_int,
    pub pad0: std::os::raw::c_int,
    pub i387: user_fpregs_struct,
    pub u_tsize: std::os::raw::c_ulong,
    pub u_dsize: std::os::raw::c_ulong,
    pub u_ssize: std::os::raw::c_ulong,
    pub start_code: std::os::raw::c_ulong,
    pub start_stack: std::os::raw::c_ulong,
    pub signal: std::os::raw::c_long,
    pub reserved: std::os::raw::c_int,
    pub pad1: std::os::raw::c_int,
    pub u_ar0: uref<user_regs_struct>,
    pub u_fpstate: uref<user_fpregs_struct>,
    pub magic: std::os::raw::c_ulong,
    pub u_comm: [std::os::raw::c_char; 32usize],
    pub u_debugreg: [std::os::raw::c_ulong; 8usize],
    pub error_code: std::os::raw::c_ulong,
    pub fault_address: std::os::raw::c_ulong,
}

impl Default for user {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
