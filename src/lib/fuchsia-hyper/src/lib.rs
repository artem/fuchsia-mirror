// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    futures::{
        future::{Future, FutureExt},
        io::{self, AsyncRead, AsyncWrite},
        task::{Context, Poll},
    },
    hyper::{
        client::{
            connect::{Connected, Connection},
            Client,
        },
        Body,
    },
    std::{
        marker::PhantomData,
        net::{
            AddrParseError, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, SocketAddrV6,
        },
        num::ParseIntError,
        pin::Pin,
    },
    tokio::io::ReadBuf,
};

#[cfg(not(target_os = "fuchsia"))]
use async_net as net;

#[cfg(target_os = "fuchsia")]
use fuchsia_async::net;

#[cfg(not(target_os = "fuchsia"))]
mod not_fuchsia;
#[cfg(not(target_os = "fuchsia"))]
pub use not_fuchsia::*;

#[cfg(target_os = "fuchsia")]
mod fuchsia;
#[cfg(target_os = "fuchsia")]
pub use crate::fuchsia::*;

mod session_cache;
pub use session_cache::C4CapableSessionCache;

#[cfg(target_os = "fuchsia")]
mod happy_eyeballs;

/// A Fuchsia-compatible hyper client configured for making HTTP requests.
pub type HttpClient = Client<HyperConnector, Body>;

/// A Fuchsia-compatible hyper client configured for making HTTP and HTTPS requests.
pub type HttpsClient = Client<hyper_rustls::HttpsConnector<HyperConnector>, Body>;

/// A trait to implement a builder for a Fuchsia compatible hyper client
/// configured for either only HTTP or both HTTP and HTTPS requests.
pub trait MakeClientBuilder: Sized {
    fn builder() -> HttpClientBuilder<Self> {
        HttpClientBuilder::default()
    }
}

impl MakeClientBuilder for HttpClient {}
impl MakeClientBuilder for HttpsClient {}

/// A future that yields a hyper-compatible TCP stream.
#[must_use = "futures do nothing unless polled"]
pub struct HyperConnectorFuture {
    // FIXME(https://github.com/rust-lang/rust/issues/63063): We should be able to remove this
    // `Box` once rust allows impl Traits in type aliases.
    fut: Pin<Box<dyn Future<Output = Result<TcpStream, io::Error>> + Send>>,
}

impl Future for HyperConnectorFuture {
    type Output = Result<TcpStream, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.fut.as_mut().poll(cx)
    }
}

pub struct TcpStream {
    pub stream: net::TcpStream,
}

impl tokio::io::AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf.initialize_unfilled()).map_ok(|sz| {
            buf.advance(sz);
            ()
        })
    }

    // TODO: override poll_read_buf and call readv on the underlying stream
}

impl tokio::io::AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.get_mut().stream.shutdown(Shutdown::Write))
    }

    // TODO: override poll_write_buf and call writev on the underlying stream
}

impl Connection for TcpStream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// A container of TCP settings to be applied to the sockets created by the hyper client.
pub struct TcpOptions {
    /// This sets TCP_KEEPIDLE and SO_KEEPALIVE.
    pub keepalive_idle: Option<std::time::Duration>,
    /// This sets TCP_KEEPINTVL and SO_KEEPALIVE.
    pub keepalive_interval: Option<std::time::Duration>,
    /// This sets TCP_KEEPCNT and SO_KEEPALIVE.
    pub keepalive_count: Option<u32>,
}

impl TcpOptions {
    /// keepalive_timeout returns a TCP keepalive policy that times out after the specified
    /// duration. The keepalive policy returned waits for half of the supplied duration before
    /// sending keepalive packets, and attempts to keep the connection alive three times for
    /// the remaining period.
    ///
    /// If the supplied duration does not contain at least one whole second, no TCP keepalive
    /// policy is returned.
    pub fn keepalive_timeout(dur: std::time::Duration) -> Self {
        if dur.as_secs() == 0 {
            return TcpOptions::default();
        }

        TcpOptions {
            keepalive_idle: dur.checked_div(2),
            keepalive_interval: dur.checked_div(6),
            keepalive_count: Some(3),
        }
    }

    pub(crate) fn apply<T: std::os::fd::AsRawFd>(&self, stream: &T) -> io::Result<()> {
        let stream = socket2::SockRef::from(stream);
        let mut any = false;
        let mut keepalive = socket2::TcpKeepalive::new();
        if let Some(idle) = self.keepalive_idle {
            any = true;
            keepalive = keepalive.with_time(idle);
        };
        if let Some(interval) = self.keepalive_interval {
            any = true;
            keepalive = keepalive.with_interval(interval);
        }
        if let Some(count) = self.keepalive_count {
            any = true;
            keepalive = keepalive.with_retries(count);
        }
        if any {
            stream.set_tcp_keepalive(&keepalive)
        } else {
            Ok(())
        }
    }
}

/// Extra socket options to ensure that requests are made through a particular
/// device or over a particular IP domain.
#[derive(Clone, Debug, Default)]
pub struct SocketOptions {
    /// Specifies, as a string, the device that the created socket should bind to.
    pub bind_device: Option<String>,
}

#[derive(Clone)]
pub struct Executor;

impl<F: Future + Send + 'static> hyper::rt::Executor<F> for Executor {
    fn execute(&self, fut: F) {
        fuchsia_async::Task::spawn(fut.map(|_| ())).detach()
    }
}

#[derive(Clone)]
pub struct LocalExecutor;

/// Implements the Builder pattern for constructing HTTP or HTTPS clients.
#[derive(Clone)]
pub struct HttpClientBuilder<T> {
    tcp_options: Option<TcpOptions>,
    socket_options: Option<SocketOptions>,
    tls: Option<rustls::ClientConfig>,
    phantom: PhantomData<T>,
}

impl<T> Default for HttpClientBuilder<T> {
    fn default() -> Self {
        Self {
            tcp_options: Default::default(),
            socket_options: Default::default(),
            tls: Default::default(),
            phantom: Default::default(),
        }
    }
}

impl HttpClientBuilder<HttpClient> {
    /// Constructs an HttpClient
    pub fn build(mut self) -> HttpClient {
        Client::builder().executor(Executor).build(self.connector())
    }
}

impl HttpClientBuilder<HttpsClient> {
    /// Constructs an HttpsClient
    pub fn build(mut self) -> HttpsClient {
        let https = hyper_rustls::HttpsConnector::from((
            self.connector(),
            self.tls.unwrap_or_else(|| {
                let mut tls = new_rustls_client_config();
                configure_cert_store(&mut tls);
                tls
            }),
        ));
        Client::builder().executor(Executor).build(https)
    }

    /// Overrides the default tls `ClientConfig`
    pub fn tls(self, tls: rustls::ClientConfig) -> Self {
        Self { tls: Some(tls), ..self }
    }
}

impl<T> HttpClientBuilder<T> {
    fn connector(&mut self) -> HyperConnector {
        HyperConnector::from((
            self.tcp_options.take().unwrap_or_default(),
            self.socket_options.take().unwrap_or_default(),
        ))
    }

    /// Sets the TCP options for the underlying HyperConnector
    pub fn tcp_options(self, tcp_options: TcpOptions) -> Self {
        Self { tcp_options: Some(tcp_options), ..self }
    }

    /// Sets the SocketOptions for the underlying HyperConnector
    pub fn socket_options(self, socket_options: SocketOptions) -> Self {
        Self { socket_options: Some(socket_options), ..self }
    }
}

impl<F: Future + 'static> hyper::rt::Executor<F> for LocalExecutor {
    fn execute(&self, fut: F) {
        fuchsia_async::Task::local(fut.map(drop)).detach()
    }
}

/// Returns a new Fuchsia-compatible hyper client for making HTTP requests.
pub fn new_client() -> HttpClient {
    HttpClient::builder().build()
}

pub fn new_https_client_dangerous(
    tls: rustls::ClientConfig,
    tcp_options: TcpOptions,
) -> HttpsClient {
    HttpsClient::builder().tls(tls).tcp_options(tcp_options).build()
}

/// Returns a new Fuchsia-compatible hyper client for making HTTP and HTTPS requests.
pub fn new_https_client_from_tcp_options(tcp_options: TcpOptions) -> HttpsClient {
    HttpsClient::builder().tcp_options(tcp_options).build()
}

/// Returns a new Fuchsia-compatible hyper client for making HTTP and HTTPS requests.
pub fn new_https_client() -> HttpsClient {
    HttpsClient::builder().build()
}

/// Returns a rustls::ClientConfig for further construction with improved session cache and without
/// a configured certificate store.
pub fn new_rustls_client_config() -> rustls::ClientConfig {
    let mut config = rustls::ClientConfig::new();
    // The default depth for the ClientSessionMemoryCache in the default ClientConfig is 32; this
    // value is assumed to be a sufficient default here as well.
    config.set_persistence(session_cache::C4CapableSessionCache::new(32));
    config
}

pub(crate) async fn parse_ip_addr<'a, F, Fut>(
    host: &'a str,
    port: u16,
    interface_name_to_index: F,
) -> Result<Option<SocketAddr>, io::Error>
where
    F: Fn(&'a str) -> Fut + 'a,
    Fut: Future<Output = Result<u32, io::Error>> + 'a,
{
    match host.parse::<Ipv4Addr>() {
        Ok(addr) => {
            return Ok(Some(SocketAddr::V4(SocketAddrV4::new(addr, port))));
        }
        Err(AddrParseError { .. }) => {}
    }

    // IPv6 literals are always enclosed in [].
    if !host.starts_with("[") || !host.ends_with(']') {
        return Ok(None);
    }

    let host = &host[1..host.len() - 1];

    // IPv6 addresses with zones always contain "%25", which is "%" URL encoded.
    let (host, zone_id) = if let Some((host, zone_id)) = host.split_once("%25") {
        (host, Some(zone_id))
    } else {
        (host, None)
    };

    let addr = match host.parse::<Ipv6Addr>() {
        Ok(addr) => addr,
        Err(AddrParseError { .. }) => {
            return Ok(None);
        }
    };

    let scope_id = if let Some(zone_id) = zone_id {
        // rfc6874 section 4 states:
        //
        //     The security considerations from the URI syntax specification
        //     [RFC3986] and the IPv6 Scoped Address Architecture specification
        //     [RFC4007] apply.  In particular, this URI format creates a specific
        //     pathway by which a deceitful zone index might be communicated, as
        //     mentioned in the final security consideration of the Scoped Address
        //     Architecture specification.  It is emphasised that the format is
        //     intended only for debugging purposes, but of course this intention
        //     does not prevent misuse.
        //
        //     To limit this risk, implementations MUST NOT allow use of this format
        //     except for well-defined usages, such as sending to link-local
        //     addresses under prefix fe80::/10.  At the time of writing, this is
        //     the only well-defined usage known.
        //
        // Since the only known use-case of IPv6 Zone Identifiers on Fuchsia is to communicate
        // with link-local devices, restrict addresses to link-local zone identifiers.
        //
        // TODO: use Ipv6Addr::is_unicast_link_local_strict when available in stable rust.
        if addr.segments()[..4] != [0xfe80, 0, 0, 0] {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "zone_id is only usable with link local addresses",
            ));
        }

        // TODO: validate that the value matches rfc6874 grammar `ZoneID = 1*( unreserved / pct-encoded )`.
        match zone_id.parse::<u32>() {
            Ok(scope_id) => scope_id,
            Err(ParseIntError { .. }) => interface_name_to_index(zone_id).await?,
        }
    } else {
        0
    };

    Ok(Some(SocketAddr::V6(SocketAddrV6::new(addr, port, 0, scope_id))))
}

#[cfg(target_os = "fuchsia")]
pub(crate) fn connect_and_bind_device<D: AsRef<[u8]>>(
    addr: SocketAddr,
    bind_device: Option<D>,
) -> io::Result<net::TcpConnector> {
    let socket = socket2::Socket::new(
        match addr {
            SocketAddr::V4(_) => socket2::Domain::IPV4,
            SocketAddr::V6(_) => socket2::Domain::IPV6,
        },
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    if let Some(bind_device) = bind_device {
        socket.bind_device(Some(bind_device.as_ref()))?;
    }
    net::TcpStream::connect_from_raw(socket, addr)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fuchsia_async::{self as fasync},
    };

    async fn unsupported(_name: &str) -> Result<u32, io::Error> {
        panic!("should not have happened")
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_parse_ipv4_addr() {
        let expected = "1.2.3.4:8080".parse::<SocketAddr>().unwrap();
        assert_matches!(
            parse_ip_addr("1.2.3.4", 8080, unsupported).await,
            Ok(Some(addr)) if addr == expected);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_parse_invalid_addresses() {
        assert_matches!(parse_ip_addr("1.2.3", 8080, unsupported).await, Ok(None));
        assert_matches!(parse_ip_addr("1.2.3.4.5", 8080, unsupported).await, Ok(None));
        assert_matches!(parse_ip_addr("localhost", 8080, unsupported).await, Ok(None));
        assert_matches!(parse_ip_addr("[fe80::1:2:3:4", 8080, unsupported).await, Ok(None));
        assert_matches!(parse_ip_addr("[[fe80::1:2:3:4]", 8080, unsupported).await, Ok(None));
        assert_matches!(parse_ip_addr("[]", 8080, unsupported).await, Ok(None));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_parse_ipv6_addr() {
        let expected = "[fe80::1:2:3:4]:8080".parse::<SocketAddr>().unwrap();
        assert_matches!(
            parse_ip_addr("[fe80::1:2:3:4]", 8080, unsupported).await,
            Ok(Some(addr)) if addr == expected
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_parse_ipv6_addr_with_zone_must_be_local() {
        assert_matches!(
            parse_ip_addr("[fe81::1:2:3:4%252]", 8080, unsupported).await,
            Err(err) if err.kind() == io::ErrorKind::Other
        );
    }
}
