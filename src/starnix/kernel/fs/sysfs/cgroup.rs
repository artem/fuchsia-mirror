// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file contains a very basic implementation of control groups.
//!
//! There is no support for actual resource constraints, or any operations outside of adding tasks
//! to a control group (for the duration of their lifetime).

use crate::{
    task::{CurrentTask, Task},
    vfs::{
        buffers::InputBuffer, fileops_impl_delegate_read_and_seek, fs_node_impl_not_dir,
        DynamicFile, DynamicFileBuf, DynamicFileSource, FileObject, FileOps, FsNode, FsNodeHandle,
        FsNodeInfo, FsNodeOps, FsStr, MemoryDirectoryFile,
    },
};
use starnix_lock::Mutex;
use starnix_uapi::{
    auth::FsCred, device_type::DeviceType, errno, error, errors::Errno, file_mode::FileMode,
    open_flags::OpenFlags, ownership::WeakRef, pid_t,
};
use std::sync::Arc;

type ControlGroupHandle = Arc<Mutex<ControlGroup>>;

struct ControlGroup {
    /// The tasks that are part of this control group.
    tasks: Vec<WeakRef<Task>>,
}

impl ControlGroup {
    fn new() -> ControlGroupHandle {
        Arc::new(Mutex::new(Self { tasks: vec![] }))
    }
}

/// A `CgroupDirectoryNode` represents the node associated with a particular control group. A
/// control group node may have other control groups as children, each of which will also be
/// represented as a `CgroupDirectoryNode`.
pub struct CgroupDirectoryNode {
    /// The control group associated with this directory node.
    control_group: ControlGroupHandle,
}

impl CgroupDirectoryNode {
    pub fn new() -> Self {
        Self { control_group: ControlGroup::new() }
    }
}

impl FsNodeOps for CgroupDirectoryNode {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MemoryDirectoryFile::new()))
    }

    fn mkdir(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        _name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        node.update_info(|info| {
            info.link_count += 1;
        });
        Ok(node.fs().create_node(
            current_task,
            CgroupDirectoryNode::new(),
            FsNodeInfo::new_factory(mode, owner),
        ))
    }

    fn mknod(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        _name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        // TODO(lindkvist): Handle files that are not `cgroup.procs`.
        let ops: Box<dyn FsNodeOps> = match mode.fmt() {
            FileMode::IFREG => Box::new(ControlGroupNode::new(self.control_group.clone())),
            _ => return error!(EACCES),
        };
        let node = node.fs().create_node(current_task, ops, |id| {
            let mut info = FsNodeInfo::new(id, mode, owner);
            info.rdev = dev;
            info
        });
        Ok(node)
    }

    fn unlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        error!(EPERM)
    }

    fn create_symlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EPERM)
    }
}

/// A `ControlGroupNode` backs the `cgroup.procs` file.
///
/// Opening and writing to this node will add tasks to the control group.
struct ControlGroupNode {
    control_group: ControlGroupHandle,
}

impl ControlGroupNode {
    fn new(control_group: ControlGroupHandle) -> Self {
        ControlGroupNode { control_group }
    }
}

impl FsNodeOps for ControlGroupNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(ControlGroupFile::new(self.control_group.clone())))
    }

    fn truncate(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        Ok(())
    }
}

/// A `ControlGroupFile` currently represents the `cgroup.procs` file for the control group. Writing
/// to this file will add tasks to the control group.
struct ControlGroupFileSource {
    control_group: ControlGroupHandle,
}
impl DynamicFileSource for ControlGroupFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let mut pids: Vec<pid_t> = vec![];
        self.control_group.lock().tasks.retain(|t| {
            if let Some(t) = t.upgrade() {
                pids.push(t.get_pid());
                true
            } else {
                // Filter out the tasks that have been dropped.
                false
            }
        });

        for pid in pids {
            write!(sink, "{pid}")?;
        }
        Ok(())
    }
}

pub struct ControlGroupFile {
    control_group: ControlGroupHandle,
    dynamic_file: DynamicFile<ControlGroupFileSource>,
}

impl ControlGroupFile {
    fn new(control_group: ControlGroupHandle) -> Self {
        Self {
            control_group: control_group.clone(),
            dynamic_file: DynamicFile::new(ControlGroupFileSource { control_group }),
        }
    }
}

impl FileOps for ControlGroupFile {
    fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;

        let pid_string = std::str::from_utf8(&bytes).map_err(|_| errno!(EINVAL))?;
        let pid = pid_string.parse::<pid_t>().map_err(|_| errno!(ENOENT))?;
        let weak_task = current_task.get_task(pid);
        let task = weak_task.upgrade().ok_or_else(|| errno!(EINVAL))?;

        // TODO(lindkvist): The task needs to be removed form any existing control group before
        // being added to a new one.
        self.control_group.lock().tasks.push(WeakRef::from(&task));

        Ok(bytes.len())
    }
}
