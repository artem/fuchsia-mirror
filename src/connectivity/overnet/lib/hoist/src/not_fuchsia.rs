// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(not(target_os = "fuchsia"))]

use anyhow::{bail, format_err, Context, Error};
use fuchsia_async::TimeoutExt;
use fuchsia_async::{Task, Timer};
use futures::channel::mpsc::unbounded;
use futures::prelude::*;
use overnet_core::{Router, RouterOptions};
use std::io::ErrorKind::{self, TimedOut};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{sync::Arc, time::Duration};

pub static CIRCUIT_ID: [u8; 8] = *b"CIRCUIT\0";

pub fn default_ascendd_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push("ascendd");
    path
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Overnet <-> API bindings

#[derive(Debug, Clone)]
pub struct Hoist {
    node: Arc<Router>,
}

impl Hoist {
    pub fn new(router_update_interval: Option<std::time::Duration>) -> Result<Self, Error> {
        let node_id = overnet_core::generate_node_id();
        tracing::trace!(hoist_node_id = node_id.0);
        let router_options = RouterOptions::new()
            .export_diagnostics(fidl_fuchsia_overnet_protocol::Implementation::HoistRustCrate)
            .set_node_id(node_id);
        let router_options = if let Some(router_update_interval) = router_update_interval {
            router_options.set_router_interval(router_update_interval)
        } else {
            router_options
        };
        let node = Router::new(router_options)?;

        Ok(Self { node })
    }

    pub fn node(&self) -> Arc<Router> {
        self.node.clone()
    }

    /// Performs initial configuration with appropriate defaults for the implementation and platform.
    ///
    /// On a fuchsia device this will likely do nothing, so that is the default implementation.
    /// On a host platform it will use the environment variable ASCENDD to find the socket, or
    /// use a default address.
    #[must_use = "Dropped tasks will not run, either hold on to the reference or detach()"]
    pub fn start_default_link(&self) -> Result<Task<()>, Error> {
        Ok(self.start_socket_link(
            std::env::var_os("ASCENDD")
                .map(PathBuf::from)
                .context("No ASCENDD socket provided in environment")?,
        ))
    }

    /// Spawn and return a task that will persistently keep a link connected
    /// to a local ascendd socket. For a single use variant, see
    /// Hoist.run_single_ascendd_link.
    #[must_use = "Dropped tasks will not run, either hold on to the reference or detach()"]
    pub fn start_socket_link(&self, sockpath: PathBuf) -> Task<()> {
        let hoist = self.clone();
        Task::spawn(async move {
            let ascendd_path = sockpath.clone();
            let hoist = hoist.clone();
            retry_with_backoff(Duration::from_millis(100), Duration::from_secs(3), || async {
                hoist.clone().run_single_ascendd_link(ascendd_path.clone()).await
            })
            .await
        })
    }

    /// Start a one-time ascendd connection, attempting to connect to the
    /// unix socket a few times, but only running a single successful
    /// connection to completion. This function will timeout with an
    /// error after one second if no connection could be established.
    pub async fn run_single_ascendd_link(self, sockpath: PathBuf) -> Result<(), Error> {
        const MAX_SINGLE_CONNECT_TIME: u64 = 1;
        let label = connection_label(Option::<String>::None);

        tracing::trace!(ascendd_path = %sockpath.display());
        tracing::trace!(overnet_connection_label = ?label);
        let now = SystemTime::now();

        let unix_socket = loop {
            let safe_socket_path = short_socket_path(&sockpath)?;
            let started = std::time::Instant::now();
            let conn = async_net::unix::UnixStream::connect(&safe_socket_path)
                .on_timeout(Duration::from_secs(30), || {
                    Err(std::io::Error::new(
                        TimedOut,
                        format_err!(
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
                Err(e)
                    if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) =>
                {
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

        run_ascendd_connection(&self, &mut rx, &mut tx).await
    }
}

async fn run_ascendd_connection<'a>(
    hoist: &Hoist,
    rx: &'a mut (dyn AsyncRead + Unpin + Send),
    tx: &'a mut (dyn AsyncWrite + Unpin + Send),
) -> Result<(), Error> {
    let node = hoist.node.clone();
    let (errors_sender, errors) = unbounded();
    tx.write_all(&CIRCUIT_ID).await?;
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
    .map_err(Error::from)
}

/// Retry a future until it succeeds or retries run out.
async fn retry_with_backoff<E, F>(
    backoff0: Duration,
    max_backoff: Duration,
    mut f: impl FnMut() -> F,
) where
    F: futures::Future<Output = Result<(), E>>,
    E: std::fmt::Debug,
{
    let mut backoff = backoff0;
    loop {
        match f().await {
            Ok(()) => {
                backoff = backoff0;
            }
            Err(e) => {
                tracing::warn!("Operation failed: {:?} -- retrying in {:?}", e, backoff);
                Timer::new(backoff).await;
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
        }
    }
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Hacks to hardcode a resource file without resources

const OVERNET_CONNECTION_LABEL: &'static str = "OVERNET_CONNECTION_LABEL";

fn connection_label<S>(o: Option<S>) -> String
where
    S: Into<String>,
{
    let mut connection_label = o.map(Into::into).or(std::env::var(OVERNET_CONNECTION_LABEL).ok());
    if connection_label.is_none() {
        connection_label = std::env::current_exe()
            .ok()
            .map(|p| format!("exe:{} pid:{}", p.display(), std::process::id()));
    }

    match connection_label {
        Some(label) => label,
        None => format!("pid:{}", std::process::id()),
    }
}

/// If necessary, holds a tempdir open with a symlink to a socket path
/// that is too long to fit in the system's SUN_LEN.
#[derive(Debug)]
pub struct ShortPathLink {
    /// The shorthand path we're using to connect to the daemon
    pub short_path: PathBuf,
    /// The temporary directory handle we're putting the socket file in
    _temp_location: Option<tempfile::TempDir>,
}

impl ShortPathLink {
    // there seems to be no standard binding available for the SUN_LEN constant
    // in std, libc, or nix, so we'll just conservatively guess 100 for now.
    pub const MAX_SUN_LEN: usize = 100;
}

impl AsRef<Path> for ShortPathLink {
    fn as_ref(&self) -> &Path {
        self.short_path.as_ref()
    }
}

/// If `real_path` is too long to fit in a socket bind/connect struct,
/// creates a symlink in the tempdir that points to the 'real' socket
/// path.
///
/// Returns a [`ShortPathSocket`] that keeps the reference alive
/// while it's being connected to.
pub fn short_socket_path(real_path: &Path) -> std::io::Result<ShortPathLink> {
    #[cfg(not(target_os = "windows"))]
    use std::os::unix::fs::symlink;
    #[cfg(target_os = "windows")]
    use std::os::windows::fs::symlink_dir as symlink;

    let short_path;
    let temp_location;
    if real_path.as_os_str().len() > ShortPathLink::MAX_SUN_LEN {
        // we make a symlink from the original home of the socket to a (hopefully shorter) tmpdir path,
        // and then return a path that looks into that symlink to find the socket. This avoids a bunch of
        // annoying situations around things trying to create the socket when it doesn't already exist.
        let socket_filename = real_path.file_name().ok_or_else(|| {
            let error_str = format!(
                "{real_path} did not have a filename component",
                real_path = real_path.display()
            );
            std::io::Error::new(ErrorKind::InvalidInput, error_str)
        })?;
        let socket_dir = real_path.parent().ok_or_else(|| {
            let error_str = format!(
                "{real_path} did not have a path component",
                real_path = real_path.display()
            );
            std::io::Error::new(ErrorKind::InvalidInput, error_str)
        })?;

        let tempdir = tempfile::tempdir()?;
        let symlink_path = tempdir.path().join("root");

        short_path = symlink_path.join(socket_filename).to_owned();
        if short_path.as_os_str().len() > ShortPathLink::MAX_SUN_LEN {
            let error_str = format!(
                "Even tmpdir path was too long to create a short enough socket path for {real_path} (tried: {short_path})",
                real_path=real_path.display(),
                short_path=short_path.display());
            return Err(std::io::Error::new(ErrorKind::InvalidInput, error_str));
        }
        symlink(socket_dir, &symlink_path)?;
        temp_location = Some(tempdir);
    } else {
        short_path = real_path.to_owned();
        temp_location = None;
    }
    Ok(ShortPathLink { short_path, _temp_location: temp_location })
}

#[cfg(test)]
mod test {
    use super::*;
    use scopeguard::guard;

    #[fuchsia::test]
    fn test_connection_label() {
        let original = std::env::var_os(OVERNET_CONNECTION_LABEL);
        guard(original, |orig| {
            orig.map(|v| std::env::set_var(OVERNET_CONNECTION_LABEL, v));
        });

        std::env::remove_var(OVERNET_CONNECTION_LABEL);

        let cs = connection_label(Option::<String>::None);
        // Note: conditional test is not great, but covers where cover works.
        if let Ok(path) = std::env::current_exe() {
            assert!(cs.contains(&path.to_string_lossy().to_string()));
        }

        assert!(cs.contains(&format!("pid:{}", std::process::id())));

        std::env::set_var(OVERNET_CONNECTION_LABEL, "onetwothree");
        assert_eq!("onetwothree", connection_label(Option::<String>::None));

        assert_eq!("precedence", connection_label(Some("precedence")));
    }

    #[fuchsia::test]
    fn test_shortened_path() {
        let test_temp = tempfile::tempdir().expect("creating tempdir for tests");
        let already_short_path = test_temp.path().join("a").join("my.sock");
        let shortened_short_path =
            short_socket_path(&already_short_path).expect("creating short path");
        assert_eq!(
            already_short_path, shortened_short_path.short_path,
            "Should not have done anything to already short path"
        );

        let long_dir = test_temp.path().join("a".to_owned().repeat(ShortPathLink::MAX_SUN_LEN + 1));
        let long_path = long_dir.join("my.sock");
        std::fs::create_dir_all(&long_dir).expect("Creating long directory name should work");
        std::fs::File::create(&long_path).expect("Creating file in long directory should work");
        let shortened_long_path = short_socket_path(&long_path).expect("creating short path");
        assert_ne!(
            long_path, shortened_long_path.short_path,
            "Should have generated a shorter path for a long path"
        );
        assert!(
            shortened_long_path.short_path.as_os_str().len() < ShortPathLink::MAX_SUN_LEN,
            "new short path should be shorter than the maximum length"
        );
        assert_eq!(
            &std::fs::canonicalize(&shortened_long_path.short_path)
                .expect("read link should resolve"),
            &long_path,
            "short path link should resolve to original long path"
        )
    }
}
