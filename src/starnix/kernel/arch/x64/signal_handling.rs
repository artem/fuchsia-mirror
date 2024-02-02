// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::{
    arch::registers::RegisterState,
    signals::{SignalInfo, SignalState},
    task::{CurrentTask, Task},
};
use extended_pstate::ExtendedPstateState;
use starnix_logging::log_debug;
use starnix_uapi::{
    self as uapi, __NR_restart_syscall, error,
    errors::{Errno, ErrnoCode, ERESTART_RESTARTBLOCK},
    sigaction, sigaltstack, sigcontext, siginfo_t, ucontext,
    user_address::UserAddress,
};
use static_assertions::const_assert_eq;

/// The size of the red zone.
///
/// From the AMD64 ABI:
///   > The 128-byte area beyond the location pointed to
///   > by %rsp is considered to be reserved and shall not be modified by signal or
///   > interrupt handlers. Therefore, functions may use this area for temporary
///   > data that is not needed across function calls. In particular, leaf functions
///   > may use this area for their entire stack frame, rather than adjusting the
///   > stack pointer in the prologue and epilogue. This area is known as the red
///   > zone.
pub const RED_ZONE_SIZE: u64 = 128;

/// The size of the syscall instruction in bytes.
pub const SYSCALL_INSTRUCTION_SIZE_BYTES: u64 = 2;

/// A `SignalStackFrame` contains all the state that is stored on the stack prior
/// to executing a signal handler.
///
/// The ordering of the fields is significant, as it is part of the syscall ABI. In particular,
/// restorer_address must be the first field, since that is where the signal handler will return
/// after it finishes executing.
#[repr(C)]
pub struct SignalStackFrame {
    /// The address of the signal handler function.
    ///
    /// Must be the first field, to be positioned to serve as the return address.
    restorer_address: u64,

    /// Information about the signal.
    pub siginfo_bytes: [u8; std::mem::size_of::<siginfo_t>()],

    /// The state of the thread at the time the signal was handled.
    pub context: ucontext,

    /// Extended CPU state, i.e, FPU, SSE & AVX registers.
    xstate: XState,
}

/// CPU state that needs to restored when returning from the signal handler and that is not
/// include in the `ucontext`. Currently it contains just `uapi::_xstate` that stores  X87, SSE
/// and AVX registers. This matches the set of extensions supported by Zircon. In the future it
/// may be extended with a buffer for other extensions (e.g. AVX-512). That buffer should be added
/// between `xstate` and `xstate_magic2`.
/// See https://github.com/google/gvisor/blob/master/pkg/sentry/arch/fpu/fpu_amd64_unsafe.go
/// for the corresponding code in GVisor.
#[repr(C, packed)]
struct XState {
    base_xstate: uapi::_xstate,

    // Magic value marking the end of the `xstate`. Should be set to `FP_XSTATE_MAGIC2`.
    xstate_magic2: u32,
}

// There should be no padding in front of `xstate_magic2`.
const_assert_eq!(
    std::mem::size_of::<XState>(),
    std::mem::size_of::<uapi::_xstate>() + std::mem::size_of::<u32>()
);

pub const SIG_STACK_SIZE: usize = std::mem::size_of::<SignalStackFrame>();

impl SignalStackFrame {
    pub fn new(
        _task: &Task,
        registers: &RegisterState,
        extended_pstate: &ExtendedPstateState,
        signal_state: &SignalState,
        siginfo: &SignalInfo,
        action: sigaction,
        stack_pointer: UserAddress,
    ) -> SignalStackFrame {
        let fpstate_addr = (uapi::uaddr {
            addr: stack_pointer.ptr() as u64
                + memoffset::offset_of!(SignalStackFrame, xstate) as u64,
        })
        .into();
        let context = ucontext {
            uc_mcontext: sigcontext {
                r8: registers.r8,
                r9: registers.r9,
                r10: registers.r10,
                r11: registers.r11,
                r12: registers.r12,
                r13: registers.r13,
                r14: registers.r14,
                r15: registers.r15,
                rdi: registers.rdi,
                rsi: registers.rsi,
                rbp: registers.rbp,
                rbx: registers.rbx,
                rdx: registers.rdx,
                rax: registers.rax,
                rcx: registers.rcx,
                rsp: registers.rsp,
                rip: registers.rip,
                eflags: registers.rflags,
                oldmask: signal_state.mask().into(),
                fpstate: fpstate_addr,
                ..Default::default()
            },
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
            ..Default::default()
        };
        SignalStackFrame {
            context,
            siginfo_bytes: siginfo.as_siginfo_bytes(),
            restorer_address: action.sa_restorer.addr,
            xstate: get_xstate(extended_pstate),
        }
    }

    pub fn as_bytes(&self) -> &[u8; SIG_STACK_SIZE] {
        unsafe { std::mem::transmute(self) }
    }

    pub fn from_bytes(bytes: [u8; SIG_STACK_SIZE]) -> SignalStackFrame {
        unsafe { std::mem::transmute(bytes) }
    }
}

/// Aligns the stack pointer to be 16 byte aligned, and then misaligns it by 8 bytes.
///
/// This is done because x86-64 functions expect the stack to be misaligned by 8 bytes,
/// as if the stack was 16 byte aligned and then someone used a call instruction. This
/// is due to alignment-requiring SSE instructions.
pub fn align_stack_pointer(pointer: u64) -> u64 {
    pointer - (pointer % 16 + 8)
}

fn get_xstate(extended_pstate: &ExtendedPstateState) -> XState {
    const_assert_eq!(std::mem::size_of::<uapi::_xstate>(), extended_pstate::X64_XSAVE_AREA_SIZE);

    let mut xstate = XState {
        // `_xstate` layout matches the layout of the XSAVE area.
        base_xstate: unsafe { std::mem::transmute(extended_pstate.get_x64_xsave_area()) },
        xstate_magic2: uapi::FP_XSTATE_MAGIC2,
    };

    xstate.base_xstate.fpstate.__bindgen_anon_1.sw_reserved = uapi::_fpx_sw_bytes {
        // `FP_XSTATE_MAGIC1` is used to indicate that the signal stack contains the `xstate`,
        // which includes not just the default X87 registers (included in `fpstate`), but also
        // other extensions, such as SSE and AVX. The end of the `xstate` buffer is marked with
        // `FP_XSTATE_MAGIC2`.
        magic1: uapi::FP_XSTATE_MAGIC1,
        extended_size: std::mem::size_of::<XState>() as u32,
        // TODO: CPU features should be detected dynamically.
        xfeatures: extended_pstate::X64_SUPPORTED_XSAVE_FEATURES,
        xstate_size: std::mem::size_of::<uapi::_xstate>() as u32,
        ..Default::default()
    };

    xstate
}

pub fn restore_registers(
    current_task: &mut CurrentTask,
    signal_stack_frame: &SignalStackFrame,
) -> Result<(), Errno> {
    let uctx = &signal_stack_frame.context.uc_mcontext;
    // Restore the register state from before executing the signal handler.
    current_task.thread_state.registers = zx::sys::zx_thread_state_general_regs_t {
        r8: uctx.r8,
        r9: uctx.r9,
        r10: uctx.r10,
        r11: uctx.r11,
        r12: uctx.r12,
        r13: uctx.r13,
        r14: uctx.r14,
        r15: uctx.r15,
        rax: uctx.rax,
        rbx: uctx.rbx,
        rcx: uctx.rcx,
        rdx: uctx.rdx,
        rsi: uctx.rsi,
        rdi: uctx.rdi,
        rbp: uctx.rbp,
        rsp: uctx.rsp,
        rip: uctx.rip,
        rflags: uctx.eflags,
        fs_base: current_task.thread_state.registers.fs_base,
        gs_base: current_task.thread_state.registers.gs_base,
    }
    .into();

    let xstate = &signal_stack_frame.xstate;
    let fpx_sw_bytes = unsafe { xstate.base_xstate.fpstate.__bindgen_anon_1.sw_reserved };
    if fpx_sw_bytes.magic1 != uapi::FP_XSTATE_MAGIC1
        || fpx_sw_bytes.extended_size != std::mem::size_of::<XState>() as u32
        || fpx_sw_bytes.xfeatures != extended_pstate::X64_SUPPORTED_XSAVE_FEATURES
        || fpx_sw_bytes.xstate_size != std::mem::size_of::<uapi::_xstate>() as u32
        || xstate.xstate_magic2 != uapi::FP_XSTATE_MAGIC2
    {
        log_debug!("Invalid xstate found in signal stack frame.");
        return error!(EINVAL);
    }

    current_task
        .thread_state
        .extended_pstate
        .set_x64_xsave_area(unsafe { std::mem::transmute(xstate.base_xstate) });

    Ok(())
}

pub fn update_register_state_for_restart(registers: &mut RegisterState, err: ErrnoCode) {
    registers.rax = match err {
        // Custom restart, invoke restart_syscall instead of the original syscall.
        ERESTART_RESTARTBLOCK => __NR_restart_syscall as u64,
        // If the restart is not custom, simply replace `rax` with the value it had when the
        // original syscall trap occurred.
        _ => registers.orig_rax,
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        mm::{DesiredAddress, MappingName, MappingOptions, ProtectionFlags},
        signals::{restore_from_signal_handler, testing::dequeue_signal_for_test, SignalDetail},
        task::Kernel,
        testing::*,
        vfs::FileWriteGuardRef,
    };
    use starnix_uapi::{
        __NR_rt_sigreturn,
        errors::{EINTR, ERESTARTSYS},
        sigaction,
        signals::{SIGUSR1, SIGUSR2},
        user_address::UserAddress,
        SA_RESTART, SA_RESTORER, SA_SIGINFO, SI_USER,
    };

    const SYSCALL_INSTRUCTION_ADDRESS: UserAddress = UserAddress::const_from(100);
    const SYSCALL_NUMBER: u64 = 42;
    const SYSCALL_ARGS: (u64, u64, u64, u64, u64, u64) = (20, 21, 22, 23, 24, 25);
    const SA_RESTORER_ADDRESS: UserAddress = UserAddress::const_from(0xDEADBEEF);
    const SA_HANDLER_ADDRESS: UserAddress = UserAddress::const_from(0x00BADDAD);

    const SYSCALL2_INSTRUCTION_ADDRESS: UserAddress = UserAddress::const_from(200);
    const SYSCALL2_NUMBER: u64 = 84;
    const SYSCALL2_ARGS: (u64, u64, u64, u64, u64, u64) = (30, 31, 32, 33, 34, 35);
    const SA_HANDLER2_ADDRESS: UserAddress = UserAddress::const_from(0xBADDAD00);

    #[::fuchsia::test]
    async fn syscall_restart_adjusts_instruction_pointer_and_rax() {
        let (_kernel, mut current_task) = create_kernel_and_task_with_stack();

        // Register the signal action.
        current_task.thread_group.signal_actions.set(
            SIGUSR1,
            sigaction {
                sa_flags: (SA_RESTORER | SA_RESTART | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER_ADDRESS.into(),
                sa_restorer: SA_RESTORER_ADDRESS.into(),
                ..sigaction::default()
            },
        );

        // Simulate a syscall that should be restarted by setting up the register state to what it
        // was after the interrupted syscall. `rax` should have the return value (-ERESTARTSYS);
        // `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, should be the syscall arguments;
        // `orig_rax` should hold the syscall number;
        // and the instruction pointer should be 2 bytes after the syscall instruction.
        current_task.thread_state.registers.rax = ERESTARTSYS.return_value();
        current_task.thread_state.registers.rdi = SYSCALL_ARGS.0;
        current_task.thread_state.registers.rsi = SYSCALL_ARGS.1;
        current_task.thread_state.registers.rdx = SYSCALL_ARGS.2;
        current_task.thread_state.registers.r10 = SYSCALL_ARGS.3;
        current_task.thread_state.registers.r8 = SYSCALL_ARGS.4;
        current_task.thread_state.registers.r9 = SYSCALL_ARGS.5;
        current_task.thread_state.registers.orig_rax = SYSCALL_NUMBER;
        current_task.thread_state.registers.rip = (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(
            SIGUSR1,
            SI_USER as i32,
            SignalDetail::None,
        ));

        // Process the signal.
        dequeue_signal_for_test(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.thread_state.registers.rip, SA_HANDLER_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.thread_state.registers.rdi, SYSCALL_ARGS.0);
        assert_ne!(current_task.thread_state.registers.rsi, SYSCALL_ARGS.1);
        assert_ne!(current_task.thread_state.registers.rdx, SYSCALL_ARGS.2);

        // Now we assume that execution of the signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.thread_state.registers.rax = __NR_rt_sigreturn as u64;
        current_task.thread_state.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.thread_state.registers.rax, SYSCALL_NUMBER);
        assert_eq!(current_task.thread_state.registers.rdi, SYSCALL_ARGS.0);
        assert_eq!(current_task.thread_state.registers.rsi, SYSCALL_ARGS.1);
        assert_eq!(current_task.thread_state.registers.rdx, SYSCALL_ARGS.2);
        assert_eq!(current_task.thread_state.registers.r10, SYSCALL_ARGS.3);
        assert_eq!(current_task.thread_state.registers.r8, SYSCALL_ARGS.4);
        assert_eq!(current_task.thread_state.registers.r9, SYSCALL_ARGS.5);
        assert_eq!(
            current_task.thread_state.registers.rip,
            SYSCALL_INSTRUCTION_ADDRESS.ptr() as u64
        );
    }

    #[::fuchsia::test]
    async fn syscall_nested_restart() {
        let (_kernel, mut current_task) = create_kernel_and_task_with_stack();

        // Register the signal actions.
        current_task.thread_group.signal_actions.set(
            SIGUSR1,
            sigaction {
                sa_flags: (SA_RESTORER | SA_RESTART | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER_ADDRESS.into(),
                sa_restorer: SA_RESTORER_ADDRESS.into(),
                ..sigaction::default()
            },
        );
        current_task.thread_group.signal_actions.set(
            SIGUSR2,
            sigaction {
                sa_flags: (SA_RESTORER | SA_RESTART | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER2_ADDRESS.into(),
                sa_restorer: SA_RESTORER_ADDRESS.into(),
                ..sigaction::default()
            },
        );

        // Simulate a syscall that should be restarted by setting up the register state to what it
        // was after the interrupted syscall. `rax` should have the return value (-ERESTARTSYS);
        // `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, should be the syscall arguments;
        // `orig_rax` should hold the syscall number;
        // and the instruction pointer should be 2 bytes after the syscall instruction.
        current_task.thread_state.registers.rax = ERESTARTSYS.return_value();
        current_task.thread_state.registers.rdi = SYSCALL_ARGS.0;
        current_task.thread_state.registers.rsi = SYSCALL_ARGS.1;
        current_task.thread_state.registers.rdx = SYSCALL_ARGS.2;
        current_task.thread_state.registers.r10 = SYSCALL_ARGS.3;
        current_task.thread_state.registers.r8 = SYSCALL_ARGS.4;
        current_task.thread_state.registers.r9 = SYSCALL_ARGS.5;
        current_task.thread_state.registers.orig_rax = SYSCALL_NUMBER;
        current_task.thread_state.registers.rip = (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(
            SIGUSR1,
            SI_USER as i32,
            SignalDetail::None,
        ));

        // Process the signal.
        dequeue_signal_for_test(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.thread_state.registers.rip, SA_HANDLER_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.thread_state.registers.rdi, SYSCALL_ARGS.0);
        assert_ne!(current_task.thread_state.registers.rsi, SYSCALL_ARGS.1);
        assert_ne!(current_task.thread_state.registers.rdx, SYSCALL_ARGS.2);

        // Simulate another syscall being interrupted.
        current_task.thread_state.registers.rax = ERESTARTSYS.return_value();
        current_task.thread_state.registers.rdi = SYSCALL2_ARGS.0;
        current_task.thread_state.registers.rsi = SYSCALL2_ARGS.1;
        current_task.thread_state.registers.rdx = SYSCALL2_ARGS.2;
        current_task.thread_state.registers.r10 = SYSCALL2_ARGS.3;
        current_task.thread_state.registers.r8 = SYSCALL2_ARGS.4;
        current_task.thread_state.registers.r9 = SYSCALL2_ARGS.5;
        current_task.thread_state.registers.orig_rax = SYSCALL2_NUMBER;
        current_task.thread_state.registers.rip =
            (SYSCALL2_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(
            SIGUSR2,
            SI_USER as i32,
            SignalDetail::None,
        ));

        // Process the signal.
        dequeue_signal_for_test(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.thread_state.registers.rip, SA_HANDLER2_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.thread_state.registers.rdi, SYSCALL2_ARGS.0);
        assert_ne!(current_task.thread_state.registers.rsi, SYSCALL2_ARGS.1);
        assert_ne!(current_task.thread_state.registers.rdx, SYSCALL2_ARGS.2);

        // Now we assume that execution of the second signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.thread_state.registers.rax = __NR_rt_sigreturn as u64;
        current_task.thread_state.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.thread_state.registers.rax, SYSCALL2_NUMBER);
        assert_eq!(current_task.thread_state.registers.rdi, SYSCALL2_ARGS.0);
        assert_eq!(current_task.thread_state.registers.rsi, SYSCALL2_ARGS.1);
        assert_eq!(current_task.thread_state.registers.rdx, SYSCALL2_ARGS.2);
        assert_eq!(current_task.thread_state.registers.r10, SYSCALL2_ARGS.3);
        assert_eq!(current_task.thread_state.registers.r8, SYSCALL2_ARGS.4);
        assert_eq!(current_task.thread_state.registers.r9, SYSCALL2_ARGS.5);
        assert_eq!(
            current_task.thread_state.registers.rip,
            SYSCALL2_INSTRUCTION_ADDRESS.ptr() as u64
        );

        // Now we assume that execution of the first signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.thread_state.registers.rax = __NR_rt_sigreturn as u64;
        current_task.thread_state.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.thread_state.registers.rax, SYSCALL_NUMBER);
        assert_eq!(current_task.thread_state.registers.rdi, SYSCALL_ARGS.0);
        assert_eq!(current_task.thread_state.registers.rsi, SYSCALL_ARGS.1);
        assert_eq!(current_task.thread_state.registers.rdx, SYSCALL_ARGS.2);
        assert_eq!(current_task.thread_state.registers.r10, SYSCALL_ARGS.3);
        assert_eq!(current_task.thread_state.registers.r8, SYSCALL_ARGS.4);
        assert_eq!(current_task.thread_state.registers.r9, SYSCALL_ARGS.5);
        assert_eq!(
            current_task.thread_state.registers.rip,
            SYSCALL_INSTRUCTION_ADDRESS.ptr() as u64
        );
    }

    #[::fuchsia::test]
    async fn syscall_does_not_restart_if_signal_action_has_no_sa_restart_flag() {
        let (_kernel, mut current_task) = create_kernel_and_task_with_stack();

        // Register the signal action.
        current_task.thread_group.signal_actions.set(
            SIGUSR1,
            sigaction {
                sa_flags: (SA_RESTORER | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER_ADDRESS.into(),
                sa_restorer: SA_RESTORER_ADDRESS.into(),
                ..sigaction::default()
            },
        );

        // Simulate a syscall that should be restarted by setting up the register state to what it
        // was after the interrupted syscall. `rax` should have the return value (-ERESTARTSYS);
        // `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, should be the syscall arguments;
        // `orig_rax` should hold the syscall number;
        // and the instruction pointer should be 2 bytes after the syscall instruction.
        current_task.thread_state.registers.rax = ERESTARTSYS.return_value();
        current_task.thread_state.registers.rdi = SYSCALL_ARGS.0;
        current_task.thread_state.registers.rsi = SYSCALL_ARGS.1;
        current_task.thread_state.registers.rdx = SYSCALL_ARGS.2;
        current_task.thread_state.registers.r10 = SYSCALL_ARGS.3;
        current_task.thread_state.registers.r8 = SYSCALL_ARGS.4;
        current_task.thread_state.registers.r9 = SYSCALL_ARGS.5;
        current_task.thread_state.registers.orig_rax = SYSCALL_NUMBER;
        current_task.thread_state.registers.rip = (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(
            SIGUSR1,
            SI_USER as i32,
            SignalDetail::None,
        ));

        // Process the signal.
        dequeue_signal_for_test(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.thread_state.registers.rip, SA_HANDLER_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.thread_state.registers.rdi, SYSCALL_ARGS.0);
        assert_ne!(current_task.thread_state.registers.rsi, SYSCALL_ARGS.1);
        assert_ne!(current_task.thread_state.registers.rdx, SYSCALL_ARGS.2);

        // Now we assume that execution of the signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.thread_state.registers.rax = __NR_rt_sigreturn as u64;
        current_task.thread_state.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.thread_state.registers.rax, EINTR.return_value());
        assert_eq!(
            current_task.thread_state.registers.rip,
            (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64
        );
    }

    /// Creates a kernel and initial task, giving the task a stack.
    fn create_kernel_and_task_with_stack() -> (Arc<Kernel>, AutoReleasableTask) {
        let (kernel, mut current_task) = create_kernel_and_task();

        const STACK_SIZE: usize = 0x1000;

        // Give the task a stack.
        let prot_flags = ProtectionFlags::READ | ProtectionFlags::WRITE;
        let stack_base = current_task
            .mm()
            .map_vmo(
                DesiredAddress::Any,
                Arc::new(zx::Vmo::create(STACK_SIZE as u64).expect("failed to create stack VMO")),
                0,
                STACK_SIZE,
                prot_flags,
                MappingOptions::empty(),
                MappingName::Stack,
                FileWriteGuardRef(None),
            )
            .expect("failed to map stack VMO");
        current_task.thread_state.registers.rsp = (stack_base + (STACK_SIZE - 8)).ptr() as u64;

        (kernel, current_task)
    }
}
