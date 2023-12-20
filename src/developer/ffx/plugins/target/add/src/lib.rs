// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::ffx_error;
use ffx_target::add_manual_target;
use ffx_target_add_args::AddCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use netext::parse_address_parts;

#[derive(FfxTool)]
pub struct AddTool {
    #[command]
    cmd: AddCommand,
    #[with(daemon_protocol())]
    target_collection_proxy: TargetCollectionProxy,
}

fho::embedded_plugin!(AddTool);

#[async_trait(?Send)]
impl FfxMain for AddTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        add_impl(self.target_collection_proxy, self.cmd).await
    }
}

pub async fn add_impl(
    target_collection_proxy: TargetCollectionProxy,
    cmd: AddCommand,
) -> fho::Result<()> {
    let (addr, scope, port) =
        parse_address_parts(cmd.addr.as_str()).map_err(|e| ffx_error!("{}", e))?;
    let scope_id = if let Some(scope) = scope {
        match netext::get_verified_scope_id(scope) {
            Ok(res) => res,
            Err(_e) => {
                return Err(ffx_error!(
                    "Cannot add target, as scope ID '{scope}' is not a valid interface name or index"
                )
                .into());
            }
        }
    } else {
        0
    };
    add_manual_target(&target_collection_proxy, addr, scope_id, port.unwrap_or(0), !cmd.nowait)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx as ffx;
    use fidl_fuchsia_net as net;

    fn setup_fake_target_collection<T: 'static + Fn(ffx::TargetAddrInfo) + Send>(
        test: T,
    ) -> TargetCollectionProxy {
        fho::testing::fake_proxy(move |req| match req {
            ffx::TargetCollectionRequest::AddTarget {
                ip, config: _, add_target_responder, ..
            } => {
                let add_target_responder = add_target_responder.into_proxy().unwrap();
                test(ip);
                add_target_responder.success().unwrap();
            }
            _ => assert!(false),
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_add() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::Ip(ffx::TargetIp {
                    ip: net::IpAddress::Ipv4(net::Ipv4Address {
                        addr: "123.210.123.210"
                            .parse::<std::net::Ipv4Addr>()
                            .unwrap()
                            .octets()
                            .into()
                    }),
                    scope_id: 0,
                })
            )
        });
        add_impl(server, AddCommand { addr: "123.210.123.210".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_add_port() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv4(net::Ipv4Address {
                        addr: "123.210.123.210"
                            .parse::<std::net::Ipv4Addr>()
                            .unwrap()
                            .octets()
                            .into()
                    }),
                    scope_id: 0,
                    port: 2310,
                })
            )
        });
        add_impl(server, AddCommand { addr: "123.210.123.210:2310".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_add_v6() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::Ip(ffx::TargetIp {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 0,
                })
            )
        });
        add_impl(server, AddCommand { addr: "f000::1".to_owned(), nowait: true }).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_add_v6_port() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 0,
                    port: 65,
                })
            )
        });
        add_impl(server, AddCommand { addr: "[f000::1]:65".to_owned(), nowait: true })
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_add_v6_scope_id() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::Ip(ffx::TargetIp {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 1,
                })
            )
        });
        add_impl(server, AddCommand { addr: "f000::1%1".to_owned(), nowait: true }).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_add_v6_scope_id_port() {
        let server = setup_fake_target_collection(|addr| {
            assert_eq!(
                addr,
                ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
                    ip: net::IpAddress::Ipv6(net::Ipv6Address {
                        addr: "f000::1".parse::<std::net::Ipv6Addr>().unwrap().octets().into()
                    }),
                    scope_id: 1,
                    port: 640,
                })
            )
        });
        add_impl(server, AddCommand { addr: "[f000::1%1]:640".to_owned(), nowait: true })
            .await
            .unwrap();
    }
}
