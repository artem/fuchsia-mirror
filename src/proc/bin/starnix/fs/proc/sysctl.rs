// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::*;
use crate::lock::Mutex;
use crate::task::*;
use crate::types::*;
use std::borrow::Cow;

pub fn sysctl_directory(fs: &FileSystemHandle) -> FsNodeHandle {
    let mode = mode!(IFREG, 0o644);
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(b"kernel", 0o555, |dir| {
        dir.entry(b"unprivileged_bpf_disable", StubSysctl::new_node(), mode);
        dir.entry(b"kptr_restrict", StubSysctl::new_node(), mode)
    });
    dir.subdir(b"net", 0o555, |dir| {
        dir.subdir(b"core", 0o555, |dir| {
            dir.entry(b"bpf_jit_enable", StubSysctl::new_node(), mode);
            dir.entry(b"bpf_jit_kallsyms", StubSysctl::new_node(), mode);
        });
    });
    dir.subdir(b"vm", 0o555, |dir| dir.entry(b"mmap_rnd_bits", StubSysctl::new_node(), mode));
    dir.build()
}

struct StubSysctl {
    data: Mutex<Vec<u8>>,
}

impl StubSysctl {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { data: Mutex::default() })
    }
}

impl BytesFileOps for StubSysctl {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        *self.data.lock() = data;
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.data.lock().clone().into())
    }
}
