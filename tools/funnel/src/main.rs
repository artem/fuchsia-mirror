// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ssh::do_ssh;
use crate::target::choose_target;
use anyhow::anyhow;
use anyhow::{Context, Result};
use argh::FromArgs;
use camino::Utf8PathBuf;
use discovery::{
    wait_for_devices, DiscoverySources, FastbootConnectionState, TargetEvent, TargetState,
};
use futures::Stream;
use futures::StreamExt;
use signal_hook::consts::signal::*;
use signal_hook::iterator::Signals;
use std::sync::Arc;
use std::sync::Mutex;
use std::{collections::HashMap, io, io::Write, time::Duration};
use target::TargetInfo;
use timeout::timeout;
use tracing_subscriber::filter::LevelFilter;

mod logging;
mod ssh;
mod target;
mod update;

fn default_log_level() -> LevelFilter {
    LevelFilter::ERROR
}

fn default_repository_port() -> u32 {
    8083
}

#[derive(FromArgs)]
/// ffx Remote forwarding.
struct Funnel {
    /// the level to log at.
    #[argh(option, short = 'l', default = "default_log_level()")]
    log_level: LevelFilter,

    #[argh(subcommand)]
    nested: FunnelSubcommands,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum FunnelSubcommands {
    Host(SubCommandHost),
    Update(SubCommandUpdate),
    Cleanup(SubCommandCleanupRemote),
    CloseLocalTunnel(SubCommandCloseLocalTunnel),
}

#[derive(FromArgs, PartialEq, Debug)]
/// Remotely forwards a target from your local host to a remote one.
#[argh(subcommand, name = "host")]
struct SubCommandHost {
    /// the remote host to forward to
    #[argh(positional)]
    host: String,

    /// the name of the target to forward
    #[argh(option, short = 't')]
    target_name: Option<String>,

    /// the repository port to forward to the remote host
    #[argh(option, short = 'r', default = "default_repository_port()")]
    repository_port: u32,

    /// additional ports to forward from the remote host to the target.
    #[argh(option, short = 'p')]
    additional_port_forwards: Vec<u32>,

    /// time to wait to discover targets.
    #[argh(option, short = 'w', default = "1")]
    wait_for_target_time: u64,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "self-update")]
/// Perform a self-update.
///
/// This assumes that your machine both has the cipd binary available in your
/// $PATH and that there is a file named funnel-cipd-manifest in the same
/// directory that the `funnel` tool resides in, which is a valid cipd-manifest
struct SubCommandUpdate {}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "cleanup-remote-host")]
/// Cleans up a remote host's connections.
///
/// This will ssh to the provided remote host and look for sshd processes that
/// are actively listening to port 8022. If there are any, then it will kill them.
/// This needs to be run from an interactive shell as it will prompt the user for
/// their password (sudo).
struct SubCommandCleanupRemote {
    /// the remote host to cleanup
    #[argh(positional)]
    host: String,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "close-local-tunnel")]
/// Closes the local tunnel if it exists.
///
/// Instructs the ControlMaster running for funnel to exit
struct SubCommandCloseLocalTunnel {
    /// the remote host to cleanup
    #[argh(positional)]
    host: String,
}

#[fuchsia_async::run_singlethreaded]
async fn main() -> Result<()> {
    let args: Funnel = argh::from_env();

    logging::init(args.log_level)?;

    match args.nested {
        FunnelSubcommands::Host(host_command) => funnel_main(host_command).await,
        FunnelSubcommands::Update(update_command) => update_main(update_command).await,
        FunnelSubcommands::Cleanup(cleanup_command) => cleanup_main(cleanup_command).await,
        FunnelSubcommands::CloseLocalTunnel(close_existing_tunnel) => {
            close_existing_tunnel_main(close_existing_tunnel).await
        }
    }
}

async fn close_existing_tunnel_main(args: SubCommandCloseLocalTunnel) -> Result<()> {
    ssh::close_existing_tunnel(args.host)
}

async fn cleanup_main(args: SubCommandCleanupRemote) -> Result<()> {
    ssh::cleanup_remote_sshd(args.host).await
}

async fn update_main(_args: SubCommandUpdate) -> Result<()> {
    let current_exe_path = std::env::current_exe()?;
    let exe_path_buf = Utf8PathBuf::from_path_buf(current_exe_path)
        .map_err(|p| anyhow!("Non-Utf8 Path passed: {}", p.display()))?;
    update::self_update(exe_path_buf).await.map_err(|e| anyhow!(e))
}

async fn funnel_main(args: SubCommandHost) -> Result<()> {
    tracing::trace!("Discoving targets...");
    let wait_duration = Duration::from_secs(args.wait_for_target_time);

    // Only want added events
    let device_stream =
        wait_for_devices(|_: &_| true, true, false, DiscoverySources::MDNS | DiscoverySources::USB)
            .await?;

    let mut stdout = io::stdout().lock();
    let targets = discover_target_events(&mut stdout, device_stream, wait_duration).await?;

    let mut stdin = io::stdin().lock();
    let target = choose_target(&mut stdin, &mut stdout, targets, args.target_name).await?;
    // Drop the locks we have on stdin and stdout
    drop(stdin);
    drop(stdout);

    tracing::debug!("Target to forward: {:?}", target);
    tracing::info!("Additional port forwards: {:?}", args.additional_port_forwards);
    let host = args.host.clone();
    let do_ssh_fut =
        do_ssh(host.clone(), target, args.repository_port, args.additional_port_forwards);
    // Need to both do ssh and listen for signals
    let mut signals = Signals::new(&[SIGINT]).unwrap();
    let handle = signals.handle();
    // Need something to signal that we exited due to a signal
    let term_requested = Arc::new(Mutex::new(false));
    let thread_reqested = Arc::clone(&term_requested);
    let signal_thread = std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            assert_eq!(signal, SIGINT);
            eprintln!("\nCaught interrupt. Shutting down the tunnel...");
            let res = ssh::close_existing_tunnel(host.clone());
            tracing::debug!("Result from closing existing tunnel: {:?}", res);
            *thread_reqested.lock().unwrap() = true;
            return;
        }
    });

    let do_ssh_res = do_ssh_fut.await;
    tracing::info!("result from do_ssh: {:?}", do_ssh_res);
    match do_ssh_res {
        Ok(_) => {}
        e @ Err(ssh::TunnelError::TunnelAlreadyRunning { remote_host: _ }) => {
            // If this returned an error due to our signal just log and return
            let was_requested = term_requested.lock().unwrap();
            if !*was_requested {
                // We did not get a signal. Error out
                e?;
            }
        }
        e @ _ => {
            // We got an unexpected error
            e?;
        }
    }
    handle.close();
    signal_thread.join().expect("signal thread to shutdown without panic");
    Ok(())
}

async fn discover_target_events<W>(
    w: &mut W,
    mut stream: impl Stream<Item = Result<TargetEvent>> + Unpin,
    wait_time: Duration,
) -> Result<Vec<TargetInfo>>
where
    W: Write,
{
    let mut targets = HashMap::new();
    let task = async {
        while let Some(s) = stream.next().await {
            if let Ok(event) = s {
                match event {
                    TargetEvent::Removed(handle) => {
                        targets.remove(&handle.node_name.unwrap_or_else(|| "".to_string()));
                    }
                    TargetEvent::Added(handle) => match handle.state {
                        TargetState::Product(addr) => {
                            targets
                                .insert(handle.node_name.unwrap_or_else(|| "".to_string()), addr);
                        }
                        TargetState::Fastboot(fb_state) => {
                            match fb_state.connection_state {
                                FastbootConnectionState::Usb(usb_state) => {
                                    tracing::warn!("Discovered Target {} in Fastboot USB mode. It cannot be used with `funnel`. Skipping", usb_state);
                                    writeln!(w, "Discovered Target {} in Fastboot over USB mode, which funnel does not support.", usb_state).context("writing to stdout")?;
                                }
                                FastbootConnectionState::Tcp(tcp_state) => {
                                    // We support fastboot over tcp!
                                    targets.insert(
                                        handle.node_name.unwrap_or_else(|| "".to_string()),
                                        tcp_state,
                                    );
                                }
                                FastbootConnectionState::Udp(udp_state) => {
                                    tracing::warn!("Discovered Target at address: {} in Fastboot Over UDP mode. It cannot be used with `funnel`. Skipping", udp_state);
                                    tracing::warn!("Discovered Target at address: {} in Fastboot Over UDP mode, which funnel does not support.", udp_state);
                                }
                            }
                        }
                        TargetState::Zedboot => {
                            tracing::warn!("Discovered Target in Zedboot mode. It cannot be used with `funnel`. Skipping.");
                            writeln!(
                                w,
                                "Discovered Target in Zedboot mode, which funnel does not support."
                            )?;
                        }
                        TargetState::Unknown => {
                            tracing::warn!("Discovered a target in an unknown state. Skipping");
                            writeln!(w, "Discovered a Target in an unknown state.")?;
                        }
                    },
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    match timeout(wait_time, task).await {
        Ok(_) => {
            tracing::warn!("Got an okay result from the discover target loop.... this shouldnt happen we should always timeout.")
        }
        Err(_) => {
            tracing::trace!("Timeout reached");
        }
    }
    let res = targets
        .iter()
        .map(|(nodename, address)| TargetInfo {
            nodename: nodename.to_string(),
            addresses: vec![*address],
        })
        .collect();
    Ok(res)
}
