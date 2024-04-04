// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::proc::{
        pid_directory::pid_directory,
        sysctl::{net_directory, sysctl_directory},
        sysrq::SysRqNode,
    },
    task::{CurrentTask, EventHandler, Kernel, KernelStats, TaskStateCode, WaitCanceler, Waiter},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        emit_dotdot, fileops_impl_delegate_read_and_seek, fileops_impl_directory,
        fileops_impl_seekless, fs_node_impl_dir_readonly, fs_node_impl_symlink, unbounded_seek,
        BytesFile, DirectoryEntryType, DirentSink, DynamicFile, DynamicFileBuf, DynamicFileSource,
        FileObject, FileOps, FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
        FsString, SeekTarget, SimpleFileNode, StaticDirectoryBuilder, StubEmptyFile, SymlinkTarget,
    },
};
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_zircon as zx;
use maplit::btreemap;
use once_cell::sync::Lazy;
use starnix_logging::{bug_ref, log_error, track_stub};
use starnix_sync::{FileOpsCore, Locked, WriteOps};
use starnix_uapi::{
    auth::FsCred, errno, error, errors::Errno, file_mode::mode, off_t, open_flags::OpenFlags,
    pid_t, time::duration_to_scheduler_clock, vfs::FdEvents,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    time::SystemTime,
};

/// `ProcDirectory` represents the top-level directory in `procfs`.
///
/// It contains, for example, a directory for each running task, named after the task's pid.
///
/// It also contains a special symlink, `self`, which targets the task directory for the task
/// that reads the symlink.
pub struct ProcDirectory {
    /// A map that stores all the nodes that aren't task directories.
    nodes: BTreeMap<&'static FsStr, FsNodeHandle>,
}

impl ProcDirectory {
    /// Returns a new `ProcDirectory` exposing information about `kernel`.
    pub fn new(current_task: &CurrentTask, fs: &FileSystemHandle) -> Arc<ProcDirectory> {
        let kernel = current_task.kernel();

        let nodes = btreemap! {
            "cpuinfo".into() => fs.create_node(
                current_task,
                CpuinfoFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "cmdline".into() => {
                let cmdline = Vec::from(kernel.cmdline.clone());
                fs.create_node(
                    current_task,
                    BytesFile::new_node(cmdline),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                )
            },
            "self".into() => SelfSymlink::new_node(current_task, fs),
            "thread-self".into() => ThreadSelfSymlink::new_node(current_task, fs),
            "meminfo".into() => fs.create_node(
                current_task,
                MeminfoFile::new_node(&kernel.stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            // Fake kmsg as being empty.
            "kmsg".into() => fs.create_node(
                current_task,
                SimpleFileNode::new(|| Ok(ProcKmsgFile)),
                FsNodeInfo::new_factory(mode!(IFREG, 0o100), FsCred::root()),
            ),
            "mounts".into() => MountsSymlink::new_node(current_task, fs),
            // File must exist to pass the CgroupsAvailable check, which is a little bit optional
            // for init but not optional for a lot of the system!
            "cgroups".into() => fs.create_node(
                current_task,
                BytesFile::new_node(vec![]),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "stat".into() => fs.create_node(
                current_task,
                StatFile::new_node(&kernel.stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "swaps".into() => fs.create_node(
                current_task,
                SwapsFile::new_node(kernel),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "sys".into() => sysctl_directory(current_task, fs),
            "pressure".into() => pressure_directory(current_task, fs),
            "net".into() => net_directory(current_task, fs),
            "uptime".into() => fs.create_node(
                current_task,
                UptimeFile::new_node(&kernel.stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "loadavg".into() => fs.create_node(
                current_task,
                LoadavgFile::new_node(kernel),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "config.gz".into() => fs.create_node(
                current_task,
                ConfigFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "sysrq-trigger".into() => fs.create_node(
                current_task,
                SysRqNode::new(kernel),
                // This file is normally writable only by root.
                // (https://man7.org/linux/man-pages/man5/proc.5.html)
                FsNodeInfo::new_factory(mode!(IFREG, 0o200), FsCred::root()),
            ),
            "asound".into() => fs.create_node(
                current_task,
                // Note: this is actually a directory but for now just track when it's opened.
                StubEmptyFile::new_node("/proc/asound", bug_ref!("https://fxbug.dev/322893329")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "diskstats".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/diskstats", bug_ref!("https://fxbug.dev/322893370")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "filesystems".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node(
                    "/proc/filesystems",
                    bug_ref!("https://fxbug.dev/309002087"),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "misc".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/misc", bug_ref!("https://fxbug.dev/322893943")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "modules".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/modules", bug_ref!("https://fxbug.dev/322894157")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "pagetypeinfo".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node(
                    "/proc/pagetypeinfo",
                    bug_ref!("https://fxbug.dev/322894315"),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "slabinfo".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/slabinfo", bug_ref!("https://fxbug.dev/322894195")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "uid_cputime".into() => {
                let mut dir = StaticDirectoryBuilder::new(fs);
                dir.entry(
                    current_task,
                    "remove_uid_range",
                    StubEmptyFile::new_node(
                        "/proc/uid_cputime/remove_uid_range",
                        bug_ref!("https://fxbug.dev/322894025"),
                    ),
                    mode!(IFREG, 0o222),
                );
                dir.entry(
                    current_task,
                    "show_uid_stat",
                    StubEmptyFile::new_node(
                        "/proc/uid_cputime/show_uid_stat",
                        bug_ref!("https://fxbug.dev/322893886"),
                    ),
                    mode!(IFREG, 0444),
                );
                dir.build(current_task)
            },
            "uid_io".into() => {
                let mut dir = StaticDirectoryBuilder::new(fs);
                dir.entry(
                    current_task,
                    "stats",
                    StubEmptyFile::new_node(
                        "/proc/uid_io/stats",
                        bug_ref!("https://fxbug.dev/322893966"),
                    ),
                    mode!(IFREG, 0o444),
                );
                dir.build(current_task)
            },
            "uid_procstat".into() => {
                let mut dir = StaticDirectoryBuilder::new(fs);
                dir.entry(
                    current_task,
                    "set",
                    StubEmptyFile::new_node(
                        "/proc/uid_procstat/set",
                        bug_ref!("https://fxbug.dev/322894041"),
                    ),
                    mode!(IFREG, 0o222),
                );
                dir.build(current_task)
            },
            "version".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/version", bug_ref!("https://fxbug.dev/309002311")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "vmallocinfo".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node(
                    "/proc/vmallocinfo",
                    bug_ref!("https://fxbug.dev/322894183"),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "vmstat".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/vmstat", bug_ref!("https://fxbug.dev/322894141")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "zoneinfo".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/zoneinfo", bug_ref!("https://fxbug.dev/322893866")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
        };

        Arc::new(ProcDirectory { nodes })
    }
}

impl FsNodeOps for Arc<ProcDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match self.nodes.get(name) {
            Some(node) => Ok(Arc::clone(node)),
            None => {
                let pid_string = std::str::from_utf8(name).map_err(|_| errno!(ENOENT))?;
                let pid = pid_string.parse::<pid_t>().map_err(|_| errno!(ENOENT))?;
                let weak_task = current_task.get_task(pid);
                let task = weak_task.upgrade().ok_or_else(|| errno!(ENOENT))?;
                let mut pd_state = task.proc_pid_directory_cache.lock();
                if let Some(pd) = &*pd_state {
                    Ok(pd.clone())
                } else {
                    let pd = pid_directory(current_task, &node.fs(), &task);
                    *pd_state = Some(pd.clone());
                    Ok(pd)
                }
            }
        }
    }
}

impl FileOps for Arc<ProcDirectory> {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        unbounded_seek(current_offset, target)
    }

    fn readdir(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // Iterate through all the named entries (i.e., non "task directories") and add them to
        // the sink. Subtract 2 from the offset, to account for `.` and `..`.
        for (name, node) in self.nodes.iter().skip((sink.offset() - 2) as usize) {
            sink.add(
                node.node_id,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(node.info().mode),
                name,
            )?;
        }

        // Add 2 to the number of non-"task directories", to account for `.` and `..`.
        let pid_offset = (self.nodes.len() + 2) as i32;

        // Adjust the offset to account for the other nodes in the directory.
        let adjusted_offset = (sink.offset() - pid_offset as i64) as usize;
        // Sort the pids, to keep the traversal order consistent.
        let mut pids = current_task.kernel().pids.read().process_ids();
        pids.sort();

        // The adjusted offset is used to figure out which task directories are to be listed.
        if let Some(start) = pids.iter().position(|pid| *pid as usize >= adjusted_offset) {
            for pid in &pids[start..] {
                // TODO: Figure out if this inode number is fine, given the content of the task
                // directories.
                let inode_num = file.fs.next_node_id();
                let name = FsString::from(format!("{pid}"));

                // The + 1 is to set the offset to the next possible pid for subsequent reads.
                let next_offset = (*pid + pid_offset + 1) as i64;
                sink.add(inode_num, next_offset, DirectoryEntryType::DIR, name.as_ref())?;
            }
        }

        Ok(())
    }
}

struct ProcKmsgFile;

impl FileOps for ProcKmsgFile {
    fileops_impl_seekless!();

    fn wait_async(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let syslog = current_task.kernel().syslog.access(current_task).ok()?;
        Some(syslog.wait(waiter, events, handler))
    }

    fn query_events(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let syslog = current_task.kernel().syslog.access(current_task)?;
        let mut events = FdEvents::empty();
        if syslog.size_unread()? > 0 {
            events |= FdEvents::POLLIN;
        }
        Ok(events)
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let syslog = current_task.kernel().syslog.access(current_task)?;
        file.blocking_op(current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, || {
            let bytes_written = syslog.read(data)?;
            Ok(bytes_written as usize)
        })
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EIO)
    }
}

/// A node that represents a symlink to `proc/<pid>` where <pid> is the pid of the task that
/// reads the `proc/self` symlink.
struct SelfSymlink;

impl SelfSymlink {
    fn new_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
        fs.create_node(
            current_task,
            Self,
            FsNodeInfo::new_factory(mode!(IFLNK, 0o777), FsCred::root()),
        )
    }
}

impl FsNodeOps for SelfSymlink {
    fs_node_impl_symlink!();

    fn readlink(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path(current_task.get_pid().to_string().into()))
    }
}

/// A node that represents a symlink to `proc/<pid>/task/<tid>` where <pid> and <tid> are derived
/// from the task reading the symlink.
struct ThreadSelfSymlink;

impl ThreadSelfSymlink {
    fn new_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
        fs.create_node(
            current_task,
            Self,
            FsNodeInfo::new_factory(mode!(IFLNK, 0o777), FsCred::root()),
        )
    }
}

impl FsNodeOps for ThreadSelfSymlink {
    fs_node_impl_symlink!();

    fn readlink(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path(
            format!("{}/task/{}", current_task.get_pid(), current_task.get_tid()).into(),
        ))
    }
}

/// A node that represents a link to `self/mounts`.
struct MountsSymlink;

impl MountsSymlink {
    fn new_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
        fs.create_node(
            current_task,
            Self,
            FsNodeInfo::new_factory(mode!(IFLNK, 0o777), FsCred::root()),
        )
    }
}

impl FsNodeOps for MountsSymlink {
    fs_node_impl_symlink!();

    fn readlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path("self/mounts".into()))
    }
}

/// Creates the /proc/pressure directory. https://docs.kernel.org/accounting/psi.html
fn pressure_directory(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.entry(current_task, "memory", PressureFile::new_node(), mode!(IFREG, 0o666));
    dir.build(current_task)
}

struct PressureFileSource;
impl DynamicFileSource for PressureFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        writeln!(sink, "some avg10={:.2} avg60={:.2} avg300={:.2} total={}", 0, 0, 0, 0)?;
        writeln!(sink, "full avg10={:.2} avg60={:.2} avg300={:.2} total={}", 0, 0, 0, 0)?;
        Ok(())
    }
}

struct PressureFile(DynamicFile<PressureFileSource>);
impl PressureFile {
    pub fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self(DynamicFile::new(PressureFileSource {}))))
    }
}

impl FileOps for PressureFile {
    fileops_impl_delegate_read_and_seek!(self, self.0);

    /// Pressure notifications are configured by writing to the file.
    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // Ignore the request for now.
        track_stub!(TODO("https://fxbug.dev/322873423"), "pressure notification setup");
        Ok(data.drain())
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(waiter.fake_wait())
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::empty())
    }
}

struct SysInfo {
    board_name: String,
}

impl SysInfo {
    fn is_qemu(&self) -> bool {
        matches!(
            self.board_name.as_str(),
            "Standard PC (Q35 + ICH9, 2009)" | "qemu-arm64" | "qemu-riscv64"
        )
    }

    fn fetch() -> Result<SysInfo, anyhow::Error> {
        let sysinfo = connect_to_protocol_sync::<fidl_fuchsia_sysinfo::SysInfoMarker>()?;
        let board_name = match sysinfo.get_board_name(zx::Time::INFINITE)? {
            (zx::sys::ZX_OK, Some(name)) => name,
            (_, _) => "Unknown".to_string(),
        };
        Ok(SysInfo { board_name })
    }
}

const SYSINFO: Lazy<SysInfo> = Lazy::new(|| {
    SysInfo::fetch().unwrap_or_else(|e| {
        log_error!("Failed to fetch sysinfo: {e}");
        SysInfo { board_name: "Unknown".to_string() }
    })
});

#[derive(Clone)]
struct CpuinfoFile {}
impl CpuinfoFile {
    pub fn new_node() -> impl FsNodeOps {
        DynamicFile::new_node(Self {})
    }
}
impl DynamicFileSource for CpuinfoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let is_qemu = SYSINFO.is_qemu();

        for i in 0..fuchsia_zircon::system_get_num_cpus() {
            writeln!(sink, "processor\t: {}", i)?;

            // Report emulated CPU as "QEMU Virtual CPU". Some LTP tests rely on this to detect
            // that they running in a VM.
            if is_qemu {
                writeln!(sink, "model name\t: QEMU Virtual CPU")?;
            }

            writeln!(sink)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ConfigFile;
impl ConfigFile {
    pub fn new_node() -> impl FsNodeOps {
        DynamicFile::new_node(Self)
    }
}
impl DynamicFileSource for ConfigFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let contents = std::fs::read("/pkg/data/config.gz").map_err(|e| {
            log_error!("Error reading /pkg/data/config.gz: {e}");
            errno!(EIO)
        })?;
        sink.write(&contents);
        Ok(())
    }
}

#[derive(Clone)]
struct MeminfoFile {
    kernel_stats: Arc<KernelStats>,
}
impl MeminfoFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}
impl DynamicFileSource for MeminfoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let stats = self.kernel_stats.get();
        let memory_stats = stats.get_memory_stats_extended(zx::Time::INFINITE).map_err(|e| {
            log_error!("FIDL error getting memory stats: {e}");
            errno!(EIO)
        })?;
        let compression_stats =
            stats.get_memory_stats_compression(zx::Time::INFINITE).map_err(|e| {
                log_error!("FIDL error getting memory compression stats: {e}");
                errno!(EIO)
            })?;

        let mem_total = memory_stats.total_bytes.unwrap_or_default() / 1024;
        let mem_free = memory_stats.free_bytes.unwrap_or_default() / 1024;
        let mem_available = (memory_stats.free_bytes.unwrap_or_default()
            + memory_stats.vmo_discardable_unlocked_bytes.unwrap_or_default())
            / 1024;

        let swap_used = compression_stats.uncompressed_storage_bytes.unwrap_or_default() / 1024;
        // Fuchsia doesn't have a limit on the size of its swap file, so we just pretend that
        // we're willing to grow the swap by half the amount of free memory.
        let swap_free = mem_free / 2;
        let swap_total = swap_used + swap_free;

        writeln!(sink, "MemTotal:       {:8} kB", mem_total)?;
        writeln!(sink, "MemFree:        {:8} kB", mem_free)?;
        writeln!(sink, "MemAvailable:   {:8} kB", mem_available)?;
        writeln!(sink, "SwapTotal:      {:8} kB", swap_total)?;
        writeln!(sink, "SwapFree:       {:8} kB", swap_free)?;
        Ok(())
    }
}

#[derive(Clone)]
struct UptimeFile {
    kernel_stats: Arc<KernelStats>,
}

impl UptimeFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}

impl DynamicFileSource for UptimeFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let uptime = (zx::Time::get_monotonic() - zx::Time::ZERO).into_seconds_f64();

        // Fetch CPU stats from `fuchsia.kernel.Stats` to calculate idle time.
        let cpu_stats =
            self.kernel_stats.get().get_cpu_stats(zx::Time::INFINITE).map_err(|_| errno!(EIO))?;
        let per_cpu_stats = cpu_stats.per_cpu_stats.unwrap_or(vec![]);
        let idle_time = per_cpu_stats.iter().map(|s| s.idle_time.unwrap_or(0)).sum();
        let idle_time = zx::Duration::from_nanos(idle_time).into_seconds_f64();

        writeln!(sink, "{:.2} {:.2}", uptime, idle_time)?;

        Ok(())
    }
}

#[derive(Clone)]
struct StatFile {
    kernel_stats: Arc<KernelStats>,
}
impl StatFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}
impl DynamicFileSource for StatFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let uptime = zx::Time::get_monotonic() - zx::Time::ZERO;

        let cpu_stats =
            self.kernel_stats.get().get_cpu_stats(zx::Time::INFINITE).map_err(|_| errno!(EIO))?;

        // Number of values reported per CPU. See `get_cpu_stats_row` below for the list of values.
        const NUM_CPU_STATS: usize = 10;

        let get_cpu_stats_row = |cpu_stats: &fidl_fuchsia_kernel::PerCpuStats| {
            let idle = zx::Duration::from_nanos(cpu_stats.idle_time.unwrap_or(0));

            // Assume that all non-idle time is spent in user mode.
            let user = uptime - idle;

            // Zircon currently reports only number of various interrupts, but not the time spent
            // handling them. Return zeros.
            let nice: u64 = 0;
            let system: u64 = 0;
            let iowait: u64 = 0;
            let irq: u64 = 0;
            let softirq: u64 = 0;
            let steal: u64 = 0;
            let quest: u64 = 0;
            let quest_nice: u64 = 0;

            [
                duration_to_scheduler_clock(user) as u64,
                nice,
                system,
                duration_to_scheduler_clock(idle) as u64,
                iowait,
                irq,
                softirq,
                steal,
                quest,
                quest_nice,
            ]
        };
        let per_cpu_stats = cpu_stats.per_cpu_stats.unwrap_or(vec![]);
        let mut cpu_total_row = [0u64; NUM_CPU_STATS];
        for row in per_cpu_stats.iter().map(get_cpu_stats_row) {
            for (i, value) in row.iter().enumerate() {
                cpu_total_row[i] += value
            }
        }

        writeln!(sink, "cpu {}", cpu_total_row.map(|n| n.to_string()).join(" "))?;
        for (i, row) in per_cpu_stats.iter().map(get_cpu_stats_row).enumerate() {
            writeln!(sink, "cpu{} {}", i, row.map(|n| n.to_string()).join(" "))?;
        }

        let context_switches: u64 =
            per_cpu_stats.iter().map(|s| s.context_switches.unwrap_or(0)).sum();
        writeln!(sink, "ctxt {}", context_switches)?;

        let num_interrupts: u64 = per_cpu_stats.iter().map(|s| s.ints.unwrap_or(0)).sum();
        writeln!(sink, "intr {}", num_interrupts)?;

        let epoch_time = zx::Duration::from(
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default(),
        );
        let boot_time_epoch = epoch_time - uptime;
        writeln!(sink, "btime {}", boot_time_epoch.into_seconds())?;

        Ok(())
    }
}

#[derive(Clone)]
struct LoadavgFile(Weak<Kernel>);
impl LoadavgFile {
    pub fn new_node(kernel: &Arc<Kernel>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(Arc::downgrade(kernel)))
    }
}
impl DynamicFileSource for LoadavgFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let (runnable_tasks, existing_tasks, last_pid) = {
            let kernel = self.0.upgrade().ok_or(errno!(EIO))?;
            let pid_table = kernel.pids.read();

            let curr_tids = pid_table.task_ids();
            let mut runnable_tasks = 0;
            for pid in &curr_tids {
                let weak_task = pid_table.get_task(*pid);
                if let Some(task) = weak_task.upgrade() {
                    if task.state_code() == TaskStateCode::Running {
                        runnable_tasks += 1;
                    }
                };
            }

            let existing_tasks = pid_table.process_ids().len() + curr_tids.len();
            (runnable_tasks, existing_tasks, pid_table.last_pid())
        };

        track_stub!(TODO("https://fxbug.dev/322874486"), "/proc/loadavg load stats");
        writeln!(sink, "0.50 0.50 0.50 {}/{} {}", runnable_tasks, existing_tasks, last_pid)?;
        Ok(())
    }
}

#[derive(Clone)]
// Tuple member is never used
#[allow(dead_code)]
struct SwapsFile(Weak<Kernel>);
impl SwapsFile {
    pub fn new_node(kernel: &Arc<Kernel>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(Arc::downgrade(kernel)))
    }
}
impl DynamicFileSource for SwapsFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/322874154"), "/proc/swaps includes Kernel::swap_files");
        writeln!(sink, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority")?;
        Ok(())
    }
}
