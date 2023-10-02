// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{target::Target, RETRY_DELAY};
use anyhow::{anyhow, Context};
use async_io::Async;
use async_trait::async_trait;
use compat_info::{CompatibilityInfo, ConnectionInfo};
use ffx_daemon_core::events;
use ffx_daemon_events::{HostPipeErr, TargetEvent};
use ffx_ssh::ssh::build_ssh_command_with_ssh_path;
use fuchsia_async::{unblock, Task, TimeoutExt, Timer};
use futures::io::{copy_buf, AsyncBufRead, AsyncRead, BufReader};
use futures::{AsyncReadExt, FutureExt};
use futures_lite::stream::StreamExt;
use shared_child::SharedChild;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fmt, io,
    io::Write,
    net::SocketAddr,
    process::Stdio,
    rc::{Rc, Weak},
    sync::Arc,
    time::Duration,
};

const BUFFER_SIZE: usize = 65536;

#[derive(thiserror::Error, Debug)]
pub(crate) enum PipeError {
    #[error("compatibility check not supported")]
    NoCompatibilityCheck,
    #[error("could not establish connection: {0}")]
    ConnectionFailed(String),
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("error {0}")]
    Error(String),
    #[error("target referenced has gone")]
    TargetGone,
    #[error("creating pipe to {1} failed: {0}")]
    PipeCreationFailed(String, String),
    #[error("no shh address to {0}")]
    NoAddress(String),
    #[error("running target overnet pipe: {0}")]
    SpawnError(String),
}

#[derive(Debug)]
pub struct LogBuffer {
    buf: RefCell<VecDeque<String>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { buf: RefCell::new(VecDeque::with_capacity(capacity)), capacity }
    }

    pub fn push_line(&self, line: String) {
        let mut buf = self.buf.borrow_mut();
        if buf.len() == self.capacity {
            buf.pop_front();
        }

        buf.push_back(line)
    }

    pub fn lines(&self) -> Vec<String> {
        let buf = self.buf.borrow_mut();
        buf.range(..).cloned().collect()
    }

    pub fn clear(&self) {
        let mut buf = self.buf.borrow_mut();
        buf.truncate(0);
    }
}

#[derive(Debug, Clone)]
pub struct HostAddr(String);

impl fmt::Display for HostAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<&str> for HostAddr {
    fn from(s: &str) -> Self {
        HostAddr(s.to_string())
    }
}

impl From<String> for HostAddr {
    fn from(s: String) -> Self {
        HostAddr(s)
    }
}

#[async_trait(?Send)]
pub(crate) trait HostPipeChildBuilder {
    type NodeType: Clone;
    async fn new(
        &self,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        node: Self::NodeType,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError>
    where
        Self: Sized;

    fn ssh_path(&self) -> &str;
}

#[derive(Copy, Clone)]
pub(crate) struct HostPipeChildDefaultBuilder<'a> {
    pub(crate) ssh_path: &'a str,
}

#[async_trait(?Send)]
impl HostPipeChildBuilder for HostPipeChildDefaultBuilder<'_> {
    type NodeType = Arc<overnet_core::Router>;
    async fn new(
        &self,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        node: Arc<overnet_core::Router>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        let ctx = ffx_config::global_env_context().expect("Global env context uninitialized");
        let verbose_ssh = ffx_config::logging::debugging_on(&ctx).await;

        HostPipeChild::new_inner(
            self.ssh_path(),
            addr,
            id,
            stderr_buf,
            event_queue,
            watchdogs,
            ssh_timeout,
            verbose_ssh,
            node,
        )
        .await
    }

    fn ssh_path(&self) -> &str {
        self.ssh_path
    }
}

#[derive(Debug)]
pub(crate) struct HostPipeChild {
    inner: SharedChild,
    task: Option<Task<()>>,
    pub(crate) compatibility_status: Option<CompatibilityInfo>,
}

fn setup_watchdogs() {
    use std::sync::atomic::{AtomicBool, Ordering};

    tracing::debug!("Setting up executor watchdog");
    let flag = Arc::new(AtomicBool::new(false));

    fuchsia_async::Task::spawn({
        let flag = Arc::clone(&flag);
        async move {
            fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
            flag.store(true, Ordering::Relaxed);
            tracing::debug!("Executor watchdog fired");
        }
    })
    .detach();

    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if !flag.load(Ordering::Relaxed) {
            tracing::error!("Aborting due to watchdog timeout!");
            std::process::abort();
        }
    });
}

async fn write_ssh_log(prefix: &str, line: &String) {
    let ctx = match ffx_config::global_env_context() {
        Some(c) => c,
        None => {
            tracing::warn!("Couldn't open ssh log file, due to no global env context");
            return;
        }
    };
    if !ffx_config::logging::debugging_on(&ctx).await {
        return;
    }
    // Skip keepalives, which will show up in the steady-state
    if line.contains("keepalive") {
        return;
    }
    let mut f = match ffx_config::logging::log_file_with_info(&ctx, "ssh", true).await {
        Ok((f, _)) => f,
        Err(e) => {
            tracing::warn!("Couldn't open ssh log file: {e:?}");
            return;
        }
    };
    const TIME_FORMAT: &str = "%b %d %H:%M:%S%.3f";
    let timestamp = chrono::Local::now().format(TIME_FORMAT);
    write!(&mut f, "{timestamp}: {prefix} {line}")
        .unwrap_or_else(|e| tracing::warn!("Couldn't write ssh log: {e:?}"));
}

impl HostPipeChild {
    pub fn get_compatibility_status(&self) -> Option<CompatibilityInfo> {
        self.compatibility_status.clone()
    }

    #[tracing::instrument(skip(stderr_buf, event_queue))]
    async fn new_inner_legacy(
        ssh_path: &str,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        verbose_ssh: bool,
        node: Arc<overnet_core::Router>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        let id_string = format!("{}", id);
        let args = vec![
            "echo",
            "++ $SSH_CONNECTION ++",
            "&&",
            "remote_control_runner",
            "--circuit",
            &id_string,
        ];

        Self::start_ssh_connection(
            ssh_path,
            addr,
            args,
            stderr_buf,
            event_queue,
            watchdogs,
            ssh_timeout,
            verbose_ssh,
            node,
        )
        .await
    }

    #[tracing::instrument(skip(stderr_buf, event_queue))]
    async fn new_inner(
        ssh_path: &str,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        verbose_ssh: bool,
        node: Arc<overnet_core::Router>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        let id_string = format!("{}", id);

        // pass the abi revision as a base 10 number so it is easy to parse.
        let rev: u64 = *version_history::LATEST_VERSION.abi_revision;
        let abi_revision = format!("{}", rev);
        let args =
            vec!["remote_control_runner", "--circuit", &id_string, "--abi-revision", &abi_revision];

        match Self::start_ssh_connection(
            ssh_path,
            addr,
            args,
            stderr_buf.clone(),
            event_queue.clone(),
            watchdogs,
            ssh_timeout,
            verbose_ssh,
            Arc::clone(&node),
        )
        .await
        {
            Ok((addr, pipe)) => Ok((addr, pipe)),
            Err(PipeError::NoCompatibilityCheck) => {
                Self::new_inner_legacy(
                    ssh_path,
                    addr,
                    id,
                    stderr_buf,
                    event_queue,
                    watchdogs,
                    ssh_timeout,
                    verbose_ssh,
                    node,
                )
                .await
            }
            Err(e) => Err(e),
        }
    }

    async fn start_ssh_connection(
        ssh_path: &str,
        addr: SocketAddr,
        mut args: Vec<&str>,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        verbose_ssh: bool,
        node: Arc<overnet_core::Router>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        if verbose_ssh {
            args.insert(0, "-v");
        }

        let mut ssh = build_ssh_command_with_ssh_path(ssh_path, addr, args)
            .await
            .map_err(|e| PipeError::Error(e.to_string()))?;

        tracing::debug!("Spawning new ssh instance: {:?}", ssh);

        if watchdogs {
            setup_watchdogs();
        }

        let mut ssh_cmd = ssh.stdout(Stdio::piped()).stdin(Stdio::piped()).stderr(Stdio::piped());

        let ssh =
            SharedChild::spawn(&mut ssh_cmd).map_err(|e| PipeError::SpawnError(e.to_string()))?;

        let mut error_message = String::from("");

        let (pipe_rx, mut pipe_tx) =
            futures::AsyncReadExt::split(overnet_pipe(node).map_err(|e| {
                PipeError::PipeCreationFailed(
                    format!("creating local overnet pipe: {e}"),
                    addr.to_string(),
                )
            })?);

        let stdout = Async::new(
            ssh.take_stdout()
                .ok_or(PipeError::Error("unable to get stdout from target pipe".into()))?,
        )?;

        let mut stdin = Async::new(
            ssh.take_stdin()
                .ok_or(PipeError::Error("unable to get stdin from target pipe".into()))?,
        )?;

        let mut stderr = Async::new(
            ssh.take_stderr()
                .ok_or(PipeError::Error("unable to stderr from target pipe".into()))?,
        )?;

        // Read the first line. This can be either either be an empty string "",
        // which signifies the STDOUT has been closed, or the $SSH_CONNECTION
        // value.
        let mut stdout = BufReader::with_capacity(BUFFER_SIZE, stdout);

        tracing::debug!("Awaiting client address from ssh connection");
        let (ssh_host_address, compatibility_status) =
            match parse_ssh_connection(&mut stdout, Some(ssh_timeout))
                .on_timeout(Duration::from_secs(ssh_timeout as u64), || {
                    Err(ParseSshConnectionError::Timeout)
                })
                .await
                .context("reading ssh connection")
            {
                Ok((addr, compatibility_status)) => (Some(HostAddr(addr)), compatibility_status),
                Err(e) => {
                    error_message = format!("Failed to read ssh client address: {e:?}");
                    tracing::error!("{error_message}");
                    (None, None)
                }
            };
        // Check for early exit.
        if ssh_host_address.is_none() {
            let mut lb = LineBuffer::new();
            while let Ok(l) = read_ssh_line(&mut lb, &mut stderr).await {
                write_ssh_log("E", &l).await;
                // If we are running with "ssh -v", the stderr will also contain the initial
                // "OpenSSH" line; then any additional debugging messages will begin with "debug1".
                if l.contains("OpenSSH") {
                    continue;
                }
                if l.starts_with("debug1:") {
                    continue;
                }
                // At this point, we just want to look at one line to see if it is the compatibility
                // failure.
                tracing::debug!("Reading stderr:  {}", l);
                if l.contains("Unrecognized argument: --abi-revision") {
                    // It is an older image, so use the legacy command.
                    tracing::info!("Target does not support abi compatibility check, reverting to legacy connection");
                    ssh.kill()?;
                    while let Ok(line) = read_ssh_line(&mut lb, &mut stderr).await {
                        tracing::error!("SSH stderr: {line}");
                    }

                    if let Some(status) = ssh.try_wait()? {
                        tracing::error!("Target pipe exited with {status}");
                    } else {
                        tracing::error!(
                            "ssh child has not ended, trying one more time then ignoring it."
                        );
                        fuchsia_async::Timer::new(std::time::Duration::from_secs(2)).await;
                        tracing::error!("ssh child status is {:?}", ssh.try_wait());
                    }
                    return Err(PipeError::NoCompatibilityCheck);
                }
                break;
            }
        }

        let copy_in = async move {
            if let Err(e) = copy_buf(stdout, &mut pipe_tx).await {
                tracing::error!("SSH stdout read failure: {:?}", e);
            }
        };
        let copy_out = async move {
            if let Err(e) =
                copy_buf(BufReader::with_capacity(BUFFER_SIZE, pipe_rx), &mut stdin).await
            {
                tracing::error!("SSH stdin write failure: {:?}", e);
            }
        };

        let log_stderr = async move {
            let mut lb = LineBuffer::new();
            loop {
                let result = read_ssh_line(&mut lb, &mut stderr).await;
                match result {
                    Ok(line) => {
                        // TODO(slgrady) -- either remove this once we stop having
                        // ssh connection problems; or change it so that once we
                        // know the connection is established, the error messages
                        // go to the event queue as normal.
                        if verbose_ssh {
                            write_ssh_log("E", &line).await;
                        } else {
                            tracing::info!("SSH stderr: {}", line);
                            stderr_buf.push_line(line.clone());
                            event_queue
                                .push(TargetEvent::SshHostPipeErr(HostPipeErr::from(line)))
                                .unwrap_or_else(|e| {
                                    tracing::warn!("queueing host pipe err event: {:?}", e)
                                });
                        }
                    }
                    Err(ParseSshConnectionError::UnexpectedEOF(s)) => {
                        if !s.is_empty() {
                            tracing::error!("Got unexpected EOF -- buffer so far: {s:?}");
                        }
                        break;
                    }
                    Err(e) => tracing::error!("SSH stderr read failure: {:?}", e),
                }
            }
        };

        if ssh_host_address.is_some() {
            tracing::debug!("Establishing host-pipe process to target");
            Ok((
                ssh_host_address,
                HostPipeChild {
                    inner: ssh,
                    task: Some(Task::local(async move {
                        futures::join!(copy_in, copy_out, log_stderr);
                    })),
                    compatibility_status,
                },
            ))
        } else {
            return Err(PipeError::ConnectionFailed(error_message));
        }
    }

    fn kill(&self) -> io::Result<()> {
        self.inner.kill()
    }

    fn wait(&self) -> io::Result<std::process::ExitStatus> {
        self.inner.wait()
    }
}

#[derive(Debug, thiserror::Error)]
enum ParseSshConnectionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Parse error: {:?}", .0)]
    Parse(String),
    #[error("Unexpected EOF: {:?}", .0)]
    UnexpectedEOF(String),
    #[error("Read-line timeout")]
    Timeout,
}

const BUFSIZE: usize = 1024;
struct LineBuffer {
    buffer: [u8; BUFSIZE],
    pos: usize,
}

// 1K should be enough for the initial line, which just looks something like
//    "++ 192.168.1.1 1234 10.0.0.1 22 ++\n"
impl LineBuffer {
    fn new() -> Self {
        Self { buffer: [0; BUFSIZE], pos: 0 }
    }
    fn line(&self) -> &[u8] {
        &self.buffer[..self.pos]
    }
}

impl ToString for LineBuffer {
    fn to_string(&self) -> String {
        String::from_utf8_lossy(self.line()).into()
    }
}

#[tracing::instrument(skip(lb, rdr))]
async fn read_ssh_line<R: AsyncRead + Unpin>(
    lb: &mut LineBuffer,
    rdr: &mut R,
) -> std::result::Result<String, ParseSshConnectionError> {
    loop {
        // We're reading a byte at a time, which would be bad if we were doing it a lot,
        // but it's only used for stderr (which should normally not produce much data),
        // and the first line of stdout.
        let mut b = [0u8];
        let n = rdr.read(&mut b[..]).await.map_err(ParseSshConnectionError::Io)?;
        let b = b[0];
        if n == 0 {
            return Err(ParseSshConnectionError::UnexpectedEOF(lb.to_string()));
        }
        lb.buffer[lb.pos] = b;
        lb.pos += 1;
        if lb.pos >= lb.buffer.len() {
            return Err(ParseSshConnectionError::Parse(format!(
                "Buffer full: {:?}...",
                &lb.buffer[..64]
            )));
        }
        if b == b'\n' {
            let s = lb.to_string();
            // Clear for next read
            lb.pos = 0;
            return Ok(s);
        }
    }
}

#[tracing::instrument(skip(rdr))]
async fn read_ssh_line_with_timeouts<R: AsyncBufRead + Unpin>(
    rdr: &mut R,
    timeout: Option<u16>,
) -> Result<String, ParseSshConnectionError> {
    let mut time = 0;
    let wait_time = 2;
    let mut lb = LineBuffer::new();
    loop {
        match read_ssh_line(&mut lb, rdr)
            .on_timeout(Duration::from_secs(wait_time), || Err(ParseSshConnectionError::Timeout))
            .await
        {
            Ok(s) => {
                return Ok(s);
            }
            Err(ParseSshConnectionError::Timeout) => {
                tracing::debug!("No line after {time}, line so far: {:?}", lb.line());
                time += wait_time;
                if let Some(t) = timeout {
                    if time > t.into() {
                        return Err(ParseSshConnectionError::Timeout);
                    }
                }
            }
            Err(e) => {
                return Err(e);
            }
        }
    }
}

#[tracing::instrument(skip(rdr))]
async fn parse_ssh_connection<R: AsyncBufRead + Unpin>(
    rdr: &mut R,
    timeout: Option<u16>,
) -> std::result::Result<(String, Option<CompatibilityInfo>), ParseSshConnectionError> {
    let line = read_ssh_line_with_timeouts(rdr, timeout).await?;
    if line.is_empty() {
        tracing::error!("Failed to first line");
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }

    write_ssh_log("O", &line).await;

    if line.starts_with("{") {
        parse_ssh_connection_with_info(&line)
    } else {
        parse_ssh_connection_legacy(&line)
    }
}

fn parse_ssh_connection_with_info(
    line: &str,
) -> std::result::Result<(String, Option<CompatibilityInfo>), ParseSshConnectionError> {
    let connection_info: ConnectionInfo =
        serde_json::from_str(&line).map_err(|e| ParseSshConnectionError::Parse(e.to_string()))?;
    let mut parts = connection_info.ssh_connection.split(" ");
    // SSH_CONNECTION identifies the client and server ends of the connection.
    // The variable contains four space-separated values: client IP address,
    // client port number, server IP address, and server port number.
    if let Some(client_address) = parts.nth(0) {
        Ok((client_address.to_string(), Some(connection_info.compatibility)))
    } else {
        Err(ParseSshConnectionError::Parse(line.into()))
    }
}
fn parse_ssh_connection_legacy(
    line: &str,
) -> std::result::Result<(String, Option<CompatibilityInfo>), ParseSshConnectionError> {
    let mut parts = line.split(" ");
    // The first part should be our anchor.
    match parts.next() {
        Some("++") => {}
        Some(_) | None => {
            tracing::error!("Failed to read first anchor: {line}");
            return Err(ParseSshConnectionError::Parse(line.into()));
        }
    }

    // SSH_CONNECTION identifies the client and server ends of the connection.
    // The variable contains four space-separated values: client IP address,
    // client port number, server IP address, and server port number.
    // This is left as a string since std::net::IpAddr does not support string scope_ids.
    let client_address = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read client_address: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // Followed by the client port.
    let _client_port = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read port: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // Followed by the server address.
    let _server_address = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read port: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // Followed by the server port.
    let _server_port = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read server_port: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // The last part should be our anchor.
    match parts.next() {
        Some("++\n") => {}
        None | Some(_) => {
            return Err(ParseSshConnectionError::Parse(line.into()));
        }
    };

    // Finally, there should be nothing left.
    if let Some(_) = parts.next() {
        tracing::error!("Extra data: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    }

    Ok((client_address.to_string(), None))
}

impl Drop for HostPipeChild {
    fn drop(&mut self) {
        match self.inner.try_wait() {
            Ok(Some(result)) => {
                tracing::info!("HostPipeChild exited with {}", result);
            }
            Ok(None) => {
                let _ = self
                    .kill()
                    .map_err(|e| tracing::warn!("failed to kill HostPipeChild: {:?}", e));
                let _ = self
                    .wait()
                    .map_err(|e| tracing::warn!("failed to clean up HostPipeChild: {:?}", e));
            }
            Err(e) => {
                tracing::warn!("failed to soft-wait HostPipeChild: {:?}", e);
                // defensive kill & wait, both may fail.
                let _ = self
                    .kill()
                    .map_err(|e| tracing::warn!("failed to kill HostPipeChild: {:?}", e));
                let _ = self
                    .wait()
                    .map_err(|e| tracing::warn!("failed to clean up HostPipeChild: {:?}", e));
            }
        };

        drop(self.task.take());
    }
}

#[derive(Debug)]
pub(crate) struct HostPipeConnection<T>
where
    T: HostPipeChildBuilder + Copy,
{
    target: Rc<Target>,
    inner: Arc<HostPipeChild>,
    relaunch_command_delay: Duration,
    host_pipe_child_builder: T,
    ssh_timeout: u16,
    watchdogs: bool,
}

impl<T> Drop for HostPipeConnection<T>
where
    T: HostPipeChildBuilder + Copy,
{
    fn drop(&mut self) {
        let res = self.inner.kill();
        tracing::info!("killed inner {:?}", res)
    }
}

pub(crate) async fn spawn<'a>(
    target: Weak<Target>,
    watchdogs: bool,
    ssh_timeout: u16,
    node: Arc<overnet_core::Router>,
) -> Result<HostPipeConnection<HostPipeChildDefaultBuilder<'a>>, anyhow::Error> {
    let host_pipe_child_builder = HostPipeChildDefaultBuilder { ssh_path: "ssh" };
    HostPipeConnection::<HostPipeChildDefaultBuilder<'_>>::spawn_with_builder(
        target,
        host_pipe_child_builder,
        ssh_timeout,
        RETRY_DELAY,
        watchdogs,
        node,
    )
    .await
    .map_err(|e| anyhow!(e))
}

impl<T> HostPipeConnection<T>
where
    T: HostPipeChildBuilder + Copy,
{
    async fn start_child_pipe(
        target: &Weak<Target>,
        builder: T,
        ssh_timeout: u16,
        watchdogs: bool,
        node: T::NodeType,
    ) -> Result<Arc<HostPipeChild>, PipeError> {
        let target = target.upgrade().ok_or(PipeError::TargetGone)?;
        let target_nodename: String = target.nodename_str();
        tracing::debug!("Spawning new host-pipe instance to target {target_nodename}");
        let log_buf = target.host_pipe_log_buffer();
        log_buf.clear();

        let ssh_address =
            target.ssh_address().ok_or(PipeError::NoAddress(target_nodename.clone()))?;

        let (host_addr, cmd) = builder
            .new(
                ssh_address,
                target.id(),
                log_buf.clone(),
                target.events.clone(),
                watchdogs,
                ssh_timeout,
                node,
            )
            .await
            .map_err(|e| PipeError::PipeCreationFailed(e.to_string(), target_nodename.clone()))?;

        *target.ssh_host_address.borrow_mut() = host_addr;
        tracing::debug!(
            "Set ssh_host_address to {:?} for {}@{}",
            target.ssh_host_address,
            target.nodename_str(),
            target.id(),
        );
        if cmd.compatibility_status.is_some() {
            target.set_compatibility_status(&cmd.compatibility_status);
        }
        let hpc = Arc::new(cmd);
        Ok(hpc)
    }

    async fn spawn_with_builder(
        target: Weak<Target>,
        host_pipe_child_builder: T,
        ssh_timeout: u16,
        relaunch_command_delay: Duration,
        watchdogs: bool,
        node: T::NodeType,
    ) -> Result<Self, PipeError> {
        let hpc =
            Self::start_child_pipe(&target, host_pipe_child_builder, ssh_timeout, watchdogs, node)
                .await?;
        let target = target.upgrade().ok_or(PipeError::TargetGone)?;

        Ok(Self {
            target,
            inner: hpc,
            relaunch_command_delay,
            host_pipe_child_builder,
            ssh_timeout,
            watchdogs,
        })
    }

    pub async fn wait(&mut self, node: &T::NodeType) -> Result<(), anyhow::Error> {
        loop {
            // Waits on the running the command. If it exits successfully (disconnect
            // due to peer dropping) then will set the target to disconnected
            // state. If there was an error running the command for some reason,
            // then continue and attempt to run the command again.
            let target_nodename = self.target.nodename();
            let clone = self.inner.clone();
            let res = unblock(move || clone.wait()).await.map_err(|e| {
                anyhow!(
                    "host-pipe error to target {:?} running try-wait: {}",
                    target_nodename,
                    e.to_string()
                )
            });

            tracing::debug!("host-pipe command res: {:?}", res);

            // Keep the ssh_host address in the target. This is the address of the host as seen from
            // the target. It is primarily used when configuring the package server address.
            tracing::debug!(
                "Skipped clearing ssh_host_address for {}@{}",
                self.target.nodename_str(),
                self.target.id()
            );

            match res {
                Ok(_) => {
                    return Ok(());
                }
                Err(e) => tracing::debug!("running cmd on {:?}: {:#?}", target_nodename, e),
            }

            // TODO(fxbug.dev/52038): Want an exponential backoff that
            // is sync'd with an explicit "try to start this again
            // anyway" channel using a select! between the two of them.
            tracing::debug!(
                "waiting {} before restarting child_pipe",
                self.relaunch_command_delay.as_secs()
            );
            Timer::new(self.relaunch_command_delay).await;

            let hpc = Self::start_child_pipe(
                &Rc::downgrade(&self.target),
                self.host_pipe_child_builder,
                self.ssh_timeout,
                self.watchdogs,
                node.clone(),
            )
            .await?;
            self.inner = hpc;
        }
    }

    pub fn get_compatibility_status(&self) -> Option<CompatibilityInfo> {
        self.inner.get_compatibility_status()
    }
}

/// creates the socket for overnet. IoError is possible from socket operations.
fn overnet_pipe(node: Arc<overnet_core::Router>) -> Result<fidl::AsyncSocket, io::Error> {
    let (local_socket, remote_socket) = fidl::Socket::create_stream();
    let local_socket = fidl::AsyncSocket::from_socket(local_socket)?;
    let (errors_sender, errors) = futures::channel::mpsc::unbounded();
    Task::spawn(
        futures::future::join(
            async move {
                if let Err(e) = async move {
                    let (mut rx, mut tx) =
                        fuchsia_async::Socket::from_socket(remote_socket)?.split();
                    circuit::multi_stream::multi_stream_node_connection_to_async(
                        node.circuit_node(),
                        &mut rx,
                        &mut tx,
                        false,
                        circuit::Quality::LOCAL_SOCKET,
                        errors_sender,
                        "remote_control_runner".to_owned(),
                    )
                    .await?;
                    Result::<(), anyhow::Error>::Ok(())
                }
                .await
                {
                    tracing::warn!("Host pipe circuit failed: {:?}", e);
                }
            },
            errors
                .map(|e| {
                    tracing::warn!("A host pipe circuit stream failed: {e:?}");
                })
                .collect::<()>(),
        )
        .map(|((), ())| ()),
    )
    .detach();

    Ok(local_socket)
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use assert_matches::assert_matches;
    use ffx_config::ConfigLevel;
    use serde_json::json;
    use std::fs;
    use std::os::unix::prelude::PermissionsExt;
    use std::process::Command;
    use std::{rc::Rc, str::FromStr};

    const ERR_CTX: &'static str = "running fake host-pipe command for test";

    impl HostPipeChild {
        /// Implements some fake join handles that wait on a join command before
        /// closing. The reader and writer handles don't do anything other than
        /// spin until they receive a message to stop.
        pub fn fake_new(child: &mut Command) -> Self {
            let child = SharedChild::spawn(child).expect(ERR_CTX);
            Self { inner: child, task: Some(Task::local(async {})), compatibility_status: None }
        }
    }

    #[derive(Copy, Clone, Debug)]
    enum ChildOperationType {
        Normal,
        InternalFailure,
        SshFailure,
        DefaultBuilder,
    }

    #[derive(Copy, Clone, Debug)]
    struct FakeHostPipeChildBuilder<'a> {
        operation_type: ChildOperationType,
        ssh_path: &'a str,
    }

    #[async_trait(?Send)]
    impl HostPipeChildBuilder for FakeHostPipeChildBuilder<'_> {
        type NodeType = ();
        async fn new(
            &self,
            addr: SocketAddr,
            id: u64,
            stderr_buf: Rc<LogBuffer>,
            event_queue: events::Queue<TargetEvent>,
            watchdogs: bool,
            ssh_timeout: u16,
            _node: (),
        ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
            match self.operation_type {
                ChildOperationType::Normal => {
                    start_child_normal_operation(addr, id, stderr_buf, event_queue).await
                }
                ChildOperationType::InternalFailure => {
                    start_child_internal_failure(addr, id, stderr_buf, event_queue).await
                }
                ChildOperationType::SshFailure => {
                    start_child_ssh_failure(addr, id, stderr_buf, event_queue).await
                }
                ChildOperationType::DefaultBuilder => {
                    let builder = HostPipeChildDefaultBuilder { ssh_path: self.ssh_path };
                    builder
                        .new(
                            addr,
                            id,
                            stderr_buf,
                            event_queue,
                            watchdogs,
                            ssh_timeout,
                            hoist::Hoist::new(None).unwrap().node(),
                        )
                        .await
                }
            }
        }

        fn ssh_path(&self) -> &str {
            self.ssh_path
        }
    }

    async fn start_child_normal_operation(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        _events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        Ok((
            Some(HostAddr("127.0.0.1".to_string())),
            HostPipeChild::fake_new(
                std::process::Command::new("echo")
                    .arg("127.0.0.1 44315 192.168.1.1 22")
                    .stdout(Stdio::piped())
                    .stdin(Stdio::piped()),
            ),
        ))
    }

    async fn start_child_internal_failure(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        _events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        Err(PipeError::Error(ERR_CTX.into()))
    }

    async fn start_child_ssh_failure(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        events.push(TargetEvent::SshHostPipeErr(HostPipeErr::Unknown("foo".to_string()))).unwrap();
        Ok((
            Some(HostAddr("127.0.0.1".to_string())),
            HostPipeChild::fake_new(
                std::process::Command::new("echo")
                    .arg("127.0.0.1 44315 192.168.1.1 22")
                    .stdout(Stdio::piped())
                    .stdin(Stdio::piped()),
            ),
        ))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_start_and_stop_normal_operation() {
        let target = crate::target::Target::new_with_addrs(
            Some("flooooooooberdoober"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::Normal,
                ssh_path: "ssh",
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await;
        assert_matches!(res, Ok(_));
        // Shouldn't panic when dropped.
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_start_and_stop_internal_failure() {
        // TODO(awdavies): Verify the error matches.
        let target = crate::target::Target::new_with_addrs(
            Some("flooooooooberdoober"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::InternalFailure,
                ssh_path: "ssh",
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await;
        assert!(res.is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_start_and_stop_ssh_failure() {
        let target = crate::target::Target::new_with_addrs(
            Some("flooooooooberdoober"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let events = target.events.clone();
        let task = Task::local(async move {
            events
                .wait_for(None, |e| {
                    assert_matches!(e, TargetEvent::SshHostPipeErr(_));
                    true
                })
                .await
                .unwrap();
        });
        // This is here to allow for the above task to get polled so that the `wait_for` can be
        // placed on at the appropriate time (before the failure occurs in the function below).
        futures_lite::future::yield_now().await;
        let res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::SshFailure,
                ssh_path: "ssh",
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await;
        assert_matches!(res, Ok(_));
        // If things are not setup correctly this will hang forever.
        task.await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_parse_ssh_connection_works() {
        for (line, expected) in [
            (&"++ 192.168.1.1 1234 10.0.0.1 22 ++\n"[..], ("192.168.1.1".to_string(), None)),
            (
                &"++ fe80::111:2222:3333:444 56671 10.0.0.1 22 ++\n",
                ("fe80::111:2222:3333:444".to_string(), None),
            ),
            (
                &"++ fe80::111:2222:3333:444%ethxc2 56671 10.0.0.1 22 ++\n",
                ("fe80::111:2222:3333:444%ethxc2".to_string(), None),
            ),
        ] {
            match parse_ssh_connection(&mut line.as_bytes(), None).await {
                Ok(actual) => assert_eq!(expected, actual),
                res => panic!(
                    "unexpected result for {:?}: expected {:?}, got {:?}",
                    line, expected, res
                ),
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_parse_ssh_connection_errors() {
        for line in [
            // Test for invalid anchors
            &"192.168.1.1 1234 10.0.0.1 22\n"[..],
            &"++192.168.1.1 1234 10.0.0.1 22++\n"[..],
            &"++192.168.1.1 1234 10.0.0.1 22 ++\n"[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22++\n"[..],
            &"++ ++\n"[..],
            &"## 192.168.1.1 1234 10.0.0.1 22 ##\n"[..],
        ] {
            let res = parse_ssh_connection(&mut line.as_bytes(), None).await;
            assert_matches!(res, Err(ParseSshConnectionError::Parse(_)));
        }
        for line in [
            // Truncation
            &"++"[..],
            &"++ 192.168.1.1"[..],
            &"++ 192.168.1.1 1234"[..],
            &"++ 192.168.1.1 1234 "[..],
            &"++ 192.168.1.1 1234 10.0.0.1"[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22"[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22 "[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22 ++"[..],
        ] {
            let res = parse_ssh_connection(&mut line.as_bytes(), None).await;
            assert_matches!(res, Err(ParseSshConnectionError::UnexpectedEOF(_)));
        }
    }

    #[test]
    fn test_log_buffer_empty() {
        let buf = LogBuffer::new(2);
        assert!(buf.lines().is_empty());
    }

    #[test]
    fn test_log_buffer() {
        let buf = LogBuffer::new(2);

        buf.push_line(String::from("1"));
        buf.push_line(String::from("2"));
        buf.push_line(String::from("3"));

        assert_eq!(buf.lines(), vec![String::from("2"), String::from("3")]);
    }

    #[test]
    fn test_clear_log_buffer() {
        let buf = LogBuffer::new(2);

        buf.push_line(String::from("1"));
        buf.push_line(String::from("2"));

        buf.clear();

        assert!(buf.lines().is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_read_ssh_line() {
        // async fn read_ssh_line<R: AsyncBufRead + Unpin>(
        //     lb: &mut LineBuffer,
        //     rdr: &mut R,
        // ) -> std::result::Result<bool, ParseSshConnectionError> {
        let mut lb = LineBuffer::new();
        let input = &"++ 192.168.1.1 1234 10.0.0.1 22 ++\n"[..];
        match read_ssh_line(&mut lb, &mut input.as_bytes()).await {
            Ok(s) => assert_eq!(s, String::from("++ 192.168.1.1 1234 10.0.0.1 22 ++\n")),
            res => panic!("unexpected result: {res:?}"),
        }

        let mut lb = LineBuffer::new();
        let input = &"no newline"[..];
        let res = read_ssh_line(&mut lb, &mut input.as_bytes()).await;
        assert_matches!(res, Err(ParseSshConnectionError::UnexpectedEOF(_)));

        let mut lb = LineBuffer::new();
        let input = [b'A'; 1024];
        let res = read_ssh_line(&mut lb, &mut &input[..]).await;
        assert_matches!(res, Err(ParseSshConnectionError::Parse(_)));

        // Can continue after reading partial result
        let mut lb = LineBuffer::new();
        let input1 = &"foo"[..];
        let _ = read_ssh_line(&mut lb, &mut input1.as_bytes()).await;
        // We'll get a no-newline error, but it has the same semantics as
        // being interrupted due to a timeout
        let input2 = &"bar\n"[..];
        match read_ssh_line(&mut lb, &mut input2.as_bytes()).await {
            Ok(s) => assert_eq!(s, String::from("foobar\n")),
            res => panic!("unexpected result: {res:?}"),
        }
    }

    static INIT: std::sync::Once = std::sync::Once::new();

    #[fuchsia::test]
    async fn test_start_with_failure() {
        let env = ffx_config::test_init().await.unwrap();
        INIT.call_once(|| {
            hoist::init_hoist().expect("init hoist");
        });

        // Set the ssh key paths to something, the contents do no matter for this test.
        env.context
            .query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!([env.isolate_root.path().join("test_authorized_keys")]))
            .await
            .expect("setting ssh pub key");

        let ssh_priv = env.isolate_root.path().join("test_ed25519_key");
        fs::write(&ssh_priv, "test-key").expect("writing test key");
        env.context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([ssh_priv.to_string_lossy()]))
            .await
            .expect("setting ssh priv key");

        let target = crate::target::Target::new_with_addrs(
            Some("test_target"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let _res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::DefaultBuilder,
                ssh_path: "echo",
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await
        .expect_err("host connection");
    }

    #[fuchsia::test]
    async fn test_start_ok() {
        let env = ffx_config::test_init().await.unwrap();
        const SUPPORTED_HOST_PIPE_SH: &str = include_str!("../../test_data/supported_host_pipe.sh");

        let ssh_path = env.isolate_root.path().join("supported_host_pipe.sh");
        fs::write(&ssh_path, SUPPORTED_HOST_PIPE_SH).expect("writing test script");
        fs::set_permissions(&ssh_path, fs::Permissions::from_mode(0o770))
            .expect("setting permissions");

        INIT.call_once(|| {
            hoist::init_hoist().expect("init hoist");
        });

        // Set the ssh key paths to something, the contents do no matter for this test.
        env.context
            .query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!([env.isolate_root.path().join("test_authorized_keys")]))
            .await
            .expect("setting ssh pub key");

        let ssh_priv = env.isolate_root.path().join("test_ed25519_key");
        fs::write(&ssh_priv, "test-key").expect("writing test key");
        env.context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([ssh_priv.to_string_lossy()]))
            .await
            .expect("setting ssh priv key");

        let target = crate::target::Target::new_with_addrs(
            Some("test_target"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let ssh_path_str: String = ssh_path.to_string_lossy().to_string();
        let _res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::DefaultBuilder,
                ssh_path: &ssh_path_str,
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await
        .expect("host connection");
    }

    #[fuchsia::test]
    async fn test_start_legacy_ok() {
        let env = ffx_config::test_init().await.unwrap();
        const SUPPORTED_HOST_PIPE_SH: &str = include_str!("../../test_data/legacy_host_pipe.sh");

        let ssh_path = env.isolate_root.path().join("legacy_host_pipe.sh");
        fs::write(&ssh_path, SUPPORTED_HOST_PIPE_SH).expect("writing test script");
        fs::set_permissions(&ssh_path, fs::Permissions::from_mode(0o770))
            .expect("setting permissions");

        INIT.call_once(|| {
            hoist::init_hoist().expect("init hoist");
        });

        // Set the ssh key paths to something, the contents do no matter for this test.
        env.context
            .query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!([env.isolate_root.path().join("test_authorized_keys")]))
            .await
            .expect("setting ssh pub key");

        let ssh_priv = env.isolate_root.path().join("test_ed25519_key");
        fs::write(&ssh_priv, "test-key").expect("writing test key");
        env.context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([ssh_priv.to_string_lossy()]))
            .await
            .expect("setting ssh priv key");

        let target = crate::target::Target::new_with_addrs(
            Some("test_target"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let ssh_path_str: String = ssh_path.to_string_lossy().to_string();
        let _res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::DefaultBuilder,
                ssh_path: &ssh_path_str,
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await
        .expect("host connection");
    }
}
