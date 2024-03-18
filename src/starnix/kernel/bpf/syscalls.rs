// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use crate::{
    bpf::{
        fs::{get_bpf_object, get_selinux_context, BpfFsDir, BpfFsObject, BpfHandle},
        map::Map,
        program::{Program, ProgramInfo},
    },
    mm::{MemoryAccessor, MemoryAccessorExt},
    task::CurrentTask,
    vfs::{Anon, FdFlags, FdNumber, LookupContext, OutputBuffer, UserBuffersOutputBuffer},
};
use ebpf::MapSchema;
use smallvec::smallvec;
use starnix_logging::{log_trace, track_stub};
use starnix_sync::{Locked, Unlocked};
use starnix_syscalls::{SyscallResult, SUCCESS};
use starnix_uapi::{
    bpf_attr__bindgen_ty_1, bpf_attr__bindgen_ty_10, bpf_attr__bindgen_ty_12,
    bpf_attr__bindgen_ty_2, bpf_attr__bindgen_ty_4, bpf_attr__bindgen_ty_5, bpf_attr__bindgen_ty_9,
    bpf_cmd, bpf_cmd_BPF_BTF_GET_FD_BY_ID, bpf_cmd_BPF_BTF_GET_NEXT_ID, bpf_cmd_BPF_BTF_LOAD,
    bpf_cmd_BPF_ENABLE_STATS, bpf_cmd_BPF_ITER_CREATE, bpf_cmd_BPF_LINK_CREATE,
    bpf_cmd_BPF_LINK_DETACH, bpf_cmd_BPF_LINK_GET_FD_BY_ID, bpf_cmd_BPF_LINK_GET_NEXT_ID,
    bpf_cmd_BPF_LINK_UPDATE, bpf_cmd_BPF_MAP_CREATE, bpf_cmd_BPF_MAP_DELETE_BATCH,
    bpf_cmd_BPF_MAP_DELETE_ELEM, bpf_cmd_BPF_MAP_FREEZE, bpf_cmd_BPF_MAP_GET_FD_BY_ID,
    bpf_cmd_BPF_MAP_GET_NEXT_ID, bpf_cmd_BPF_MAP_GET_NEXT_KEY,
    bpf_cmd_BPF_MAP_LOOKUP_AND_DELETE_BATCH, bpf_cmd_BPF_MAP_LOOKUP_AND_DELETE_ELEM,
    bpf_cmd_BPF_MAP_LOOKUP_BATCH, bpf_cmd_BPF_MAP_LOOKUP_ELEM, bpf_cmd_BPF_MAP_UPDATE_BATCH,
    bpf_cmd_BPF_MAP_UPDATE_ELEM, bpf_cmd_BPF_OBJ_GET, bpf_cmd_BPF_OBJ_GET_INFO_BY_FD,
    bpf_cmd_BPF_OBJ_PIN, bpf_cmd_BPF_PROG_ATTACH, bpf_cmd_BPF_PROG_BIND_MAP,
    bpf_cmd_BPF_PROG_DETACH, bpf_cmd_BPF_PROG_GET_FD_BY_ID, bpf_cmd_BPF_PROG_GET_NEXT_ID,
    bpf_cmd_BPF_PROG_LOAD, bpf_cmd_BPF_PROG_QUERY, bpf_cmd_BPF_PROG_RUN,
    bpf_cmd_BPF_RAW_TRACEPOINT_OPEN, bpf_cmd_BPF_TASK_FD_QUERY, bpf_insn, bpf_map_info,
    bpf_map_type_BPF_MAP_TYPE_DEVMAP, bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH, bpf_prog_info, errno,
    error,
    errors::Errno,
    open_flags::OpenFlags,
    user_address::{UserAddress, UserCString, UserRef},
    user_buffer::UserBuffer,
    BPF_F_RDONLY_PROG, PATH_MAX,
};
use zerocopy::{AsBytes, FromBytes};

/// Read the arguments for a BPF command. The ABI works like this: If the arguments struct
/// passed is larger than the kernel knows about, the excess must be zeros. Similarly, if the
/// arguments struct is smaller than the kernel knows about, the kernel fills the excess with
/// zero.
fn read_attr<Attr: FromBytes>(
    current_task: &CurrentTask,
    attr_addr: UserAddress,
    attr_size: u32,
) -> Result<Attr, Errno> {
    let mut attr_size = attr_size as usize;
    let sizeof_attr = std::mem::size_of::<Attr>();

    // Verify that the extra is all zeros.
    if attr_size > sizeof_attr {
        let tail =
            current_task.read_memory_to_vec(attr_addr + sizeof_attr, attr_size - sizeof_attr)?;
        if tail.into_iter().any(|byte| byte != 0) {
            return error!(E2BIG);
        }

        attr_size = sizeof_attr;
    }

    // If the struct passed is smaller than our definition of the struct, let whatever is not
    // passed be zero.
    current_task.read_object_partial(UserRef::new(attr_addr), attr_size)
}

fn install_bpf_fd(
    current_task: &CurrentTask,
    obj: impl Into<BpfHandle>,
) -> Result<SyscallResult, Errno> {
    let handle: BpfHandle = obj.into();
    // All BPF FDs have the CLOEXEC flag turned on by default.
    let file = Anon::new_file(current_task, Box::new(handle), OpenFlags::CLOEXEC);
    Ok(current_task.add_file(file, FdFlags::CLOEXEC)?.into())
}

#[derive(Clone)]
pub struct BpfTypeFormat {
    #[allow(dead_code)]
    data: Vec<u8>,
}

pub fn sys_bpf(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    cmd: bpf_cmd,
    attr_addr: UserAddress,
    attr_size: u32,
) -> Result<SyscallResult, Errno> {
    // TODO(security): Implement the actual security semantics of BPF. This is commented out
    // because Android calls bpf from unprivileged processes.
    // if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
    //     return error!(EPERM);
    // }

    // The best available documentation on the various BPF commands is at
    // https://www.kernel.org/doc/html/latest/userspace-api/ebpf/syscall.html.
    // Comments on commands are copied from there.

    match cmd {
        // Create a map and return a file descriptor that refers to the map.
        bpf_cmd_BPF_MAP_CREATE => {
            let map_attr: bpf_attr__bindgen_ty_1 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_MAP_CREATE {:?}", map_attr);
            let schema = MapSchema {
                map_type: map_attr.map_type,
                key_size: map_attr.key_size,
                value_size: map_attr.value_size,
                max_entries: map_attr.max_entries,
            };
            let mut map = Map::new(schema, map_attr.map_flags)?;

            // To quote
            // https://cs.android.com/android/platform/superproject/+/master:system/bpf/libbpf_android/Loader.cpp;l=670;drc=28e295395471b33e662b7116378d15f1e88f0864
            // "DEVMAPs are readonly from the bpf program side's point of view, as such the kernel
            // in kernel/bpf/devmap.c dev_map_init_map() will set the flag"
            if schema.map_type == bpf_map_type_BPF_MAP_TYPE_DEVMAP
                || schema.map_type == bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH
            {
                map.flags |= BPF_F_RDONLY_PROG;
            }

            install_bpf_fd(current_task, map)
        }

        bpf_cmd_BPF_MAP_LOOKUP_ELEM => {
            let elem_attr: bpf_attr__bindgen_ty_2 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_MAP_LOOKUP_ELEM");
            let map_fd = FdNumber::from_raw(elem_attr.map_fd as i32);
            let map = get_bpf_object(current_task, map_fd)?;
            let map = map.as_map()?;

            let key = current_task.read_memory_to_vec(
                UserAddress::from(elem_attr.key),
                map.schema.key_size as usize,
            )?;
            // SAFETY: this union object was created with FromBytes so it's safe to access any
            // variant because all variants must be valid with all bit patterns.
            let user_value = UserAddress::from(unsafe { elem_attr.__bindgen_anon_1.value });
            map.lookup(locked, current_task, &key, user_value)?;
            Ok(SUCCESS)
        }

        // Create or update an element (key/value pair) in a specified map.
        bpf_cmd_BPF_MAP_UPDATE_ELEM => {
            let elem_attr: bpf_attr__bindgen_ty_2 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_MAP_UPDATE_ELEM");
            let map_fd = FdNumber::from_raw(elem_attr.map_fd as i32);
            let map = get_bpf_object(current_task, map_fd)?;
            let map = map.as_map()?;

            let flags = elem_attr.flags;
            let key = current_task.read_memory_to_vec(
                UserAddress::from(elem_attr.key),
                map.schema.key_size as usize,
            )?;
            // SAFETY: this union object was created with FromBytes so it's safe to access any
            // variant because all variants must be valid with all bit patterns.
            let user_value = UserAddress::from(unsafe { elem_attr.__bindgen_anon_1.value });
            let value =
                current_task.read_memory_to_vec(user_value, map.schema.value_size as usize)?;

            map.update(locked, key, &value, flags)?;
            Ok(SUCCESS)
        }

        bpf_cmd_BPF_MAP_DELETE_ELEM => {
            let elem_attr: bpf_attr__bindgen_ty_2 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_MAP_DELETE_ELEM");
            let map_fd = FdNumber::from_raw(elem_attr.map_fd as i32);
            let map = get_bpf_object(current_task, map_fd)?;
            let map = map.as_map()?;

            let key = current_task.read_memory_to_vec(
                UserAddress::from(elem_attr.key),
                map.schema.key_size as usize,
            )?;

            map.delete(locked, &key)?;
            Ok(SUCCESS)
        }

        // Look up an element by key in a specified map and return the key of the next element. Can
        // be used to iterate over all elements in the map.
        bpf_cmd_BPF_MAP_GET_NEXT_KEY => {
            let elem_attr: bpf_attr__bindgen_ty_2 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_MAP_GET_NEXT_KEY");
            let map_fd = FdNumber::from_raw(elem_attr.map_fd as i32);
            let map = get_bpf_object(current_task, map_fd)?;
            let map = map.as_map()?;
            let key = if elem_attr.key != 0 {
                let key = current_task.read_memory_to_vec(
                    UserAddress::from(elem_attr.key),
                    map.schema.key_size as usize,
                )?;
                Some(key)
            } else {
                None
            };
            // SAFETY: this union object was created with FromBytes so it's safe to access any
            // variant (right?)
            let user_next_key = UserAddress::from(unsafe { elem_attr.__bindgen_anon_1.next_key });
            map.get_next_key(locked, current_task, key, user_next_key)?;
            Ok(SUCCESS)
        }

        // Verify and load an eBPF program, returning a new file descriptor associated with the
        // program.
        bpf_cmd_BPF_PROG_LOAD => {
            let prog_attr: bpf_attr__bindgen_ty_4 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_PROG_LOAD");

            let info = ProgramInfo::from(&prog_attr);
            let user_code = UserRef::<bpf_insn>::new(UserAddress::from(prog_attr.insns));
            let code = current_task.read_objects_to_vec(user_code, prog_attr.insn_cnt as usize)?;

            let program = if current_task.kernel().features.bpf_v2 {
                let mut log_buffer = if prog_attr.log_buf != 0 && prog_attr.log_size > 1 {
                    UserBuffersOutputBuffer::unified_new(
                        current_task,
                        smallvec![UserBuffer {
                            address: prog_attr.log_buf.into(),
                            length: (prog_attr.log_size - 1) as usize
                        }],
                    )?
                } else {
                    UserBuffersOutputBuffer::unified_new(current_task, smallvec![])?
                };
                let result = Program::new(current_task, info, &mut log_buffer, code)?;
                // Ensures the log buffer ends with a 0.
                log_buffer.write(b"\0")?;
                result
            } else {
                // We pretend to succeed at loading the program in the basic version of bpf.
                // Eventually we'll be able to remove this stub when we can load bpf programs
                // accurately.
                Program::new_stub(info)
            };

            install_bpf_fd(current_task, program)
        }

        // Attach an eBPF program to a target_fd at the specified attach_type hook.
        bpf_cmd_BPF_PROG_ATTACH => {
            log_trace!("BPF_PROG_ATTACH");
            track_stub!(TODO("https://fxbug.dev/322874307"), "Bpf::BPF_PROG_ATTACH");
            Ok(SUCCESS)
        }

        // Obtain information about eBPF programs associated with the specified attach_type hook.
        bpf_cmd_BPF_PROG_QUERY => {
            let mut prog_attr: bpf_attr__bindgen_ty_10 =
                read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_PROG_QUERY");
            track_stub!(TODO("https://fxbug.dev/322873416"), "Bpf::BPF_PROG_QUERY");
            current_task.write_memory(UserAddress::from(prog_attr.prog_ids), 1.as_bytes())?;
            prog_attr.prog_cnt = std::mem::size_of::<u64>() as u32;
            current_task.write_memory(attr_addr, prog_attr.as_bytes())?;
            Ok(SUCCESS)
        }

        // Pin an eBPF program or map referred by the specified bpf_fd to the provided pathname on
        // the filesystem.
        bpf_cmd_BPF_OBJ_PIN => {
            let pin_attr: bpf_attr__bindgen_ty_5 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_OBJ_PIN {:?}", pin_attr);
            let bpf_fd = FdNumber::from_raw(pin_attr.bpf_fd as i32);
            let object = get_bpf_object(current_task, bpf_fd)?;
            let path_addr = UserCString::new(UserAddress::from(pin_attr.pathname));
            let pathname = current_task.read_c_string_to_vec(path_addr, PATH_MAX as usize)?;
            let (parent, basename) = current_task.lookup_parent_at(
                &mut LookupContext::default(),
                FdNumber::AT_FDCWD,
                pathname.as_ref(),
            )?;
            let bpf_dir =
                parent.entry.node.downcast_ops::<BpfFsDir>().ok_or_else(|| errno!(EINVAL))?;
            let selinux_context = get_selinux_context(pathname.as_ref());
            bpf_dir.register_pin(
                current_task,
                &parent,
                basename,
                object,
                selinux_context.as_ref(),
            )?;
            Ok(SUCCESS)
        }

        // Open a file descriptor for the eBPF object pinned to the specified pathname.
        bpf_cmd_BPF_OBJ_GET => {
            let path_attr: bpf_attr__bindgen_ty_5 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_OBJ_GET {:?}", path_attr);
            let path_addr = UserCString::new(UserAddress::from(path_attr.pathname));
            let pathname = current_task.read_c_string_to_vec(path_addr, PATH_MAX as usize)?;
            let node = current_task.lookup_path_from_root(pathname.as_ref())?;
            // TODO(tbodt): This might be the wrong error code, write a test program to find out
            let node =
                node.entry.node.downcast_ops::<BpfFsObject>().ok_or_else(|| errno!(EINVAL))?;
            install_bpf_fd(current_task, node.handle.clone())
        }

        // Obtain information about the eBPF object corresponding to bpf_fd.
        bpf_cmd_BPF_OBJ_GET_INFO_BY_FD => {
            let mut get_info_attr: bpf_attr__bindgen_ty_9 =
                read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_OBJ_GET_INFO_BY_FD {:?}", get_info_attr);
            let bpf_fd = FdNumber::from_raw(get_info_attr.bpf_fd as i32);
            let object = get_bpf_object(current_task, bpf_fd)?;

            let mut info = match object {
                BpfHandle::Map(map) => {
                    bpf_map_info {
                        type_: map.schema.map_type,
                        id: 0, // not used by android as far as I can tell
                        key_size: map.schema.key_size,
                        value_size: map.schema.value_size,
                        max_entries: map.schema.max_entries,
                        map_flags: map.flags,
                        ..Default::default()
                    }
                    .as_bytes()
                    .to_owned()
                }
                BpfHandle::Program(_) => {
                    #[allow(unknown_lints, clippy::unnecessary_struct_initialization)]
                    bpf_prog_info {
                        // Doesn't matter yet
                        ..Default::default()
                    }
                    .as_bytes()
                    .to_owned()
                }
                _ => {
                    return error!(EINVAL);
                }
            };

            // If info_len is larger than info, write out the full length of info and write the
            // smaller size into info_len. If info_len is smaller, truncate info.
            // TODO(tbodt): This is just a guess for the behavior. Works with BpfSyscallWrappers.h,
            // but could be wrong.
            info.truncate(get_info_attr.info_len as usize);
            get_info_attr.info_len = info.len() as u32;
            current_task.write_memory(UserAddress::from(get_info_attr.info), &info)?;
            current_task.write_memory(attr_addr, get_info_attr.as_bytes())?;
            Ok(SUCCESS)
        }

        // Verify and load BPF Type Format (BTF) metadata into the kernel, returning a new file
        // descriptor associated with the metadata. BTF is described in more detail at
        // https://www.kernel.org/doc/html/latest/bpf/btf.html.
        bpf_cmd_BPF_BTF_LOAD => {
            let btf_attr: bpf_attr__bindgen_ty_12 = read_attr(current_task, attr_addr, attr_size)?;
            log_trace!("BPF_BTF_LOAD {:?}", btf_attr);
            let data = current_task
                .read_memory_to_vec(UserAddress::from(btf_attr.btf), btf_attr.btf_size as usize)?;
            install_bpf_fd(current_task, BpfTypeFormat { data })
        }
        bpf_cmd_BPF_PROG_DETACH => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_PROG_DETACH");
            error!(EINVAL)
        }
        bpf_cmd_BPF_PROG_RUN => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_PROG_RUN");
            error!(EINVAL)
        }
        bpf_cmd_BPF_PROG_GET_NEXT_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_PROG_GET_NEXT_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_GET_NEXT_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_GET_NEXT_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_PROG_GET_FD_BY_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_PROG_GET_FD_BY_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_GET_FD_BY_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_GET_FD_BY_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_RAW_TRACEPOINT_OPEN => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_RAW_TRACEPOINT_OPEN");
            error!(EINVAL)
        }
        bpf_cmd_BPF_BTF_GET_FD_BY_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_BTF_GET_FD_BY_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_TASK_FD_QUERY => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_TASK_FD_QUERY");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_LOOKUP_AND_DELETE_ELEM => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_LOOKUP_AND_DELETE_ELEM");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_FREEZE => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_FREEZE");
            error!(EINVAL)
        }
        bpf_cmd_BPF_BTF_GET_NEXT_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_BTF_GET_NEXT_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_LOOKUP_BATCH => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_LOOKUP_BATCH");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_LOOKUP_AND_DELETE_BATCH => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_LOOKUP_AND_DELETE_BATCH");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_UPDATE_BATCH => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_UPDATE_BATCH");
            error!(EINVAL)
        }
        bpf_cmd_BPF_MAP_DELETE_BATCH => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_MAP_DELETE_BATCH");
            error!(EINVAL)
        }
        bpf_cmd_BPF_LINK_CREATE => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_LINK_CREATE");
            error!(EINVAL)
        }
        bpf_cmd_BPF_LINK_UPDATE => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_LINK_UPDATE");
            error!(EINVAL)
        }
        bpf_cmd_BPF_LINK_GET_FD_BY_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_LINK_GET_FD_BY_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_LINK_GET_NEXT_ID => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_LINK_GET_NEXT_ID");
            error!(EINVAL)
        }
        bpf_cmd_BPF_ENABLE_STATS => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_ENABLE_STATS");
            error!(EINVAL)
        }
        bpf_cmd_BPF_ITER_CREATE => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_ITER_CREATE");
            error!(EINVAL)
        }
        bpf_cmd_BPF_LINK_DETACH => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_LINK_DETACH");
            error!(EINVAL)
        }
        bpf_cmd_BPF_PROG_BIND_MAP => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "BPF_PROG_BIND_MAP");
            error!(EINVAL)
        }
        _ => {
            track_stub!(TODO("https://fxbug.dev/322874055"), "bpf", cmd);
            error!(EINVAL)
        }
    }
}
