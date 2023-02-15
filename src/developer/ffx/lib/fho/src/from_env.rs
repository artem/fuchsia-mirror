// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use ffx_command::{Error, FfxContext, Result};
use ffx_fidl::DaemonError;
use ffx_writer::ToolIO;
use fidl::endpoints::Proxy;
use fidl_fuchsia_developer_ffx as ffx_fidl;
use selectors::{self, VerboseError};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::time::Duration;
use std::{rc::Rc, sync::Arc};

use crate::FhoEnvironment;

#[async_trait(?Send)]
pub trait TryFromEnv: Sized {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self>;
}

#[async_trait(?Send)]
pub trait CheckEnv {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()>;
}

#[async_trait(?Send)]
pub trait TryFromEnvWith: 'static {
    type Output: 'static;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output>;
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Arc<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        T::try_from_env(env).await.map(Arc::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Rc<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        T::try_from_env(env).await.map(Rc::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Box<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        T::try_from_env(env).await.map(Box::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Result<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(T::try_from_env(env).await)
    }
}

/// Checks if the experimental config flag is set. This gates the execution of the command.
/// If the flag is set to `true`, this returns `Ok(())`, else returns an error.
pub struct AvailabilityFlag<T>(pub T);

#[async_trait(?Send)]
impl<T: AsRef<str>> CheckEnv for AvailabilityFlag<T> {
    async fn check_env(self, _env: &FhoEnvironment) -> Result<()> {
        let flag = self.0.as_ref();
        if ffx_config::get(flag).await.unwrap_or(false) {
            Ok(())
        } else {
            ffx_bail!(
                "This is an experimental subcommand.  To enable this subcommand run 'ffx config set {} true'",
                flag
            );
        }
    }
}

/// Allows you to defer the initialization of an object in your tool struct
/// until you need it (if at all) or apply additional combinators on it (like
/// custom timeout logic or anything like that).
///
/// If you need to defer something that requires a decorator, use the
/// [`deferred`] decorator around it.
///
/// Example:
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     daemon: fho::Deferred<fho::DaemonProxy>,
/// }
/// impl fho::FfxMain for Tool {
///     type Writer = fho::SimpleWriter;
///     async fn main(self, _writer: fho::SimpleWriter) -> fho::Result<()> {
///         let daemon = self.daemon.await?;
///         writeln!(writer, "Loaded the daemon proxy!");
///     }
/// }
/// ```
pub struct Deferred<T: 'static>(Pin<Box<dyn Future<Output = Result<T>>>>);
#[async_trait(?Send)]
impl<T> TryFromEnv for Deferred<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let env = env.clone();
        Ok(Self(Box::pin(async move { T::try_from_env(&env).await })))
    }
}

/// The implementation of the decorator returned by [`deferred`]
pub struct WithDeferred<T>(T);
#[async_trait(?Send)]
impl<T> TryFromEnvWith for WithDeferred<T>
where
    T: TryFromEnvWith + 'static,
{
    type Output = Deferred<T::Output>;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let env = env.clone();
        Ok(Deferred(Box::pin(async move { self.0.try_from_env_with(&env).await })))
    }
}

/// A decorator for proxy types in [`crate::FfxTool`] implementations so you can
/// specify the selector string for the proxy you're loading.
///
/// Example:
///
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     #[with(fho::deferred(fho::selector("core/selector/thing")))]
///     foo_proxy: fho::Deferred<FooProxy>,
/// }
/// ```
pub fn deferred<T: TryFromEnvWith>(inner: T) -> WithDeferred<T> {
    WithDeferred(inner)
}

impl<T> Future for Deferred<T> {
    type Output = Result<T>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.0.as_mut().poll(cx)
    }
}

/// Gets the actively configured SDK from the environment
#[async_trait(?Send)]
impl TryFromEnv for ffx_config::Sdk {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.context.get_sdk().await.user_message("Could not load currently active SDK")
    }
}

/// The implementation of the decorator returned by [`selector`] and [`selector_timeout`]
pub struct WithSelector<P> {
    selector: String,
    timeout: Duration,
    _p: PhantomData<fn() -> P>,
}

#[async_trait(?Send)]
impl<P: Proxy + 'static> TryFromEnvWith for WithSelector<P> {
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>()
            .with_user_message(|| {
                format!("Failed creating proxy for selector {}", self.selector)
            })?;
        let _ = selectors::parse_selector::<VerboseError>(&self.selector)
            .with_bug_context(|| format!("Parsing selector {}", self.selector))?;
        let retry_count = 1;
        let mut tries = 0;
        // TODO(fxbug.dev/113143): Remove explicit retries/timeouts here so they can be
        // configurable instead.
        let rcs_instance = loop {
            tries += 1;
            let res = env.injector.remote_factory().await;
            if res.is_ok() || tries > retry_count {
                break res;
            }
        }?;
        rcs::connect_with_timeout(
            self.timeout,
            &self.selector,
            &rcs_instance,
            server_end.into_channel(),
        )
        .await?;
        Ok(proxy)
    }
}

/// A decorator for proxy types in [`crate::FfxTool`] implementations so you can
/// specify the selector string for the proxy you're loading.
///
/// Example:
///
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     #[with(fho::selector("core/selector/thing"))]
///     foo_proxy: FooProxy,
/// }
/// ```
pub fn selector<P: Proxy>(selector: impl AsRef<str>) -> WithSelector<P> {
    selector_timeout(selector, 15)
}

/// Like [`selector`], but lets you also specify an override for the default
/// timeout.
pub fn selector_timeout<P: Proxy>(selector: impl AsRef<str>, timeout_secs: u64) -> WithSelector<P> {
    WithSelector {
        selector: selector.as_ref().to_owned(),
        timeout: Duration::from_secs(timeout_secs),
        _p: Default::default(),
    }
}

#[derive(Debug, Clone)]
pub struct DaemonProtocol<P: Clone> {
    proxy: P,
}

impl<P: Clone> DaemonProtocol<P> {
    pub fn new(proxy: P) -> Self {
        Self { proxy }
    }
}

impl<P: Clone> DaemonProtocol<P> {
    pub fn into_inner(self) -> P {
        self.proxy
    }
}

impl<P: Clone> std::ops::Deref for DaemonProtocol<P> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        &self.proxy
    }
}

fn map_daemon_error(svc_name: &str, err: DaemonError) -> Error {
    match err {
        DaemonError::ProtocolNotFound => ffx_error!(
            "The daemon protocol '{svc_name}' did not match any protocols on the daemon
If you are not developing this plugin or the protocol it connects to, then this is a bug

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        DaemonError::ProtocolOpenError => ffx_error!(
            "The daemon protocol '{svc_name}' failed to open on the daemon.

If you are developing the protocol, there may be an internal failure when invoking the start
function. See the ffx.daemon.log for details at `ffx config get log.dir -p sub`.

If you are NOT developing this plugin or the protocol it connects to, then this is a bug.

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        unexpected => ffx_error!(
"While attempting to open the daemon protocol '{svc_name}', received an unexpected error:

{unexpected:?}

This is not intended behavior and is a bug.
Please report it at http://fxbug.dev/new/ffx+User+Bug."

        ),
    }
    .into()
}

#[async_trait(?Send)]
impl<P: Proxy + Clone> TryFromEnv for DaemonProtocol<P>
where
    P::Protocol: fidl::endpoints::DiscoverableProtocolMarker,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let svc_name = <P::Protocol as fidl::endpoints::DiscoverableProtocolMarker>::PROTOCOL_NAME;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>()
            .with_user_message(|| format!("Failed creating proxy for service {}", svc_name))?;
        let daemon = env.injector.daemon_factory().await?;

        daemon
            .connect_to_protocol(svc_name, server_end.into_channel())
            .await
            .bug_context("Connecting to protocol")?
            .map_err(|err| map_daemon_error(svc_name, err))
            .map(|_| DaemonProtocol { proxy })
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::DaemonProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.daemon_factory().await.user_message("Failed to create daemon proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for Option<ffx_fidl::DaemonProxy> {
    /// Attempts to connect to the ffx daemon, returning Ok(None) if no instance of the daemon is
    /// started. If you would like to use the normal flow of attempting to connect to the daemon,
    /// and starting a new instance of the daemon if none is currently present, you should use the
    /// impl for `ffx_fidl::DaemonProxy`, which returns a `Result<ffx_fidl::DaemonProxy>`.
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let res = env
            .injector
            .try_daemon()
            .await
            .user_message("Failed internally while checking for daemon.")?;
        Ok(res)
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::TargetProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.target_factory().await.user_message("Failed to create target proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::FastbootProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.fastboot_factory().await.user_message("Failed to create fastboot proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for fidl_fuchsia_developer_remotecontrol::RemoteControlProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.remote_factory().await.user_message("Failed to create remote control proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_writer::Writer {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.writer().await.user_message("Failed to create writer")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_writer::SimpleWriter {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.ffx.global.machine.is_some() {
            Err(Error::User(anyhow::anyhow!(
                "The machine flag is not supported for this subcommand"
            )))
        } else {
            Ok(ffx_writer::SimpleWriter::new())
        }
    }
}

#[async_trait(?Send)]
impl<T: serde::Serialize> TryFromEnv for ffx_writer::MachineWriter<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.ffx.global.machine.is_some() && !Self::is_machine_supported() {
            Err(Error::User(anyhow::anyhow!(
                "The machine flag is not supported for this subcommand"
            )))
        } else {
            Ok(ffx_writer::MachineWriter::new(env.ffx.global.machine))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct AlwaysError;
    #[async_trait(?Send)]
    impl TryFromEnv for AlwaysError {
        async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
            Err(Error::User(anyhow::anyhow!("boom")))
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_deferred_err() {
        let config_env = ffx_config::test_init().await.unwrap();
        let tool_env = crate::testing::ToolEnv::new().make_environment(config_env.context.clone());

        Deferred::<AlwaysError>::try_from_env(&tool_env)
            .await
            .expect("Deferred result should be Ok")
            .await
            .expect_err("Inner AlwaysError should error after second await");
    }
}
