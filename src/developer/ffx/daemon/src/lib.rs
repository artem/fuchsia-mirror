// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Result};
use daemonize::daemonize;
use errors::{ffx_error, FfxError};
use ffx_config::{logging::LogDirHandling, EnvironmentContext};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_developer_ffx::{DaemonMarker, DaemonProxy};
use fidl_fuchsia_overnet_protocol::NodeId;
use fuchsia_async::{TimeoutExt, Timer};
use futures::prelude::*;
use nix::sys::signal;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Child, Command},
    sync::Arc,
    time::{Duration, SystemTime},
};

mod config;
mod constants;
mod socket;

pub use config::*;

pub use constants::DAEMON_LOG_FILENAME;

pub use socket::SocketDetails;

async fn create_daemon_proxy(
    node: &Arc<overnet_core::Router>,
    id: &mut NodeId,
) -> Result<DaemonProxy> {
    let (s, p) = fidl::Channel::create();
    node.connect_to_service((*id).into(), DaemonMarker::PROTOCOL_NAME, s).await?;
    let proxy = fidl::AsyncChannel::from_channel(p);
    Ok(DaemonProxy::new(proxy))
}

/// Start a one-time ascendd connection, attempting to connect to the
/// unix socket a few times, but only running a single successful
/// connection to completion. This function will timeout with an
/// error after one second if no connection could be established.
pub async fn run_single_ascendd_link(
    node: Arc<overnet_core::Router>,
    sockpath: PathBuf,
) -> Result<(), anyhow::Error> {
    const MAX_SINGLE_CONNECT_TIME: u64 = 1;

    tracing::trace!(ascendd_path = %sockpath.display());
    let now = SystemTime::now();

    let unix_socket = loop {
        let safe_socket_path = ascendd::short_socket_path(&sockpath)?;
        let started = std::time::Instant::now();
        let conn = async_net::unix::UnixStream::connect(&safe_socket_path)
            .on_timeout(Duration::from_secs(30), || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    anyhow::format_err!(
                        "Timed out (30s) connecting to ascendd socket at {}",
                        sockpath.display()
                    ),
                ))
            })
            .await;
        match conn {
            // We got our connections.
            Ok(conn) => {
                let elapsed = std::time::Instant::now() - started;
                if elapsed.as_millis() > 100 {
                    tracing::warn!("Socket connection took {elapsed:?}");
                }
                break conn;
            }
            // There was an error connecting that's likely due to the daemon not being ready yet.
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
                if now.elapsed()?.as_secs() > MAX_SINGLE_CONNECT_TIME {
                    bail!(
                        "took too long connecting to ascendd socket at {}. Last error: {e:#?}",
                        sockpath.display(),
                    );
                }
            }
            // There was an unknown error connecting.
            Err(e) => {
                bail!(
                    "unexpected error while trying to connect to ascendd socket at {}: {e:?}",
                    sockpath.display()
                );
            }
        }
    };

    let (mut rx, mut tx) = unix_socket.split();

    run_ascendd_connection(node, &mut rx, &mut tx).await
}

async fn run_ascendd_connection<'a>(
    node: Arc<overnet_core::Router>,
    rx: &'a mut (dyn AsyncRead + Unpin + Send),
    tx: &'a mut (dyn AsyncWrite + Unpin + Send),
) -> Result<(), anyhow::Error> {
    let (errors_sender, errors) = futures::channel::mpsc::unbounded();
    tx.write_all(&ascendd::CIRCUIT_ID).await?;
    futures::future::join(
        circuit::multi_stream::multi_stream_node_connection_to_async(
            node.circuit_node(),
            rx,
            tx,
            false,
            circuit::Quality::LOCAL_SOCKET,
            errors_sender,
            "ascendd".to_owned(),
        ),
        errors
            .map(|e| {
                tracing::warn!("An ascendd circuit failed: {e:?}");
            })
            .collect::<()>(),
    )
    .map(|(result, ())| result)
    .await
    .map_err(anyhow::Error::from)
}

pub async fn get_daemon_proxy_single_link(
    node: &Arc<overnet_core::Router>,
    socket_path: PathBuf,
    exclusions: Option<Vec<NodeId>>,
) -> Result<(NodeId, DaemonProxy, Pin<Box<impl Future<Output = Result<()>>>>), FfxError> {
    // Start a race betwen:
    // - The unix socket link being lost
    // - A timeout
    // - Getting a FIDL proxy over the link

    let link = run_single_ascendd_link(Arc::clone(node), socket_path.clone()).fuse();
    let mut link = Box::pin(link);
    let find = find_next_daemon(node, exclusions).fuse();
    let mut find = Box::pin(find);
    let mut timeout = Timer::new(Duration::from_secs(5)).fuse();

    let res = futures::select! {
        r = link => {
            Err(ffx_error!("Daemon link lost while attempting to connect to socket {}: {:#?}\nRun `ffx doctor` for further diagnostics.", socket_path.display(), r))
        }
        _ = timeout => {
            Err(ffx_error!("Timed out waiting for the ffx daemon on the Overnet mesh over socket {}.\nRun `ffx doctor --restart-daemon` for further diagnostics.", socket_path.display()))
        }
        proxy = find => proxy.map_err(|e| ffx_error!("Error connecting to Daemon at socket: {}: {:#?}\nRun `ffx doctor` for further diagnostics.", socket_path.display(), e)),
    };
    res.map(|(nodeid, proxy)| (nodeid, proxy, link))
}

async fn find_next_daemon<'a>(
    node: &Arc<overnet_core::Router>,
    exclusions: Option<Vec<NodeId>>,
) -> Result<(NodeId, DaemonProxy)> {
    let lpc = node.new_list_peers_context().await;
    loop {
        let peers = lpc.list_peers().await?;
        for peer in peers.iter() {
            if !peer.services.iter().any(|name| *name == DaemonMarker::PROTOCOL_NAME) {
                continue;
            }
            match exclusions {
                Some(ref exclusions) => {
                    if exclusions.iter().any(|n| *n == peer.node_id.into()) {
                        continue;
                    }
                }
                None => {}
            }
            return create_daemon_proxy(node, &mut peer.node_id.into())
                .await
                .map(|proxy| (peer.node_id.into(), proxy));
        }
    }
}

// Note that this function assumes the daemon has been started separately.
pub async fn find_and_connect(
    node: &Arc<overnet_core::Router>,
    socket_path: PathBuf,
) -> Result<DaemonProxy> {
    // This function is due for deprecation/removal. It should only be used
    // currently by the doctor daemon_manager, which should instead learn to
    // understand the link state in future revisions.
    get_daemon_proxy_single_link(node, socket_path, None)
        .await
        .map(|(_nodeid, proxy, link_fut)| {
            fuchsia_async::Task::local(link_fut.map(|_| ())).detach();
            proxy
        })
        .context("connecting to the ffx daemon")
}

// We have both spawn_daemon(), and run_daemon() below, because we want
// the daemon to be handled in two different ways. "Real" invocations use
// spawn_daemon(), because it daemonizes the process, disconnecting it from the
// controlling terminal, etc.  "Test" invocations uses run_daemon(), because
// they want to have control of the child process, running wait(), etc.
#[tracing::instrument]
pub async fn spawn_daemon(context: &EnvironmentContext) -> Result<()> {
    let mut cmd = daemon_cmd(context).await?;
    tracing::info!("Starting new background ffx daemon from {:?}", &cmd.get_program());
    daemonize(&mut cmd)
        .spawn()
        .context("spawning daemon start")?
        .wait()
        .map(|_| ())
        .context("waiting for daemon start")
}

// See the above comment for spawn_daemon(). This function is only used by the
// "ffx self-test" function `test_config_flag()`.
#[tracing::instrument]
pub async fn run_daemon(context: &EnvironmentContext) -> Result<Child> {
    let mut cmd = daemon_cmd(context).await?;
    tracing::info!("Starting new ffx daemon from {:?}", &cmd.get_program());
    let child = cmd.spawn().context("running daemon start")?;
    Ok(child)
}

#[tracing::instrument]
async fn daemon_cmd(context: &EnvironmentContext) -> Result<Command> {
    use std::process::Stdio;

    let mut cmd = context.rerun_prefix().await?;
    let socket_path = context.get_ascendd_path().await.context("No socket path configured")?;

    let mut stdout = Stdio::null();
    let mut stderr = Stdio::null();

    if ffx_config::logging::is_enabled(context).await {
        let file = PathBuf::from(DAEMON_LOG_FILENAME);
        stdout = Stdio::from(
            ffx_config::logging::log_file(context, &file, LogDirHandling::WithDirWithRotate)
                .await?,
        );
        // Third argument says not to rotate the logs.  We rotated the logs once
        // for the call above, we shouldn't do it again.
        stderr = Stdio::from(
            ffx_config::logging::log_file(context, &file, LogDirHandling::WithDirWithoutRotate)
                .await?,
        );
    }

    cmd.stdin(Stdio::null()).stdout(stdout).stderr(stderr).env("RUST_BACKTRACE", "full");
    // Note: daemon start produces output on stdout by default -- see ffx_command::run()
    cmd.arg("daemon");
    cmd.arg("start");
    cmd.arg("--path").arg(socket_path);
    Ok(cmd)
}

// Time between polling to see if process has exited
const STOP_WAIT_POLL_TIME: Duration = Duration::from_millis(50);

pub async fn try_to_kill_pid(pid: u32) -> Result<()> {
    // UNIX defines a pid as a _signed_ int -- who knew?
    let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
    let res = signal::kill(nix_pid, Some(signal::Signal::SIGTERM));
    match res {
        // No longer there
        Err(nix::errno::Errno::ESRCH) => return Ok(()),
        // Kill failed for some other reason
        Err(e) => bail!("Could not kill daemon: {e}"),
        // The kill() worked, i.e. the process was still around
        _ => (),
    }
    // This had been just 50ms, but that sometimes wasn't enough time for
    // systemd (ppid 1) to swap in and reap the zombie child, so let's increase
    // it.
    // This code is only invoked in tests and will only happen when the daemon
    // didn't exit cleanly, so a sizeable delay isn't a terrible thing.
    Timer::new(STOP_WAIT_POLL_TIME * 5).await;
    // Check to see if it actually died
    let res = signal::kill(nix_pid, None);
    match res {
        // No longer there
        Err(nix::errno::Errno::ESRCH) => return Ok(()),
        // Kill failed for some other reason
        Err(e) => bail!("Could not kill daemon: {e}"),
        // The kill() worked, i.e. the process was still around
        _ => {
            // Let's find out what happened
            let stat_file = format!("/proc/{pid}/status");
            let status = match std::fs::read_to_string(&stat_file)
                .with_context(|| format!("could not cat {stat_file}"))
            {
                Ok(s) => s,
                Err(e) => format!("[Failed to read /proc] {e}"),
            };

            bail!("Daemon did not exit. Giving up.  Proc status: {status}")
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// start

#[tracing::instrument]
pub fn is_daemon_running_at_path(socket_path: &Path) -> bool {
    // Not strictly necessary check, but improves log output for diagnostics
    match std::fs::metadata(socket_path) {
        Ok(_) => {}
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no daemon found at {}", socket_path.display());
            return false;
        }
        Err(e) => {
            tracing::info!("error stating {}: {}", socket_path.display(), e);
            // speculatively carry on
        }
    }

    let sock = ascendd::short_socket_path(&socket_path)
        .and_then(|safe_socket_path| std::os::unix::net::UnixStream::connect(&safe_socket_path));
    match sock {
        Ok(sock) => match sock.peer_addr() {
            Ok(_) => {
                tracing::debug!("found running daemon at {}", socket_path.display());
                true
            }
            Err(err) => {
                tracing::info!(
                    "found daemon socket at {} but could not see peer: {}",
                    socket_path.display(),
                    err
                );
                false
            }
        },
        Err(err) => {
            tracing::info!("failed to connect to daemon at {}: {}", socket_path.display(), err);
            false
        }
    }
}
