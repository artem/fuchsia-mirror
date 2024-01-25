// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::FuchsiaTime,
    async_trait::async_trait,
    fuchsia_async::net::TcpStream,
    futures::io::{AsyncRead, AsyncWrite},
    futures::task::{Context, Poll},
    std::net::SocketAddr,
    std::{io, pin::Pin},
    trust_dns_proto::tcp::Connect,
};

/// A Fuchsia-compatible implementation of trust-dns's `Connect` trait which allows
/// creating a `TcpStream` to a particular destination.
pub struct DnsTcpStream(TcpStream);

impl trust_dns_proto::tcp::DnsTcpStream for DnsTcpStream {
    type Time = FuchsiaTime;
}

#[async_trait]
impl Connect for DnsTcpStream {
    async fn connect(addr: SocketAddr) -> io::Result<Self> {
        TcpStream::connect(addr)?.await.map(Self)
    }

    async fn connect_with_bind(
        addr: SocketAddr,
        bind_addr: Option<SocketAddr>,
    ) -> io::Result<Self> {
        match bind_addr {
            Some(bind_addr) => {
                unimplemented!(
                    "https://fxbug.dev/42180092: cannot bind to {:?}; `connect_with_bind` is \
                    unimplemented",
                    bind_addr,
                )
            }
            None => Self::connect(addr).await,
        }
    }
}

impl AsyncRead for DnsTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for DnsTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::{FuchsiaExec, FuchsiaTime};
    use net_declare::std::ip;

    #[test]
    fn test_tcp_stream_ipv4() {
        use trust_dns_proto::tests::tcp_stream_test;
        let exec = FuchsiaExec::new().expect("failed to create fuchsia executor");
        tcp_stream_test::<DnsTcpStream, FuchsiaExec, FuchsiaTime>(ip!("127.0.0.1"), exec)
    }

    #[test]
    fn test_tcp_stream_ipv6() {
        use trust_dns_proto::tests::tcp_stream_test;
        let exec = FuchsiaExec::new().expect("failed to create fuchsia executor");
        tcp_stream_test::<DnsTcpStream, FuchsiaExec, FuchsiaTime>(ip!("::1"), exec)
    }

    #[test]
    fn test_tcp_client_stream_ipv4() {
        use trust_dns_proto::tests::tcp_client_stream_test;
        let exec = FuchsiaExec::new().expect("failed to create fuchsia executor");
        tcp_client_stream_test::<DnsTcpStream, FuchsiaExec, FuchsiaTime>(ip!("127.0.0.1"), exec)
    }

    #[test]
    fn test_tcp_client_stream_ipv6() {
        use trust_dns_proto::tests::tcp_client_stream_test;
        let exec = FuchsiaExec::new().expect("failed to create fuchsia executor");
        tcp_client_stream_test::<DnsTcpStream, FuchsiaExec, FuchsiaTime>(ip!("::1"), exec)
    }
}
