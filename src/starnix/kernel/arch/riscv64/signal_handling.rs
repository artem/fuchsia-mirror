// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::{
    arch::registers::RegisterState,
    mm::vmo::round_up_to_increment,
    signals::{SignalInfo, SignalState},
    task::{CurrentTask, Task},
};
use extended_pstate::{
    riscv64::{RiscvVectorCsrs, NUM_V_REGISTERS, VLEN},
    ExtendedPstateState,
};
use starnix_logging::log_debug;
use starnix_uapi::{
    self as uapi, error,
    errors::{Errno, ErrnoCode, ERESTART_RESTARTBLOCK},
    sigaction, sigaltstack, sigcontext, siginfo_t, ucontext,
    user_address::UserAddress,
};

/// The size of the red zone.
// TODO(https://fxbug.dev/42072654): Determine whether or not this is the correct red zone size for riscv64.
pub const RED_ZONE_SIZE: u64 = 128;

/// The size of the syscall instruction in bytes. `ECALL` is not compressed, i.e. it always takes 4
/// bytes.
pub const SYSCALL_INSTRUCTION_SIZE_BYTES: u64 = 4;

/// The size, in bytes, of the signal stack frame.
pub const SIG_STACK_SIZE: usize = std::mem::size_of::<SignalStackFrame>();

/// A `SignalStackFrame` contains all the state that is stored on the stack prior to executing a
/// signal handler. The exact layout of this structure is part of the platform's ABI.
#[repr(C)]
pub struct SignalStackFrame {
    pub siginfo_bytes: [u8; std::mem::size_of::<siginfo_t>()],
    pub context: ucontext,

    // On RISC-V, state for extensions not included in `ucontext` can be stored immediately after
    // `ucontext`. Each extension starts with a header defined by `__riscv_ctx_hdr`. The header for
    // the first extensions is placed at the tail of `ucontext`.  The end of these extension is
    // marked by the header containing zeros (see `end_header` below).
    // Currently only V extension is supported.
    v_state: VState,

    // Should be set to `{END_MAGIC, END_HDR_SIZE}`
    end_header: uapi::__riscv_ctx_hdr,
}

pub const V_REGISTERS_SIZE: usize = VLEN / 8 * NUM_V_REGISTERS;

#[repr(C)]
struct VState {
    v_csrs: uapi::__riscv_v_ext_state,
    v_registers: [u8; V_REGISTERS_SIZE],
}

impl SignalStackFrame {
    pub fn new(
        task: &Task,
        registers: &mut RegisterState,
        extended_pstate: &ExtendedPstateState,
        signal_state: &SignalState,
        siginfo: &SignalInfo,
        _action: sigaction,
        stack_pointer: UserAddress,
    ) -> SignalStackFrame {
        let context = ucontext {
            uc_flags: 0,
            uc_link: Default::default(),
            uc_stack: signal_state
                .alt_stack
                .map(|stack| sigaltstack {
                    ss_sp: stack.ss_sp.into(),
                    ss_flags: stack.ss_flags as i32,
                    ss_size: stack.ss_size as u64,
                    ..Default::default()
                })
                .unwrap_or_default(),
            uc_sigmask: signal_state.mask().into(),
            uc_mcontext: sigcontext {
                sc_regs: registers.to_user_regs_struct(),
                __bindgen_anon_1: uapi::sigcontext__bindgen_ty_1 {
                    sc_fpregs: extended_pstate_to_riscv_fpregs(extended_pstate),
                },
            },
            ..Default::default()
        };

        let vdso_sigreturn_offset = task.kernel().vdso.sigreturn_offset;
        let sigreturn_addr = task.mm().state.read().vdso_base.ptr() as u64 + vdso_sigreturn_offset;
        registers.ra = sigreturn_addr;

        let v_registers_addr = stack_pointer.ptr()
            + memoffset::offset_of!(SignalStackFrame, v_state)
            + memoffset::offset_of!(VState, v_registers);
        let state = extended_pstate.get_riscv64_state();
        let v_state = VState {
            v_csrs: uapi::__riscv_v_ext_state {
                vstart: state.vcsrs.vstart,
                vl: state.vcsrs.vl,
                vtype: state.vcsrs.vtype,
                vcsr: state.vcsrs.vcsr,
                vlenb: VLEN as u64 / 8,
                datap: uapi::uaddr { addr: v_registers_addr as u64 },
            },
            v_registers: unsafe { std::mem::transmute(state.v_registers) },
        };

        let mut sigstack = SignalStackFrame {
            siginfo_bytes: siginfo.as_siginfo_bytes(),
            context,
            v_state,
            end_header: uapi::__riscv_ctx_hdr { magic: uapi::END_MAGIC, size: uapi::END_HDR_SIZE },
        };

        // Setup headers for `VState`. It follows immediately after `context`. Size of this
        // section also includes size of the header.
        sigstack.context.uc_mcontext.__bindgen_anon_1.sc_extdesc.hdr = uapi::__riscv_ctx_hdr {
            magic: uapi::RISCV_V_MAGIC,
            size: (std::mem::size_of::<VState>() + std::mem::size_of::<uapi::__riscv_ctx_hdr>())
                as u32,
        };

        sigstack
    }

    pub fn as_bytes(&self) -> &[u8; SIG_STACK_SIZE] {
        unsafe { std::mem::transmute(self) }
    }

    pub fn from_bytes(bytes: [u8; SIG_STACK_SIZE]) -> SignalStackFrame {
        unsafe { std::mem::transmute(bytes) }
    }
}

pub fn restore_registers(
    current_task: &mut CurrentTask,
    signal_stack_frame: &SignalStackFrame,
    stack_pointer: UserAddress,
) -> Result<(), Errno> {
    let regs = &signal_stack_frame.context.uc_mcontext.sc_regs;
    // Restore the register state from before executing the signal handler.
    current_task.thread_state.registers = zx::sys::zx_thread_state_general_regs_t {
        pc: regs.pc,
        ra: regs.ra,
        sp: regs.sp,
        gp: regs.gp,
        tp: regs.tp,
        t0: regs.t0,
        t1: regs.t1,
        t2: regs.t2,
        s0: regs.s0,
        s1: regs.s1,
        a0: regs.a0,
        a1: regs.a1,
        a2: regs.a2,
        a3: regs.a3,
        a4: regs.a4,
        a5: regs.a5,
        a6: regs.a6,
        a7: regs.a7,
        s2: regs.s2,
        s3: regs.s3,
        s4: regs.s4,
        s5: regs.s5,
        s6: regs.s6,
        s7: regs.s7,
        s8: regs.s8,
        s9: regs.s9,
        s10: regs.s10,
        s11: regs.s11,
        t3: regs.t3,
        t4: regs.t4,
        t5: regs.t5,
        t6: regs.t6,
    }
    .into();

    let v_registers_addr = stack_pointer.ptr()
        + memoffset::offset_of!(SignalStackFrame, v_state)
        + memoffset::offset_of!(VState, v_registers);
    let hdr = unsafe { &signal_stack_frame.context.uc_mcontext.__bindgen_anon_1.sc_extdesc.hdr };
    let vcsrs = &signal_stack_frame.v_state.v_csrs;
    if hdr.magic != uapi::RISCV_V_MAGIC
        || hdr.size as usize
            != std::mem::size_of::<VState>() + std::mem::size_of::<uapi::__riscv_ctx_hdr>()
        || vcsrs.vlenb != (VLEN as u64 / 8)
        || vcsrs.datap.addr != v_registers_addr as u64
    {
        log_debug!("Invalid V state found in signal stack frame.");
        return error!(EINVAL);
    }

    let d_state = unsafe { &signal_stack_frame.context.uc_mcontext.__bindgen_anon_1.sc_fpregs.d };
    let state = current_task.thread_state.extended_pstate.get_riscv64_state_mut();
    state.fp_registers = d_state.f;
    state.fcsr = d_state.fcsr;

    state.vcsrs = RiscvVectorCsrs {
        vstart: vcsrs.vstart,
        vl: vcsrs.vl,
        vtype: vcsrs.vtype,
        vcsr: vcsrs.vcsr,
    };
    unsafe {
        state.v_registers = std::mem::transmute(signal_stack_frame.v_state.v_registers);
    };

    Ok(())
}

pub fn align_stack_pointer(pointer: u64) -> u64 {
    round_up_to_increment(pointer as usize, 16).expect("Failed to round up stack pointer") as u64
}

pub fn update_register_state_for_restart(registers: &mut RegisterState, err: ErrnoCode) {
    if err == ERESTART_RESTARTBLOCK {
        // Update the register containing the syscall number to reference `restart_syscall`.
        registers.a7 = uapi::__NR_restart_syscall as u64;
    }
    // Reset the a0 register value to what it was when the original syscall trap occurred. This
    // needs to be done because a0 may have been overwritten in the syscall dispatch loop.
    registers.a0 = registers.orig_a0;
}

// Generates `__riscv_fp_state` struct from ExtendedPstateState.
fn extended_pstate_to_riscv_fpregs(
    extended_pstate: &ExtendedPstateState,
) -> starnix_uapi::__riscv_fp_state {
    let state = extended_pstate.get_riscv64_state();
    let d_state = starnix_uapi::__riscv_d_ext_state {
        f: state.fp_registers,
        fcsr: state.fcsr,
        __bindgen_padding_0: [0u8; 4],
    };
    unsafe {
        let mut r: starnix_uapi::__riscv_fp_state = std::mem::zeroed();
        r.d = d_state;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn sigstack_layout() {
        // There should be no padding between `context`, `v_state` and `end_header` in `SignalStackFrame`.
        assert_eq!(
            memoffset::offset_of!(SignalStackFrame, context) + std::mem::size_of::<ucontext>(),
            memoffset::offset_of!(SignalStackFrame, v_state)
        );
        assert_eq!(
            memoffset::offset_of!(SignalStackFrame, v_state) + std::mem::size_of::<VState>(),
            memoffset::offset_of!(SignalStackFrame, end_header)
        );
    }
}
