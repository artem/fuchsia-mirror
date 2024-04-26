// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_filter_ext as fnet_filter_ext;
use net_types::ip::{GenericOverIp, Ip, IpInvariant};
use packet_formats::ip::IpExt;

use super::{ConversionResult, IpVersionMismatchError, IpVersionStrictness, TryConvertToCoreState};

impl TryConvertToCoreState for fnet::IpAddress {
    type CoreState<I: IpExt> = I::Addr;

    fn try_convert<I: IpExt>(
        self,
        ip_version_strictness: IpVersionStrictness,
    ) -> Result<ConversionResult<Self::CoreState<I>>, IpVersionMismatchError> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrap<I: IpExt>(Result<ConversionResult<I::Addr>, IpVersionMismatchError>);
        let Wrap(result) = I::map_ip(
            IpInvariant(self),
            |IpInvariant(addr)| match addr {
                Self::Ipv4(addr) => Wrap(Ok(ConversionResult::State(addr.into_ext()))),
                Self::Ipv6(_) => Wrap(ip_version_strictness.mismatch_result()),
            },
            |IpInvariant(addr)| match addr {
                Self::Ipv4(_) => Wrap(ip_version_strictness.mismatch_result()),
                Self::Ipv6(addr) => Wrap(Ok(ConversionResult::State(addr.into_ext()))),
            },
        );
        result
    }
}

impl TryConvertToCoreState for fnet_filter_ext::TransparentProxy {
    type CoreState<I: IpExt> = netstack3_core::filter::TransparentProxy<I>;

    fn try_convert<I: IpExt>(
        self,
        ip_version_strictness: IpVersionStrictness,
    ) -> Result<ConversionResult<Self::CoreState<I>>, IpVersionMismatchError> {
        match self {
            Self::LocalPort(port) => Ok(ConversionResult::State(
                netstack3_core::filter::TransparentProxy::LocalPort(port),
            )),
            Self::LocalAddr(addr) => {
                let addr = match addr.try_convert::<I>(ip_version_strictness) {
                    Ok(ConversionResult::State(addr)) => addr,
                    Ok(ConversionResult::Omit) => return Ok(ConversionResult::Omit),
                    Err(IpVersionMismatchError) => return Err(IpVersionMismatchError),
                };
                Ok(ConversionResult::State(netstack3_core::filter::TransparentProxy::LocalAddr(
                    addr,
                )))
            }
            Self::LocalAddrAndPort(addr, port) => {
                let addr = match addr.try_convert::<I>(ip_version_strictness) {
                    Ok(ConversionResult::State(addr)) => addr,
                    Ok(ConversionResult::Omit) => return Ok(ConversionResult::Omit),
                    Err(IpVersionMismatchError) => return Err(IpVersionMismatchError),
                };
                Ok(ConversionResult::State(
                    netstack3_core::filter::TransparentProxy::LocalAddrAndPort(addr, port),
                ))
            }
        }
    }
}
