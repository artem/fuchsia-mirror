// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::{
        vmo::round_up_to_system_page_size, DesiredAddress, MappingName, MappingOptions,
        MemoryAccessor, MemoryManager, ProtectionFlags, PAGE_SIZE, VMEX_RESOURCE,
    },
    selinux::hooks::thread_group_hooks::SeLinuxResolvedElfState,
    task::CurrentTask,
    vfs::{FileHandle, FileWriteGuardMode, FileWriteGuardRef},
};
use fuchsia_zircon::{
    HandleBased, {self as zx},
};
use process_builder::{elf_load, elf_parse};
use starnix_logging::{log_error, log_warn};
use starnix_uapi::{
    errno, error, errors::Errno, from_status_like_fdio, open_flags::OpenFlags,
    time::SCHEDULER_CLOCK_HZ, user_address::UserAddress, AT_BASE, AT_CLKTCK, AT_EGID, AT_ENTRY,
    AT_EUID, AT_EXECFN, AT_GID, AT_NULL, AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM, AT_RANDOM,
    AT_SECURE, AT_SYSINFO_EHDR, AT_UID,
};
use std::{
    ffi::{CStr, CString},
    mem::size_of,
    ops::Deref as _,
    sync::Arc,
};

#[derive(Debug)]
struct StackResult {
    stack_pointer: UserAddress,
    auxv_start: UserAddress,
    auxv_end: UserAddress,
    argv_start: UserAddress,
    argv_end: UserAddress,
    environ_start: UserAddress,
    environ_end: UserAddress,
}

const RANDOM_SEED_BYTES: usize = 16;

fn get_initial_stack_size(
    path: &CStr,
    argv: &Vec<CString>,
    environ: &Vec<CString>,
    auxv: &Vec<(u32, u64)>,
) -> usize {
    argv.iter().map(|x| x.as_bytes_with_nul().len()).sum::<usize>()
        + environ.iter().map(|x| x.as_bytes_with_nul().len()).sum::<usize>()
        + path.to_bytes_with_nul().len()
        + RANDOM_SEED_BYTES
        + (argv.len() + 1 + environ.len() + 1) * size_of::<*const u8>()
        + auxv.len() * 2 * size_of::<u64>()
}

fn populate_initial_stack(
    ma: &impl MemoryAccessor,
    path: &CStr,
    argv: &Vec<CString>,
    environ: &Vec<CString>,
    mut auxv: Vec<(u32, u64)>,
    original_stack_start_addr: UserAddress,
) -> Result<StackResult, Errno> {
    let mut stack_pointer = original_stack_start_addr;
    let write_stack = |data: &[u8], addr: UserAddress| ma.write_memory(addr, data);

    let argv_end = stack_pointer;
    for arg in argv.iter().rev() {
        stack_pointer -= arg.as_bytes_with_nul().len();
        write_stack(arg.as_bytes_with_nul(), stack_pointer)?;
    }
    let argv_start = stack_pointer;

    let environ_end = stack_pointer;
    for env in environ.iter().rev() {
        stack_pointer -= env.as_bytes_with_nul().len();
        write_stack(env.as_bytes_with_nul(), stack_pointer)?;
    }
    let environ_start = stack_pointer;

    // Write the path used with execve.
    stack_pointer -= path.to_bytes_with_nul().len();
    let execfn_addr = stack_pointer;
    write_stack(path.to_bytes_with_nul(), execfn_addr)?;

    let mut random_seed = [0; RANDOM_SEED_BYTES];
    zx::cprng_draw(&mut random_seed);
    stack_pointer -= random_seed.len();
    let random_seed_addr = stack_pointer;
    write_stack(&random_seed, random_seed_addr)?;

    auxv.push((AT_EXECFN, execfn_addr.ptr() as u64));
    auxv.push((AT_RANDOM, random_seed_addr.ptr() as u64));
    auxv.push((AT_NULL, 0));

    // After the remainder (argc/argv/environ/auxv) is pushed, the stack pointer must be 16 byte
    // aligned. This is required by the ABI and assumed by the compiler to correctly align SSE
    // operations. But this can't be done after it's pushed, since it has to be right at the top of
    // the stack. So we collect it all, align the stack appropriately now that we know the size,
    // and push it all at once.
    let mut main_data = vec![];
    // argc
    let argc: u64 = argv.len() as u64;
    main_data.extend_from_slice(&argc.to_ne_bytes());
    // argv
    const ZERO: [u8; 8] = [0; 8];
    let mut next_arg_addr = argv_start;
    for arg in argv {
        main_data.extend_from_slice(&next_arg_addr.ptr().to_ne_bytes());
        next_arg_addr += arg.as_bytes_with_nul().len();
    }
    main_data.extend_from_slice(&ZERO);
    // environ
    let mut next_env_addr = environ_start;
    for env in environ {
        main_data.extend_from_slice(&next_env_addr.ptr().to_ne_bytes());
        next_env_addr += env.as_bytes_with_nul().len();
    }
    main_data.extend_from_slice(&ZERO);
    // auxv
    let auxv_start_offset = main_data.len();
    for (tag, val) in auxv {
        main_data.extend_from_slice(&(tag as u64).to_ne_bytes());
        main_data.extend_from_slice(&val.to_ne_bytes());
    }
    let auxv_end_offset = main_data.len();

    // Time to push.
    stack_pointer -= main_data.len();
    stack_pointer -= stack_pointer.ptr() % 16;
    write_stack(main_data.as_slice(), stack_pointer)?;
    let auxv_start = stack_pointer + auxv_start_offset;
    let auxv_end = stack_pointer + auxv_end_offset;

    Ok(StackResult {
        stack_pointer,
        auxv_start,
        auxv_end,
        argv_start,
        argv_end,
        environ_start,
        environ_end,
    })
}

struct LoadedElf {
    headers: elf_parse::Elf64Headers,
    file_base: usize,
    vaddr_bias: usize,
}

// TODO: Improve the error reporting produced by this function by mapping ElfParseError to Errno more precisely.
fn elf_parse_error_to_errno(err: elf_parse::ElfParseError) -> Errno {
    log_warn!("elf parse error: {:?}", err);
    errno!(ENOEXEC)
}

// TODO: Improve the error reporting produced by this function by mapping ElfLoadError to Errno more precisely.
fn elf_load_error_to_errno(err: elf_load::ElfLoadError) -> Errno {
    log_warn!("elf load error: {:?}", err);
    errno!(EINVAL)
}

struct Mapper<'a> {
    file: &'a FileHandle,
    mm: &'a MemoryManager,
    file_write_guard: FileWriteGuardRef,
}
impl elf_load::Mapper for Mapper<'_> {
    fn map(
        &self,
        vmar_offset: usize,
        vmo: &zx::Vmo,
        vmo_offset: u64,
        length: usize,
        vmar_flags: zx::VmarFlags,
    ) -> Result<usize, zx::Status> {
        let vmo = Arc::new(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?);
        self.mm
            .map_vmo(
                DesiredAddress::Fixed(self.mm.base_addr.checked_add(vmar_offset).ok_or_else(
                    || {
                        log_error!(
                            "in elf load, addition overflow attempting to map at {:?} + {:#x}",
                            self.mm.base_addr,
                            vmar_offset
                        );
                        zx::Status::INVALID_ARGS
                    },
                )?),
                vmo,
                vmo_offset,
                length,
                ProtectionFlags::from_vmar_flags(vmar_flags),
                MappingOptions::ELF_BINARY,
                MappingName::File(self.file.name.clone()),
                self.file_write_guard.clone(),
            )
            .map_err(|e| {
                // TODO: Find a way to propagate this errno to the caller.
                log_error!("elf map error: {:?}", e);
                zx::Status::INVALID_ARGS
            })
            .map(|addr| addr.ptr())
    }
}

fn load_elf(
    elf: FileHandle,
    elf_vmo: Arc<zx::Vmo>,
    mm: &MemoryManager,
    file_write_guard: FileWriteGuardRef,
) -> Result<LoadedElf, Errno> {
    let headers = elf_parse::Elf64Headers::from_vmo(&elf_vmo).map_err(elf_parse_error_to_errno)?;
    let elf_info = elf_load::loaded_elf_info(&headers);
    let file_base = match headers.file_header().elf_type() {
        Ok(elf_parse::ElfType::SharedObject) => {
            mm.get_random_base(elf_info.high - elf_info.low).ptr()
        }
        Ok(elf_parse::ElfType::Executable) => elf_info.low,
        _ => return error!(EINVAL),
    };
    let vaddr_bias = file_base.wrapping_sub(elf_info.low);
    let mapper = Mapper { file: &elf, mm, file_write_guard };
    elf_load::map_elf_segments(&elf_vmo, &headers, &mapper, mm.base_addr.ptr(), vaddr_bias)
        .map_err(elf_load_error_to_errno)?;
    Ok(LoadedElf { headers, file_base, vaddr_bias })
}

pub struct ThreadStartInfo {
    pub entry: UserAddress,
    pub stack: UserAddress,
}

/// Holds a resolved ELF VMO and associated parameters necessary for an execve call.
pub struct ResolvedElf {
    /// A file handle to the resolved ELF executable.
    pub file: FileHandle,
    /// A VMO to the resolved ELF executable.
    pub vmo: Arc<zx::Vmo>,
    /// An ELF interpreter, if specified in the ELF executable header.
    pub interp: Option<ResolvedInterpElf>,
    /// Arguments to be passed to the new process.
    pub argv: Vec<CString>,
    /// The environment to initialize for the new process.
    pub environ: Vec<CString>,
    /// The SELinux state for the new process. None if SELinux is disabled.
    pub selinux_state: Option<SeLinuxResolvedElfState>,
    /// Exec/write lock.
    pub file_write_guard: FileWriteGuardRef,
}

/// Holds a resolved ELF interpreter VMO.
pub struct ResolvedInterpElf {
    /// A file handle to the resolved ELF interpreter.
    file: FileHandle,
    /// A VMO to the resolved ELF interpreter.
    vmo: Arc<zx::Vmo>,
    /// Exec/write lock.
    file_write_guard: FileWriteGuardRef,
}

// The magic bytes of a script file.
const HASH_BANG: &[u8; 2] = b"#!";
const MAX_RECURSION_DEPTH: usize = 5;

/// Resolves a file into a validated executable ELF, following script interpreters to a fixed
/// recursion depth. `argv` may change due to script interpreter logic.
pub fn resolve_executable(
    current_task: &CurrentTask,
    file: FileHandle,
    path: CString,
    argv: Vec<CString>,
    environ: Vec<CString>,
    selinux_state: Option<SeLinuxResolvedElfState>,
) -> Result<ResolvedElf, Errno> {
    resolve_executable_impl(current_task, file, path, argv, environ, 0, selinux_state)
}

/// Resolves a file into a validated executable ELF, following script interpreters to a fixed
/// recursion depth.
fn resolve_executable_impl(
    current_task: &CurrentTask,
    file: FileHandle,
    path: CString,
    argv: Vec<CString>,
    environ: Vec<CString>,
    recursion_depth: usize,
    selinux_state: Option<SeLinuxResolvedElfState>,
) -> Result<ResolvedElf, Errno> {
    if recursion_depth > MAX_RECURSION_DEPTH {
        return error!(ELOOP);
    }
    let vmo = file.get_vmo(current_task, None, ProtectionFlags::READ | ProtectionFlags::EXEC)?;
    let mut header = [0u8; 2];
    match vmo.read(&mut header, 0) {
        Ok(()) => {}
        Err(zx::Status::OUT_OF_RANGE) => {
            // The file is empty, or it would have at least one page allocated to it.
            return error!(ENOEXEC);
        }
        Err(_) => return error!(EINVAL),
    }
    if &header == HASH_BANG {
        resolve_script(current_task, vmo, path, argv, environ, recursion_depth, selinux_state)
    } else {
        resolve_elf(current_task, file, vmo, argv, environ, selinux_state)
    }
}

/// Resolves a #! script file into a validated executable ELF.
fn resolve_script(
    current_task: &CurrentTask,
    vmo: Arc<zx::Vmo>,
    path: CString,
    argv: Vec<CString>,
    environ: Vec<CString>,
    recursion_depth: usize,
    selinux_state: Option<SeLinuxResolvedElfState>,
) -> Result<ResolvedElf, Errno> {
    // All VMOs have sizes in multiple of the system page size, so as long as we only read a page or
    // less, we should never read past the end of the VMO.
    // Since Linux 5.1, the max length of the interpreter following the #! is 255.
    const HEADER_BUFFER_CAP: usize = 255 + HASH_BANG.len();
    let mut buffer = [0u8; HEADER_BUFFER_CAP];
    vmo.read(&mut buffer, 0).map_err(|_| errno!(EINVAL))?;

    let mut args = parse_interpreter_line(&buffer)?;
    let interpreter = current_task.open_file(args[0].as_bytes().into(), OpenFlags::RDONLY)?;

    // Append the original script executable path as an argument.
    args.push(path);

    // Append the original arguments (minus argv[0]).
    args.extend(argv.into_iter().skip(1));

    // Recurse and resolve the interpreter executable
    resolve_executable_impl(
        current_task,
        interpreter,
        args[0].clone(),
        args,
        environ,
        recursion_depth + 1,
        selinux_state,
    )
}

/// Parses a "#!" byte string and extracts CString arguments. The byte string must contain an
/// ASCII newline character or null-byte, or else it is considered truncated and parsing will fail.
/// If the byte string is empty or contains only whitespace, parsing fails.
/// If successful, the returned `Vec` will have at least one element (the interpreter path).
fn parse_interpreter_line(line: &[u8]) -> Result<Vec<CString>, Errno> {
    // Assuming the byte string starts with "#!", truncate the input to end at the first newline or
    // null-byte. If not found, assume the input was truncated and fail parsing.
    let end = line.iter().position(|&b| b == b'\n' || b == 0).ok_or_else(|| errno!(EINVAL))?;
    let line = &line[HASH_BANG.len()..end];

    // Skip whitespace at the start.
    let is_tab_or_space = |&b| b == b' ' || b == b'\t';
    let begin = line.iter().position(|b| !is_tab_or_space(b)).unwrap_or(0);
    let line = &line[begin..];

    // Split the byte string at the first whitespace character (or end of line). The first part
    // is the interpreter path.
    let first_whitespace = line.iter().position(is_tab_or_space).unwrap_or(line.len());
    let (interpreter, rest) = line.split_at(first_whitespace);
    if interpreter.is_empty() {
        return error!(ENOEXEC);
    }

    // The second part is the optional argument. Trim the leading and trailing whitespace, but
    // treat the middle as a single argument, even if whitespace is encountered.
    let begin = rest.iter().position(|b| !is_tab_or_space(b)).unwrap_or(rest.len());
    let end = rest.iter().rposition(|b| !is_tab_or_space(b)).map(|b| b + 1).unwrap_or(rest.len());
    let optional_arg = &rest[begin..end];

    // SAFETY: `CString::new` can only fail if it encounters a null-byte, which we've made sure
    // the input won't have.
    Ok(if optional_arg.is_empty() {
        vec![CString::new(interpreter).unwrap()]
    } else {
        vec![CString::new(interpreter).unwrap(), CString::new(optional_arg).unwrap()]
    })
}

/// Resolves a file handle into a validated executable ELF.
fn resolve_elf(
    current_task: &CurrentTask,
    file: FileHandle,
    vmo: Arc<zx::Vmo>,
    argv: Vec<CString>,
    environ: Vec<CString>,
    selinux_state: Option<SeLinuxResolvedElfState>,
) -> Result<ResolvedElf, Errno> {
    let elf_headers = elf_parse::Elf64Headers::from_vmo(&vmo).map_err(elf_parse_error_to_errno)?;
    let interp = if let Some(interp_hdr) = elf_headers
        .program_header_with_type(elf_parse::SegmentType::Interp)
        .map_err(|_| errno!(EINVAL))?
    {
        // The ELF header specified an ELF interpreter.
        // Read the path and load this ELF as well.
        let mut interp = vec![0; interp_hdr.filesz as usize];
        vmo.read(&mut interp, interp_hdr.offset as u64)
            .map_err(|status| from_status_like_fdio!(status))?;
        let interp = CStr::from_bytes_until_nul(&interp).map_err(|_| errno!(EINVAL))?;
        let interp_file = current_task.open_file(interp.to_bytes().into(), OpenFlags::RDONLY)?;
        let interp_vmo = interp_file.get_vmo(
            current_task,
            None,
            ProtectionFlags::READ | ProtectionFlags::EXEC,
        )?;
        let file_write_guard =
            interp_file.name.entry.node.create_write_guard(FileWriteGuardMode::Exec)?.into_ref();
        Some(ResolvedInterpElf { file: interp_file, vmo: interp_vmo, file_write_guard })
    } else {
        None
    };
    let file_write_guard =
        file.name.entry.node.create_write_guard(FileWriteGuardMode::Exec)?.into_ref();
    Ok(ResolvedElf { file, vmo, interp, argv, environ, selinux_state, file_write_guard })
}

/// Loads a resolved ELF into memory, along with an interpreter if one is defined, and initializes
/// the stack.
pub fn load_executable(
    current_task: &CurrentTask,
    resolved_elf: ResolvedElf,
    original_path: &CStr,
) -> Result<ThreadStartInfo, Errno> {
    let main_elf = load_elf(
        resolved_elf.file,
        resolved_elf.vmo,
        current_task.mm(),
        resolved_elf.file_write_guard,
    )?;
    let interp_elf = resolved_elf
        .interp
        .map(|interp| load_elf(interp.file, interp.vmo, current_task.mm(), interp.file_write_guard))
        .transpose()?;

    let entry_elf = interp_elf.as_ref().unwrap_or(&main_elf);
    let entry = UserAddress::from_ptr(
        entry_elf.headers.file_header().entry.wrapping_add(entry_elf.vaddr_bias),
    );

    let vdso_vmo = &current_task.kernel().vdso.vmo;
    let vvar_vmo = current_task.kernel().vdso.vvar_readonly.clone();

    let vdso_size = vdso_vmo.get_size().map_err(|_| errno!(EINVAL))?;
    const VDSO_PROT_FLAGS: ProtectionFlags = ProtectionFlags::READ.union(ProtectionFlags::EXEC);

    let vvar_size = vvar_vmo.get_size().map_err(|_| errno!(EINVAL))?;
    const VVAR_PROT_FLAGS: ProtectionFlags = ProtectionFlags::READ;

    // Create a private clone of the starnix kernel vDSO
    let vdso_clone = vdso_vmo
        .create_child(zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE, 0, vdso_size)
        .map_err(|status| from_status_like_fdio!(status))?;

    let vdso_executable = vdso_clone
        .replace_as_executable(&VMEX_RESOURCE)
        .map_err(|status| from_status_like_fdio!(status))?;

    // Memory map the vvar vmo, mapping a space the size of (size of vvar + size of vDSO)
    let vvar_map_result = current_task.mm().map_vmo(
        DesiredAddress::Any,
        vvar_vmo,
        0,
        (vvar_size as usize) + (vdso_size as usize),
        VVAR_PROT_FLAGS,
        MappingOptions::empty(),
        MappingName::Vvar,
        FileWriteGuardRef(None),
    )?;

    // Overwrite the second part of the vvar mapping to contain the vDSO clone
    let vdso_base = current_task.mm().map_vmo(
        DesiredAddress::FixedOverwrite(vvar_map_result + vvar_size),
        Arc::new(vdso_executable),
        0,
        vdso_size as usize,
        VDSO_PROT_FLAGS,
        MappingOptions::DONT_SPLIT,
        MappingName::Vdso,
        FileWriteGuardRef(None),
    )?;

    let auxv = {
        let creds = current_task.creds();
        vec![
            (AT_UID, creds.uid as u64),
            (AT_EUID, creds.euid as u64),
            (AT_GID, creds.gid as u64),
            (AT_EGID, creds.egid as u64),
            (AT_BASE, interp_elf.map_or(0, |interp| interp.file_base as u64)),
            (AT_PAGESZ, *PAGE_SIZE),
            (AT_PHDR, main_elf.file_base.wrapping_add(main_elf.headers.file_header().phoff) as u64),
            (AT_PHENT, main_elf.headers.file_header().phentsize as u64),
            (AT_PHNUM, main_elf.headers.file_header().phnum as u64),
            (
                AT_ENTRY,
                main_elf.vaddr_bias.wrapping_add(main_elf.headers.file_header().entry) as u64,
            ),
            (AT_CLKTCK, SCHEDULER_CLOCK_HZ as u64),
            (AT_SYSINFO_EHDR, vdso_base.into()),
            (AT_SECURE, 0),
        ]
    };

    // TODO(tbodt): implement MAP_GROWSDOWN and then reset this to 1 page. The current value of
    // this is based on adding 0x1000 each time a segfault appears.
    let stack_size: usize = round_up_to_system_page_size(
        get_initial_stack_size(original_path, &resolved_elf.argv, &resolved_elf.environ, &auxv)
            + 0xf0000,
    )
    .expect("stack is too big");

    let prot_flags = ProtectionFlags::READ | ProtectionFlags::WRITE;

    let stack_base = current_task.mm().map_anonymous(
        DesiredAddress::Any,
        stack_size,
        prot_flags,
        MappingOptions::ANONYMOUS,
        MappingName::Stack,
    )?;

    let stack = stack_base + (stack_size - 8);

    let stack = populate_initial_stack(
        current_task.deref(),
        original_path,
        &resolved_elf.argv,
        &resolved_elf.environ,
        auxv,
        stack,
    )?;

    let mut mm_state = current_task.mm().state.write();
    mm_state.stack_base = stack_base;
    mm_state.stack_size = stack_size;
    mm_state.stack_start = stack.stack_pointer;
    mm_state.auxv_start = stack.auxv_start;
    mm_state.auxv_end = stack.auxv_end;
    mm_state.argv_start = stack.argv_start;
    mm_state.argv_end = stack.argv_end;
    mm_state.environ_start = stack.environ_start;
    mm_state.environ_end = stack.environ_end;

    mm_state.vdso_base = vdso_base;

    Ok(ThreadStartInfo { entry, stack: stack.stack_pointer })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use assert_matches::assert_matches;
    use std::mem::MaybeUninit;

    const TEST_STACK_ADDR: UserAddress = UserAddress::const_from(0x3000_0000);

    struct StackVmo(zx::Vmo);

    impl StackVmo {
        fn address_to_offset(&self, addr: UserAddress) -> u64 {
            (addr - TEST_STACK_ADDR) as u64
        }
    }

    impl MemoryAccessor for StackVmo {
        fn read_memory<'a>(
            &self,
            _addr: UserAddress,
            _bytes: &'a mut [MaybeUninit<u8>],
        ) -> Result<&'a mut [u8], Errno> {
            todo!()
        }

        fn read_memory_partial_until_null_byte<'a>(
            &self,
            _addr: UserAddress,
            _bytes: &'a mut [MaybeUninit<u8>],
        ) -> Result<&'a mut [u8], Errno> {
            todo!()
        }

        fn read_memory_partial<'a>(
            &self,
            _addr: UserAddress,
            _bytes: &'a mut [MaybeUninit<u8>],
        ) -> Result<&'a mut [u8], Errno> {
            todo!()
        }

        fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
            self.0.write(bytes, self.address_to_offset(addr)).map_err(|_| errno!(EFAULT))?;
            Ok(bytes.len())
        }

        fn write_memory_partial(&self, _addr: UserAddress, _bytes: &[u8]) -> Result<usize, Errno> {
            todo!()
        }

        fn zero(&self, _addr: UserAddress, _length: usize) -> Result<usize, Errno> {
            todo!()
        }
    }

    #[::fuchsia::test]
    fn test_trivial_initial_stack() {
        let stack_vmo = StackVmo(zx::Vmo::create(0x4000).expect("VMO creation should succeed."));
        let original_stack_start_addr = TEST_STACK_ADDR + 0x1000u64;

        let path = CString::new(&b""[..]).unwrap();
        let argv = &vec![];
        let environ = &vec![];

        let stack_start_addr = populate_initial_stack(
            &stack_vmo,
            &path,
            argv,
            environ,
            vec![],
            original_stack_start_addr,
        )
        .expect("Populate initial stack should succeed.")
        .stack_pointer;

        let argc_size: usize = 8;
        let argv_terminator_size: usize = 8;
        let environ_terminator_size: usize = 8;
        let aux_execfn_terminator_size: usize = 8;
        let aux_execfn: usize = 16;
        let aux_random: usize = 16;
        let aux_null: usize = 16;
        let random_seed: usize = 16;

        let mut payload_size = argc_size
            + argv_terminator_size
            + environ_terminator_size
            + aux_execfn_terminator_size
            + aux_execfn
            + aux_random
            + aux_null
            + random_seed;
        payload_size += payload_size % 16;

        assert_eq!(stack_start_addr, original_stack_start_addr - payload_size);
    }

    fn exec_hello_starnix(current_task: &mut CurrentTask) -> Result<(), Errno> {
        let argv = vec![CString::new("bin/hello_starnix").unwrap()];
        let executable = current_task.open_file(argv[0].as_bytes().into(), OpenFlags::RDONLY)?;
        current_task.exec(executable, argv[0].clone(), argv, vec![])?;
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_load_hello_starnix() {
        let (_kernel, mut current_task, _) = create_kernel_task_and_unlocked_with_pkgfs();
        exec_hello_starnix(&mut current_task).expect("failed to load executable");
        assert!(current_task.mm().get_mapping_count() > 0);
    }

    // TODO(https://fxbug.dev/42072654): Figure out why this snapshot fails.
    #[cfg(target_arch = "x86_64")]
    #[::fuchsia::test]
    async fn test_snapshot_hello_starnix() {
        let (kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();
        exec_hello_starnix(&mut current_task).expect("failed to load executable");

        let current2 = create_task(&mut locked, &kernel, "another-task");
        current_task.mm().snapshot_to(&mut locked, current2.mm()).expect("failed to snapshot mm");

        assert_eq!(current_task.mm().get_mapping_count(), current2.mm().get_mapping_count());
    }

    #[::fuchsia::test]
    fn test_parse_interpreter_line() {
        assert_matches!(parse_interpreter_line(b"#!"), Err(_));
        assert_matches!(parse_interpreter_line(b"#!\n"), Err(_));
        assert_matches!(parse_interpreter_line(b"#! \n"), Err(_));
        assert_matches!(parse_interpreter_line(b"#!/bin/bash"), Err(_));
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash\x00\n"),
            Ok(vec![CString::new("/bin/bash").unwrap()])
        );
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash\n"),
            Ok(vec![CString::new("/bin/bash").unwrap()])
        );
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash -e\n"),
            Ok(vec![CString::new("/bin/bash").unwrap(), CString::new("-e").unwrap()])
        );
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash -e \n"),
            Ok(vec![CString::new("/bin/bash").unwrap(), CString::new("-e").unwrap()])
        );
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash \t -e\n"),
            Ok(vec![CString::new("/bin/bash").unwrap(), CString::new("-e").unwrap()])
        );
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash -e -x\n"),
            Ok(vec![CString::new("/bin/bash").unwrap(), CString::new("-e -x").unwrap(),])
        );
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash -e  -x\t-l\n"),
            Ok(vec![CString::new("/bin/bash").unwrap(), CString::new("-e  -x\t-l").unwrap(),])
        );
        assert_eq!(
            parse_interpreter_line(b"#!/bin/bash\nfoobar"),
            Ok(vec![CString::new("/bin/bash").unwrap()])
        );
        assert_eq!(
            parse_interpreter_line(b"#! /bin/bash -e  -x\t-l\n"),
            Ok(vec![CString::new("/bin/bash").unwrap(), CString::new("-e  -x\t-l").unwrap(),])
        );
        assert_eq!(
            parse_interpreter_line(b"#!\t/bin/bash \t-l\n"),
            Ok(vec![CString::new("/bin/bash").unwrap(), CString::new("-l").unwrap(),])
        );
    }
}
