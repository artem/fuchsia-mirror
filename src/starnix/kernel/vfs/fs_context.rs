// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{CheckAccessReason, FileSystemHandle, Namespace, NamespaceNode},
};
use starnix_logging::log_trace;
use starnix_sync::RwLock;
use starnix_uapi::{
    auth::CAP_SYS_CHROOT,
    errno, error,
    errors::Errno,
    file_mode::{Access, FileMode},
};
use std::sync::Arc;

/// The mutable state for an FsContext.
///
/// This state is cloned in FsContext::fork.
#[derive(Debug, Clone)]
struct FsContextState {
    /// The namespace tree for this FsContext.
    ///
    /// This field owns the mount table for this FsContext.
    namespace: Arc<Namespace>,

    /// The root of the namespace tree for this FsContext.
    ///
    /// Operations on the file system are typically either relative to this
    /// root or to the cwd().
    root: NamespaceNode,

    /// The current working directory.
    cwd: NamespaceNode,

    // See <https://man7.org/linux/man-pages/man2/umask.2.html>
    umask: FileMode,
}

impl FsContextState {
    fn set_namespace(&mut self, new_ns: Arc<Namespace>) -> Result<(), Errno> {
        log_trace!("updating namespace");
        let new_root =
            Namespace::translate_node(self.root.clone(), &new_ns).ok_or(errno!(EINVAL))?;
        let new_cwd = Namespace::translate_node(self.cwd.clone(), &new_ns).ok_or(errno!(EINVAL))?;

        // Only perform a mutation if the rebased nodes both exist in the target namespace.
        self.root = new_root;
        self.cwd = new_cwd;
        self.namespace = new_ns;
        log_trace!("namespace update succeeded");
        Ok(())
    }
}

/// The file system context associated with a task.
///
/// File system operations, such as opening a file or mounting a directory, are
/// performed using this context.
#[derive(Debug)]
pub struct FsContext {
    /// The mutable state for this FsContext.
    state: RwLock<FsContextState>,
}

impl FsContext {
    /// Create an FsContext for the given namespace.
    ///
    /// The root and cwd of the FsContext are initialized to the root of the
    /// namespace.
    pub fn new(root: FileSystemHandle) -> Arc<FsContext> {
        let namespace = Namespace::new(root);
        let root = namespace.root();
        Arc::new(FsContext {
            state: RwLock::new(FsContextState {
                namespace,
                root: root.clone(),
                cwd: root,
                umask: FileMode::DEFAULT_UMASK,
            }),
        })
    }

    pub fn fork(&self) -> Arc<FsContext> {
        // A child process created via fork(2) inherits its parent's umask.
        // The umask is left unchanged by execve(2).
        //
        // See <https://man7.org/linux/man-pages/man2/umask.2.html>

        Arc::new(FsContext { state: RwLock::new(self.state.read().clone()) })
    }

    /// Returns a reference to the current working directory.
    pub fn cwd(&self) -> NamespaceNode {
        let state = self.state.read();
        state.cwd.clone()
    }

    /// Returns the root.
    pub fn root(&self) -> NamespaceNode {
        let state = self.state.read();
        state.root.clone()
    }

    /// Change the current working directory.
    pub fn chdir(&self, current_task: &CurrentTask, name: NamespaceNode) -> Result<(), Errno> {
        name.check_access(current_task, Access::EXEC, CheckAccessReason::Chdir)?;
        let mut state = self.state.write();
        state.cwd = name;
        Ok(())
    }

    /// Change the root.
    pub fn chroot(&self, current_task: &CurrentTask, name: NamespaceNode) -> Result<(), Errno> {
        name.check_access(current_task, Access::EXEC, CheckAccessReason::Chroot)
            .map_err(|_| errno!(EACCES))?;
        if !current_task.creds().has_capability(CAP_SYS_CHROOT) {
            return error!(EPERM);
        }

        let mut state = self.state.write();
        state.root = name;
        Ok(())
    }

    pub fn umask(&self) -> FileMode {
        self.state.read().umask
    }

    pub fn apply_umask(&self, mode: FileMode) -> FileMode {
        let umask = self.state.read().umask;
        mode & !umask
    }

    pub fn set_umask(&self, umask: FileMode) -> FileMode {
        let mut state = self.state.write();
        let old_umask = state.umask;

        // umask() sets the calling process's file mode creation mask
        // (umask) to mask & 0o777 (i.e., only the file permission bits of
        // mask are used), and returns the previous value of the mask.
        //
        // See <https://man7.org/linux/man-pages/man2/umask.2.html>
        state.umask = umask & FileMode::from_bits(0o777);

        old_umask
    }

    pub fn set_namespace(&self, new_ns: Arc<Namespace>) -> Result<(), Errno> {
        let mut state = self.state.write();
        state.set_namespace(new_ns)?;
        Ok(())
    }

    pub fn unshare_namespace(&self) {
        let mut state = self.state.write();
        // TODO(https:://https://fxbug.dev/42080384): Implement better locking to make these failures
        // impossible. These expects can only fail if another thread changes mounts between the
        // clone_namespace and the translate_node calls, making the cwd or root disappear or move.
        let cloned = state.namespace.clone_namespace();
        state.set_namespace(cloned).expect("nodes should exist in the cloned namespace");
    }

    pub fn namespace(&self) -> Arc<Namespace> {
        Arc::clone(&self.state.read().namespace)
    }
}

#[cfg(test)]
mod test {
    use crate::{
        fs::tmpfs::TmpFs,
        testing::{create_kernel_and_task, create_kernel_task_and_unlocked_with_pkgfs},
        vfs::FsContext,
    };
    use starnix_uapi::{file_mode::FileMode, open_flags::OpenFlags};

    #[::fuchsia::test]
    async fn test_umask() {
        let (kernel, _task) = create_kernel_and_task();
        let fs = FsContext::new(TmpFs::new_fs(&kernel));

        assert_eq!(FileMode::from_bits(0o22), fs.set_umask(FileMode::from_bits(0o3020)));
        assert_eq!(FileMode::from_bits(0o646), fs.apply_umask(FileMode::from_bits(0o666)));
        assert_eq!(FileMode::from_bits(0o3646), fs.apply_umask(FileMode::from_bits(0o3666)));
        assert_eq!(FileMode::from_bits(0o20), fs.set_umask(FileMode::from_bits(0o11)));
    }

    #[::fuchsia::test]
    async fn test_chdir() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();

        assert_eq!("/", current_task.fs().cwd().path_escaping_chroot());

        let bin = current_task
            .open_file(&mut locked, "bin".into(), OpenFlags::RDONLY)
            .expect("missing bin directory");
        current_task.fs().chdir(&current_task, bin.name.clone()).expect("Failed to chdir");
        assert_eq!("/bin", current_task.fs().cwd().path_escaping_chroot());

        // Now that we have changed directories to bin, we're opening a file
        // relative to that directory, which doesn't exist.
        assert!(current_task.open_file(&mut locked, "bin".into(), OpenFlags::RDONLY).is_err());

        // However, bin still exists in the root directory.
        assert!(current_task.open_file(&mut locked, "/bin".into(), OpenFlags::RDONLY).is_ok());

        current_task
            .fs()
            .chdir(
                &current_task,
                current_task
                    .open_file(&mut locked, "..".into(), OpenFlags::RDONLY)
                    .expect("failed to open ..")
                    .name
                    .clone(),
            )
            .expect("Failed to chdir");
        assert_eq!("/", current_task.fs().cwd().path_escaping_chroot());

        // Now bin exists again because we've gone back to the root.
        assert!(current_task.open_file(&mut locked, "bin".into(), OpenFlags::RDONLY).is_ok());

        // Repeating the .. doesn't do anything because we're already at the root.
        current_task
            .fs()
            .chdir(
                &current_task,
                current_task
                    .open_file(&mut locked, "..".into(), OpenFlags::RDONLY)
                    .expect("failed to open ..")
                    .name
                    .clone(),
            )
            .expect("Failed to chdir");
        assert_eq!("/", current_task.fs().cwd().path_escaping_chroot());
        assert!(current_task.open_file(&mut locked, "bin".into(), OpenFlags::RDONLY).is_ok());
    }
}
