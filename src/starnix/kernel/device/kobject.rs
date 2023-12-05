// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_lock::Mutex;
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use crate::{
    device::DeviceMode,
    fs::sysfs::SysFsDirectory,
    task::CurrentTask,
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_seekable, fs_node_impl_not_dir, FileObject, FileOps, FsNode, FsNodeOps, FsStr,
        FsString, PathBuilder,
    },
};
use starnix_uapi::{device_type::DeviceType, error, errors::Errno, open_flags::OpenFlags};

/// ktype is the type of object that embeds a `kobject`.
#[derive(Debug, PartialEq)]
pub enum KType {
    /// A collection of kobjects.
    Collection,
    /// A bus which devices can be attached to.
    Bus,
    /// A group of devices that have a smilar behavior.
    Class,
    /// A virtual/physical device that is attached to a bus.
    ///
    /// Contains all information of a device node.
    Device(DeviceMetadata),
    #[cfg(test)]
    /// A type used for testing.
    Test,
}

#[derive(Clone, Debug)]
pub struct DeviceMetadata {
    pub class: Weak<KObject>,
    /// Name of the device in /dev.
    pub name: FsString,
    pub device_type: DeviceType,
    pub mode: DeviceMode,
}

impl DeviceMetadata {
    pub fn new(
        class: &KObjectHandle,
        name: &FsStr,
        device_type: DeviceType,
        mode: DeviceMode,
    ) -> Self {
        Self { class: Arc::downgrade(class), name: name.to_vec(), device_type, mode }
    }
}

impl PartialEq for DeviceMetadata {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.device_type == other.device_type
    }
}

/// Attributes that are used to create a KType::Device kobject.
#[derive(Clone, Debug)]
pub struct KObjectDeviceAttribute {
    /// Class kobject that the device belongs to.
    pub class: KObjectHandle,
    /// Name in /sys.
    pub name: FsString,
    pub device: DeviceMetadata,
}

impl KObjectDeviceAttribute {
    pub fn new(
        class: KObjectHandle,
        kobject_name: &FsStr,
        device_name: &FsStr,
        device_type: DeviceType,
        mode: DeviceMode,
    ) -> Self {
        match class.ktype() {
            KType::Class => Self {
                device: DeviceMetadata::new(&class, device_name, device_type, mode),
                class,
                name: kobject_name.to_vec(),
            },
            _ => {
                panic!("Invalid class kobject")
            }
        }
    }
}

/// A kobject is the fundamental unit of the sysfs /devices subsystem. Each kobject represents a
/// sysfs object.
///
/// A kobject has a name, a `ktype`, pointers to its children, and a pointer to its
/// parent, which allows it to be organized into hierarchies.
pub struct KObject {
    /// The name that will appear in sysfs.
    ///
    /// It is also used by the parent to find this child. This name will be reflected in the full
    /// path from the root.
    name: FsString,

    /// The weak reference to its parent kobject.
    parent: Option<Weak<KObject>>,

    /// A collection of the children of this kobject.
    ///
    /// The kobject tree has strong references from parent-to-child and weak
    /// references from child-to-parent. This will avoid reference cycle.
    children: Mutex<BTreeMap<FsString, KObjectHandle>>,

    /// The type of object that embeds a kobject.
    ///
    /// It controls what happens to the kobject when being created and destroyed.
    ktype: KType,

    /// Function to create the associated `FsNodeOps`.
    create_fs_node_ops: CreateFsNodeOps,
}
pub type KObjectHandle = Arc<KObject>;

type CreateFsNodeOps = Box<dyn Fn(Weak<KObject>) -> Box<dyn FsNodeOps> + Send + Sync>;

impl KObject {
    pub fn new_root() -> KObjectHandle {
        Arc::new(Self {
            name: Default::default(),
            parent: None,
            children: Default::default(),
            ktype: KType::Collection,
            create_fs_node_ops: Box::new(|kobject| Box::new(SysFsDirectory::new(kobject))),
        })
    }

    fn new<F, N>(
        name: &FsStr,
        parent: KObjectHandle,
        ktype: KType,
        create_fs_node_ops: F,
    ) -> KObjectHandle
    where
        F: Fn(Weak<KObject>) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        Arc::new(Self {
            name: name.to_vec(),
            parent: Some(Arc::downgrade(&parent)),
            children: Default::default(),
            ktype,
            create_fs_node_ops: Box::new(move |kobject| Box::new(create_fs_node_ops(kobject))),
        })
    }

    /// The name that will appear in sysfs.
    pub fn name(&self) -> FsString {
        self.name.clone()
    }

    /// The parent kobject.
    ///
    /// Returns none if this kobject is the root.
    pub fn parent(&self) -> Option<KObjectHandle> {
        self.parent.clone().and_then(|parent| Weak::upgrade(&parent))
    }

    /// The type of object that embeds a kobject.
    pub fn ktype(&self) -> &KType {
        &self.ktype
    }

    /// Returns the associated `FsNodeOps`.
    ///
    /// The `create_fs_node_ops` function will be called with a weak pointer to kobject itself.
    pub fn ops(self: &KObjectHandle) -> Box<dyn FsNodeOps> {
        self.create_fs_node_ops.as_ref()(Arc::downgrade(self))
    }

    /// Get the full path from the root.
    pub fn path(self: &KObjectHandle) -> FsString {
        let mut current = Some(self.clone());
        let mut path = PathBuilder::new();
        while let Some(n) = current {
            path.prepend_element(&n.name());
            current = n.parent();
        }

        // Dedup root directory slash.
        path.build()[1..].to_vec()
    }

    /// Get the relative path to the root.
    pub fn path_to_root(self: &KObjectHandle) -> FsString {
        let mut parent = self.parent();
        let mut path = PathBuilder::new();
        while let Some(n) = parent {
            path.prepend_element(b"..");
            parent = n.parent();
        }

        // Get the relative path.
        path.build()[1..].to_vec()
    }

    /// Checks if there is any child holding the `name`.
    pub fn has_child(self: &KObjectHandle, name: &FsStr) -> bool {
        self.get_child(name).is_some()
    }

    /// Get the child based on the name.
    pub fn get_child(self: &KObjectHandle, name: &FsStr) -> Option<KObjectHandle> {
        self.children.lock().get(name).cloned()
    }

    /// Gets the child if exists, creates a new child if not.
    pub fn get_or_create_child<F, N>(
        self: &KObjectHandle,
        name: &FsStr,
        ktype: KType,
        create_fs_node_ops: F,
    ) -> KObjectHandle
    where
        F: Fn(Weak<KObject>) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        let mut children = self.children.lock();
        match children.get(name).cloned() {
            Some(child) => child,
            None => {
                let child = KObject::new(name, self.clone(), ktype, create_fs_node_ops);
                children.insert(name.to_vec(), child.clone());
                child
            }
        }
    }

    pub fn insert_child(self: &KObjectHandle, child: KObjectHandle) {
        self.children.lock().insert(child.name(), child);
    }

    /// Collects all children names.
    pub fn get_children_names(&self) -> Vec<FsString> {
        self.children.lock().keys().cloned().collect()
    }

    pub fn get_children_kobjects(&self) -> Vec<KObjectHandle> {
        self.children.lock().values().cloned().collect::<Vec<KObjectHandle>>()
    }

    /// Removes the child if exists.
    pub fn remove_child(self: &KObjectHandle, name: &FsStr) -> Option<(FsString, KObjectHandle)> {
        self.children.lock().remove_entry(name)
    }

    /// Removes itself from the parent kobject.
    pub fn remove(&self) {
        if let Some(parent) = self.parent() {
            parent.remove_child(&self.name());
        }
    }
}

impl std::fmt::Debug for KObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KObject")
            .field("name", &String::from_utf8_lossy(&self.name()))
            .field("ktype", &self.ktype())
            .finish()
    }
}

pub struct UEventFsNode {
    kobject: KObjectHandle,
}

impl UEventFsNode {
    pub fn new(kobject: KObjectHandle) -> Self {
        Self { kobject }
    }
}

impl FsNodeOps for UEventFsNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(UEventFile::new(self.kobject.clone())))
    }
}

struct UEventFile {
    kobject: KObjectHandle,
}

impl UEventFile {
    pub fn new(kobject: KObjectHandle) -> Self {
        Self { kobject }
    }

    fn parse_commands(data: &[u8]) -> Vec<&[u8]> {
        data.split(|&c| c == b'\0' || c == b'\n').collect()
    }
}

impl FileOps for UEventFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        match self.kobject.ktype() {
            KType::Device(device) => {
                let content = format!(
                    "MAJOR={}\nMINOR={}\nDEVNAME={}\n",
                    device.device_type.major(),
                    device.device_type.minor(),
                    String::from_utf8_lossy(&device.name),
                );
                data.write(content[offset..].as_bytes())
            }
            _ => error!(ENODEV),
        }
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if offset != 0 {
            return error!(EINVAL);
        }
        // TODO(fxb/127713): Support parsing synthetic variables.
        let content = data.read_all()?;
        for command in Self::parse_commands(&content) {
            // Ignore empty lines.
            if command == b"" {
                continue;
            }

            current_task
                .kernel()
                .device_registry
                .dispatch_uevent(command.try_into()?, self.kobject.clone());
        }
        Ok(content.len())
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum UEventAction {
    Add,
    Remove,
    Change,
    Move,
    Online,
    Offline,
    Bind,
    Unbind,
}

impl std::fmt::Display for UEventAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UEventAction::Add => write!(f, "add"),
            UEventAction::Remove => write!(f, "remove"),
            UEventAction::Change => write!(f, "change"),
            UEventAction::Move => write!(f, "move"),
            UEventAction::Online => write!(f, "online"),
            UEventAction::Offline => write!(f, "offline"),
            UEventAction::Bind => write!(f, "bind"),
            UEventAction::Unbind => write!(f, "unbind"),
        }
    }
}

impl TryFrom<&[u8]> for UEventAction {
    type Error = Errno;

    fn try_from(action: &[u8]) -> Result<Self, Self::Error> {
        match action {
            b"add" => Ok(UEventAction::Add),
            b"remove" => Ok(UEventAction::Remove),
            b"change" => Ok(UEventAction::Change),
            b"move" => Ok(UEventAction::Move),
            b"online" => Ok(UEventAction::Online),
            b"offline" => Ok(UEventAction::Offline),
            b"bind" => Ok(UEventAction::Bind),
            b"unbind" => Ok(UEventAction::Unbind),
            _ => error!(EINVAL),
        }
    }
}

#[derive(Copy, Clone)]
pub struct UEventContext {
    pub seqnum: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::sysfs::DeviceDirectory;

    #[::fuchsia::test]
    fn kobject_create_child() {
        let root = KObject::new_root();
        assert!(root.parent().is_none());

        assert!(!root.has_child(b"virtual"));
        root.get_or_create_child(b"virtual", KType::Test, SysFsDirectory::new);
        assert!(root.has_child(b"virtual"));
    }

    #[::fuchsia::test]
    fn kobject_path() {
        let root = KObject::new_root();
        let bus = root.get_or_create_child(b"virtual", KType::Test, SysFsDirectory::new);
        let device = bus
            .get_or_create_child(b"mem", KType::Test, SysFsDirectory::new)
            .get_or_create_child(b"null", KType::Test, DeviceDirectory::new);
        assert_eq!(device.path(), b"/virtual/mem/null".to_vec());
    }

    #[::fuchsia::test]
    fn kobject_path_to_root() {
        let root = KObject::new_root();
        let bus = root.get_or_create_child(b"bus", KType::Test, SysFsDirectory::new);
        let device = bus.get_or_create_child(b"device", KType::Test, SysFsDirectory::new);
        assert_eq!(device.path_to_root(), b"../..".to_vec());
    }

    #[::fuchsia::test]
    fn kobject_get_children_names() {
        let root = KObject::new_root();
        root.get_or_create_child(b"virtual", KType::Test, SysFsDirectory::new);
        root.get_or_create_child(b"cpu", KType::Test, SysFsDirectory::new);
        root.get_or_create_child(b"power", KType::Test, SysFsDirectory::new);

        let names = root.get_children_names();
        assert!(names.iter().any(|name| *name == b"virtual".to_vec()));
        assert!(names.iter().any(|name| *name == b"cpu".to_vec()));
        assert!(names.iter().any(|name| *name == b"power".to_vec()));
        assert!(!names.iter().any(|name| *name == b"system".to_vec()));
    }

    #[::fuchsia::test]
    fn kobject_remove() {
        let root = KObject::new_root();
        let bus = root.get_or_create_child(b"virtual", KType::Test, SysFsDirectory::new);
        let class = bus.get_or_create_child(b"mem", KType::Test, SysFsDirectory::new);
        assert!(bus.has_child(b"mem"));
        class.remove();
        assert!(!bus.has_child(b"mem"));
    }
}
