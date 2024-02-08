// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use selinux::{
    permission_check::PermissionCheck,
    security_server::{SecurityServer, SecurityServerStatus},
    SecurityId,
};
use selinux_common::{security_context::SecurityContext, ProcessPermission};
use starnix_uapi::{
    error,
    errors::Errno,
    signals::{Signal, SIGCHLD, SIGKILL, SIGSTOP},
};
use std::sync::Arc;

/// Checks if creating a task is allowed.
pub(crate) fn check_task_create_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    task_sid: Option<SecurityId>,
) -> Result<(), Errno> {
    // When creating a process there is no transition involved, the source and target SIDs
    // are the current SID.
    let target_sid = task_sid.clone();
    check_permission(status, permission_check, task_sid, target_sid, ProcessPermission::Fork)
}

/// Checks the SELinux permissions required for exec. Returns the SELinux state of a resolved
/// elf if all required permissions are allowed.
pub(crate) fn check_exec_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    selinux_state: &Option<SeLinuxThreadGroupState>,
) -> Result<Option<SeLinuxResolvedElfState>, Errno> {
    if status.is_fake() || !status.is_enforcing() {
        // No-op if SELinux is in fake mode or not enforcing.
        return Ok(None);
    }

    return selinux_state.as_ref().map_or(Ok(None), |selinux_state| {
        let current_sid = &selinux_state.current_sid;
        let new_sid = if let Some(exec_sid) = &selinux_state.exec_sid {
            // Use the proc exec SID if set.
            exec_sid
        } else {
            // TODO(http://b/320436714): Check typetransition rules from the current security
            // context to the executable's security context. If none, use the current security
            // context.
            current_sid
        };
        if current_sid == new_sid {
            // No domain transition.
            // TODO(http://b/320436714): check that the current security context has execute
            // rights to the executable file.
        } else {
            // Domain transition, check that transition is allowed.
            if !permission_check.has_permission(
                current_sid.clone(),
                new_sid.clone(),
                ProcessPermission::Transition,
            ) {
                return error!(EACCES);
            }
            // TODO(http://b/320436714): Check executable permissions:
            // - allow rule from `new_sid` to the executable's security context for entrypoint
            //   permissions
            // - allow rule from `current_sid` to the executable's security context for read
            //   and execute permissions
        }
        return Ok(Some(SeLinuxResolvedElfState { sid: new_sid.clone() }));
    });
}

/// Updates the SELinux thread group state on exec, using the security ID associated with the
/// resolved elf.
pub(crate) fn update_state_on_exec(
    status: &impl SecurityServerStatus,
    _permission_check: &impl PermissionCheck,
    selinux_state: &mut Option<SeLinuxThreadGroupState>,
    elf_selinux_state: &Option<SeLinuxResolvedElfState>,
) {
    if status.is_fake() || !status.is_enforcing() {
        // No-op if SELinux is in fake mode or not enforcing.
        return;
    }

    // TODO(http://b/316181721): check if the previous state needs to be updated regardless.
    if let Some(elf_selinux_state) = elf_selinux_state {
        selinux_state.as_mut().map(|selinux_state| {
            selinux_state.previous_sid = selinux_state.current_sid.clone();
            selinux_state.current_sid = elf_selinux_state.sid.clone();
            selinux_state
        });
    }
}

/// Checks if source with `source_sid` may exercise the "getsched" permission on target with
/// `target_sid` according to SELinux server status `status` and permission checker
/// `permission`.
pub fn check_getsched_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    source_sid: Option<SecurityId>,
    target_sid: Option<SecurityId>,
) -> Result<(), Errno> {
    check_permission(status, permission_check, source_sid, target_sid, ProcessPermission::GetSched)
}

/// Checks if the task with `source_sid` is allowed to set scheduling parameters for the task with
/// `target_sid`.
pub(crate) fn check_setsched_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    source_sid: Option<SecurityId>,
    target_sid: Option<SecurityId>,
) -> Result<(), Errno> {
    check_permission(status, permission_check, source_sid, target_sid, ProcessPermission::SetSched)
}

/// Checks if the task with `source_sid` is allowed to get the process group ID of the task with
/// `target_sid`.
pub(crate) fn check_getpgid_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    source_sid: Option<SecurityId>,
    target_sid: Option<SecurityId>,
) -> Result<(), Errno> {
    check_permission(status, permission_check, source_sid, target_sid, ProcessPermission::GetPgid)
}

/// Checks if the task with `source_sid` is allowed to set the process group ID of the task with
/// `target_sid`.
pub(crate) fn check_setpgid_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    source_sid: Option<SecurityId>,
    target_sid: Option<SecurityId>,
) -> Result<(), Errno> {
    check_permission(status, permission_check, source_sid, target_sid, ProcessPermission::SetPgid)
}

/// Checks if the task with `source_sid` is allowed to send `signal` to the task with `target_sid`.
pub(crate) fn check_signal_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    source_sid: Option<SecurityId>,
    target_sid: Option<SecurityId>,
    signal: Signal,
) -> Result<(), Errno> {
    match signal {
        // The `sigkill` permission is required for sending SIGKILL.
        SIGKILL => check_permission(
            status,
            permission_check,
            source_sid,
            target_sid,
            ProcessPermission::SigKill,
        ),
        // The `sigstop` permission is required for sending SIGSTOP.
        SIGSTOP => check_permission(
            status,
            permission_check,
            source_sid,
            target_sid,
            ProcessPermission::SigStop,
        ),
        // The `sigchld` permission is required for sending SIGCHLD.
        SIGCHLD => check_permission(
            status,
            permission_check,
            source_sid,
            target_sid,
            ProcessPermission::SigChld,
        ),
        // The `signal` permission is required for sending any signal other than SIGKILL, SIGSTOP
        // or SIGCHLD.
        _ => check_permission(
            status,
            permission_check,
            source_sid,
            target_sid,
            ProcessPermission::Signal,
        ),
    }
}

/// Checks if `permission` is allowed from the task with `source_sid` to the task with `target_sid`.
fn check_permission(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    source_sid: Option<SecurityId>,
    target_sid: Option<SecurityId>,
    permission: ProcessPermission,
) -> Result<(), Errno> {
    if status.is_fake() || !status.is_enforcing() {
        // No-op if SELinux is in fake mode or not enforcing.
        return Ok(());
    }

    // TODO(http://b/316181721): once security contexts are propagated to all tasks, revisit
    // whether to deny access if the source or target SIDs are not set.
    return source_sid.map_or(Ok(()), |source_sid| {
        target_sid.map_or(Ok(()), |target_sid| {
            match permission_check.has_permission(source_sid, target_sid, permission) {
                true => Ok(()),
                false => error!(EACCES),
            }
        })
    });
}

/// The SELinux security structure for `ThreadGroup`.
#[derive(Clone, Debug, PartialEq)]
pub struct SeLinuxThreadGroupState {
    /// Current SID for the task.
    pub current_sid: SecurityId,

    /// SID for the task upon the next execve call.
    pub exec_sid: Option<SecurityId>,

    /// SID for files created by the task.
    pub fscreate_sid: Option<SecurityId>,

    /// SID for kernel-managed keys created by the task.
    pub keycreate_sid: Option<SecurityId>,

    /// SID prior to the last execve.
    pub previous_sid: SecurityId,

    /// SID for sockets created by the task.
    pub sockcreate_sid: Option<SecurityId>,
}

impl SeLinuxThreadGroupState {
    // TODO(http://b/316181721): initialize with correct values; use hard coded value for fake
    // mode. Move default value to the security server `create_sid` for Fake mode.
    pub fn new_default(security_server: &Arc<SecurityServer>) -> Self {
        let sid = security_server.security_context_to_sid(
            &SecurityContext::try_from("unconfined_u:unconfined_r:unconfined_t:s0-s0:c0-c1023")
                .unwrap(),
        );
        SeLinuxThreadGroupState {
            current_sid: sid.clone(),
            previous_sid: sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
        }
    }
}

/// The SELinux security structure for `ResolvedElf`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct SeLinuxResolvedElfState {
    /// Security ID for the transformed process.
    pub sid: SecurityId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use selinux::security_server::Mode;
    use starnix_uapi::signals::SIGTERM;

    const HOOKS_TESTS_BINARY_POLICY: &[u8] =
        include_bytes!("../../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

    fn security_server_with_policy() -> Arc<SecurityServer> {
        let policy_bytes = HOOKS_TESTS_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");
        security_server
    }

    #[fuchsia::test]
    fn permission_allowed_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        security_server.set_enforcing(true);
        let selinux_state = SeLinuxThreadGroupState::new_default(&security_server);
        let sid = Some(selinux_state.current_sid);

        assert_eq!(
            check_permission(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                sid.clone(),
                sid,
                ProcessPermission::Fork,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn permission_allowed_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let selinux_state = SeLinuxThreadGroupState::new_default(&security_server);
        let sid = Some(selinux_state.current_sid);

        assert_eq!(
            check_permission(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                sid.clone(),
                sid,
                ProcessPermission::Fork,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn task_create_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let security_context =
            SecurityContext::try_from("u:object_r:fork_yes_t").expect("invalid security context");
        let sid = Some(security_server.security_context_to_sid(&security_context));

        assert_eq!(
            check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                sid
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn task_create_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let security_context =
            SecurityContext::try_from("u:object_r:fork_no_t").expect("invalid security context");
        let sid = Some(security_server.security_context_to_sid(&security_context));

        assert_eq!(
            check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                sid
            ),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn exec_access_allowed_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        security_server.set_enforcing(true);
        let selinux_state = Some(SeLinuxThreadGroupState::new_default(&security_server));

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(None)
        );
    }

    #[fuchsia::test]
    fn exec_access_allowed_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let selinux_state = Some(SeLinuxThreadGroupState::new_default(&security_server));

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(None)
        );
    }

    #[fuchsia::test]
    fn exec_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();

        let current_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_source_t")
                .expect("invalid security context");
        let current_sid = security_server.security_context_to_sid(&current_security_context);
        let exec_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_target_t")
                .expect("invalid security context");
        let exec_sid = security_server.security_context_to_sid(&exec_security_context);
        let selinux_state = Some(SeLinuxThreadGroupState {
            current_sid: current_sid.clone(),
            exec_sid: Some(exec_sid.clone()),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
        });

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(Some(SeLinuxResolvedElfState { sid: exec_sid }))
        );
    }

    #[fuchsia::test]
    fn exec_access_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let current_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_target_t")
                .expect("invalid security context");
        let current_sid = security_server.security_context_to_sid(&current_security_context);
        let exec_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_source_t")
                .expect("invalid security context");
        let exec_sid = security_server.security_context_to_sid(&exec_security_context);
        let selinux_state = Some(SeLinuxThreadGroupState {
            current_sid: current_sid.clone(),
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
        });

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn no_state_update_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        security_server.set_enforcing(true);
        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());

        let elf_security_context =
            SecurityContext::try_from("u:object_r:type_t").expect("invalid security context");
        let elf_sid = security_server.security_context_to_sid(&elf_security_context);
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &Some(elf_state),
        );
        assert_eq!(selinux_state.expect("missing SELinux state"), initial_state);
    }

    #[fuchsia::test]
    fn no_state_update_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());

        let elf_security_context =
            SecurityContext::try_from("u:object_r:type_t").expect("invalid security context");
        let elf_sid = security_server.security_context_to_sid(&elf_security_context);
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &Some(elf_state),
        );
        assert_eq!(selinux_state.expect("missing SELinux state"), initial_state);
    }

    #[fuchsia::test]
    fn no_state_update_if_no_elf_state() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);

        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &None,
        );
        assert_eq!(selinux_state.expect("missing SELinux state"), initial_state);
    }

    #[fuchsia::test]
    fn state_is_updated_on_exec() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);

        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());

        let elf_security_context =
            SecurityContext::try_from("u:object_r:type_t").expect("invalid security context");
        let elf_sid = security_server.security_context_to_sid(&elf_security_context);
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &Some(elf_state),
        );
        assert_eq!(
            selinux_state.expect("missing SELinux state"),
            SeLinuxThreadGroupState {
                current_sid: elf_sid,
                exec_sid: initial_state.exec_sid,
                fscreate_sid: initial_state.fscreate_sid,
                keycreate_sid: initial_state.keycreate_sid,
                previous_sid: initial_state.previous_sid,
                sockcreate_sid: initial_state.sockcreate_sid,
            }
        );
    }

    #[fuchsia::test]
    fn getsched_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_getsched_yes_t")
            .expect("invalid security context");
        let target_security_context =
            SecurityContext::try_from("u:object_r:test_getsched_target_t")
                .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        assert_eq!(
            check_getsched_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn getsched_access_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_getsched_no_t")
            .expect("invalid security context");
        let target_security_context =
            SecurityContext::try_from("u:object_r:test_getsched_target_t")
                .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        assert_eq!(
            check_getsched_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid
            ),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn setsched_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_setsched_yes_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context =
            SecurityContext::try_from("u:object_r:test_setsched_target_t")
                .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));
        assert_eq!(
            check_setsched_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn setsched_access_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_setsched_no_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context =
            SecurityContext::try_from("u:object_r:test_setsched_target_t")
                .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));
        assert_eq!(
            check_setsched_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid
            ),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn getpgid_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_getpgid_yes_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context = SecurityContext::try_from("u:object_r:test_getpgid_target_t")
            .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        assert_eq!(
            check_getpgid_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn getpgid_access_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_getpgid_no_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context = SecurityContext::try_from("u:object_r:test_getpgid_target_t")
            .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));
        assert_eq!(
            check_getpgid_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid
            ),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn sigkill_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_kill_sigkill_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context = SecurityContext::try_from("u:object_r:test_kill_target_t")
            .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        assert_eq!(
            check_signal_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGKILL,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn sigchld_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_kill_sigchld_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context = SecurityContext::try_from("u:object_r:test_kill_target_t")
            .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        assert_eq!(
            check_signal_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGCHLD,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn sigstop_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_kill_sigstop_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context = SecurityContext::try_from("u:object_r:test_kill_target_t")
            .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        assert_eq!(
            check_signal_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGSTOP,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn signal_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_kill_signal_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context = SecurityContext::try_from("u:object_r:test_kill_target_t")
            .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        // The `signal` permission allows signals other than SIGKILL, SIGCHLD, SIGSTOP.
        assert_eq!(
            check_signal_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGTERM,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn signal_access_denied_for_denied_signals() {
        let security_server = security_server_with_policy();
        let source_security_context = SecurityContext::try_from("u:object_r:test_kill_signal_t")
            .expect("invalid security context");
        let source_sid = Some(security_server.security_context_to_sid(&source_security_context));
        let target_security_context = SecurityContext::try_from("u:object_r:test_kill_target_t")
            .expect("invalid security context");
        let target_sid = Some(security_server.security_context_to_sid(&target_security_context));

        // The `signal` permission does not allow SIGKILL, SIGCHLD or SIGSTOP.
        for signal in [SIGCHLD, SIGKILL, SIGSTOP] {
            assert_eq!(
                check_signal_access(
                    security_server.as_ref(),
                    &security_server.as_permission_check(),
                    source_sid.clone(),
                    target_sid.clone(),
                    signal,
                ),
                error!(EACCES)
            );
        }
    }
}
