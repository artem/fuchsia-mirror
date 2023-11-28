// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use starnix_syscalls::{
    decls::{Syscall, SyscallDecl},
    SyscallArg,
};

/// Generates CFI directives so the unwinder will be redirected to unwind the stack provided in `state`.
#[macro_export]
macro_rules! generate_cfi_directives {
    ($state:expr) => {
        unsafe {
            let state_addr = std::ptr::addr_of!($state);
            // The base address that the unwinder will use is stored in x27. Then it will look for
            // each register value at an offset specified below. These offsets match the offsets of
            // the register values in the `zx_restricted_state_t` struct.
            std::arch::asm!(
                ".cfi_remember_state",
                ".cfi_def_cfa x27, 0",
                ".cfi_offset x0, 0",
                ".cfi_offset x1, 0x08",
                ".cfi_offset x2, 0x10",
                ".cfi_offset x3, 0x18",
                ".cfi_offset x4, 0x20",
                ".cfi_offset x5, 0x28",
                ".cfi_offset x6, 0x30",
                ".cfi_offset x7, 0x38",
                ".cfi_offset x8, 0x40",
                ".cfi_offset x9, 0x48",
                ".cfi_offset x10, 0x50",
                ".cfi_offset x11, 0x58",
                ".cfi_offset x12, 0x60",
                ".cfi_offset x13, 0x68",
                ".cfi_offset x14, 0x70",
                ".cfi_offset x15, 0x78",
                ".cfi_offset x16, 0x80",
                ".cfi_offset x17, 0x88",
                ".cfi_offset x18, 0x90",
                ".cfi_offset x19, 0x98",
                ".cfi_offset x20, 0xA0",
                ".cfi_offset x21, 0xA8",
                ".cfi_offset x22, 0xB0",
                ".cfi_offset x23, 0xB8",
                ".cfi_offset x24, 0xC0",
                ".cfi_offset x25, 0xC8",
                ".cfi_offset x26, 0xD0",
                ".cfi_offset x27, 0xD8",
                ".cfi_offset x28, 0xE0",
                ".cfi_offset x29, 0xE8",
                ".cfi_offset x30, 0xF0",  // lr
                ".cfi_offset sp, 0xF8",
                ".cfi_offset 32, 0x100",  // GNU Assembler doesn't recognize pc.
                // Not supported yet.
                // ".cfi_offset tpidr_el0, 0x108",
                // ".cfi_offset cpsr, 0x110",

                // x27 could technically get clobbered between here and `execute_syscall`.
                // TODO(https://fxbug.dev/297897817): Use a more robust approach to unwind.
                in("x27") state_addr,

                // It appears that on nelson-release, if we only put x27 here, the register will be
                // immediately used for other purposes. e.g.
                //
                //      279                 generate_cfi_directives!(state);
                //    0x8000c0dfeed4  add   x27, sp, #0x170   <-- correct
                //    0x8000c0dfeed8  add   x27, sp, #0x5a4   <-- but immediately clobbered
                //
                // Adding x28 below will redirect the compiler to use x28 and leave x27 unchanged.
                //
                //     279                 generate_cfi_directives!(state);
                //    0x8000573efed4  add   x27, sp, #0x170   <-- correct
                //    0x8000573efed8  add   x28, sp, #0x170
                //    0x8000573efedc  add   x28, sp, #0x5a4   <-- now x28 is clobbered
                in("x28") state_addr,
                options(nomem, preserves_flags, nostack),
            );
        }
    };
}

/// Generates directives to restore the CFI state.
#[macro_export]
macro_rules! restore_cfi_directives {
    () => {
        unsafe {
            // Restore the CFI state before continuing.
            std::arch::asm!(".cfi_restore_state", options(nomem, preserves_flags, nostack));
        }
    };
}

pub(crate) use generate_cfi_directives;
pub(crate) use restore_cfi_directives;

pub fn new_syscall(syscall_decl: SyscallDecl, current_task: &CurrentTask) -> Syscall {
    Syscall {
        decl: syscall_decl,
        arg0: SyscallArg::from_raw(current_task.thread_state.registers.r[0]),
        arg1: SyscallArg::from_raw(current_task.thread_state.registers.r[1]),
        arg2: SyscallArg::from_raw(current_task.thread_state.registers.r[2]),
        arg3: SyscallArg::from_raw(current_task.thread_state.registers.r[3]),
        arg4: SyscallArg::from_raw(current_task.thread_state.registers.r[4]),
        arg5: SyscallArg::from_raw(current_task.thread_state.registers.r[5]),
    }
}
