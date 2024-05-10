// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! High-level benchmarks.
//!
//! This module contains microbenchmarks for the Netstack3 Core, built on top
//! of Criterion.

// Enable dead code warnings for benchmarks (disabled in `lib.rs`).
#![warn(dead_code, unused_imports, unused_macros)]

use alloc::vec;
#[cfg(debug_assertions)]
use assert_matches::assert_matches;

use net_types::{ip::Ipv4, Witness as _};
use netstack3_base::{bench, testutil::Bencher};
use packet::{Buf, InnerPacketBuilder, Serializer};
use packet_formats::{
    ethernet::{
        testutil::{
            ETHERNET_DST_MAC_BYTE_OFFSET, ETHERNET_HDR_LEN_NO_TAG, ETHERNET_MIN_BODY_LEN_NO_TAG,
            ETHERNET_SRC_MAC_BYTE_OFFSET,
        },
        EtherType, EthernetFrameBuilder,
    },
    ip::IpProto,
    ipv4::{
        testutil::{IPV4_CHECKSUM_OFFSET, IPV4_MIN_HDR_LEN, IPV4_TTL_OFFSET},
        Ipv4PacketBuilder,
    },
};

use crate::{
    device::{
        ethernet::{EthernetLinkDevice, RecvEthernetFrameMeta},
        DeviceId,
    },
    state::StackStateBuilder,
    testutil::{CtxPairExt as _, FakeCtxBuilder, TEST_ADDRS_V4},
};

// NOTE: Extra tests that are too expensive to run during benchmarks can be
// added by gating them on the `debug_assertions` configuration option. This
// option is disabled when running `cargo check`, but enabled when running
// `cargo test`.

// Benchmark the minimum possible time to forward an IPv4 packet by stripping
// out all interesting computation. We have the simplest possible setup - a
// forwarding table with a single entry, and a single device - and we receive an
// IPv4 packet frame which we expect will be parsed and forwarded without
// requiring any new buffers to be allocated.
fn bench_forward_minimum<B: Bencher>(b: &mut B, frame_size: usize) {
    let (mut ctx, idx_to_device_id) =
        FakeCtxBuilder::with_addrs(TEST_ADDRS_V4).build_with(StackStateBuilder::default());

    let eth_device = idx_to_device_id[0].clone();
    let device: DeviceId<_> = eth_device.clone().into();
    crate::device::testutil::set_forwarding_enabled::<_, Ipv4>(&mut ctx, &device, true);

    assert!(
        frame_size
            >= ETHERNET_HDR_LEN_NO_TAG
                + core::cmp::max(ETHERNET_MIN_BODY_LEN_NO_TAG, IPV4_MIN_HDR_LEN)
    );
    let body = vec![0; frame_size - (ETHERNET_HDR_LEN_NO_TAG + IPV4_MIN_HDR_LEN)];
    const TTL: u8 = 64;
    let mut buf = body
        .into_serializer()
        .encapsulate(Ipv4PacketBuilder::new(
            // Use the remote IP as the destination so that we decide to
            // forward.
            TEST_ADDRS_V4.remote_ip,
            TEST_ADDRS_V4.remote_ip,
            TTL,
            IpProto::Udp.into(),
        ))
        .encapsulate(EthernetFrameBuilder::new(
            TEST_ADDRS_V4.remote_mac.get(),
            TEST_ADDRS_V4.local_mac.get(),
            EtherType::Ipv4,
            ETHERNET_HDR_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .unwrap();

    let buf = buf.as_mut();
    let range = 0..buf.len();

    // Store a copy of the checksum to re-write it later.
    let ipv4_checksum = [
        buf[ETHERNET_HDR_LEN_NO_TAG + IPV4_CHECKSUM_OFFSET],
        buf[ETHERNET_HDR_LEN_NO_TAG + IPV4_CHECKSUM_OFFSET + 1],
    ];

    b.iter(|| {
        B::black_box(ctx.core_api().device::<EthernetLinkDevice>().receive_frame(
            B::black_box(RecvEthernetFrameMeta { device_id: eth_device.clone() }),
            B::black_box(Buf::new(&mut buf[..], range.clone())),
        ));

        #[cfg(debug_assertions)]
        {
            assert_matches!(&ctx.bindings_ctx.take_ethernet_frames()[..], [_frame]);
        }

        // Since we modified the buffer in-place, it now has the wrong source
        // and destination MAC addresses and IP TTL/CHECKSUM. We reset them to
        // their original values as efficiently as we can to avoid affecting the
        // results of the benchmark.
        (&mut buf[ETHERNET_SRC_MAC_BYTE_OFFSET..ETHERNET_SRC_MAC_BYTE_OFFSET + 6])
            .copy_from_slice(&TEST_ADDRS_V4.remote_mac.bytes()[..]);
        (&mut buf[ETHERNET_DST_MAC_BYTE_OFFSET..ETHERNET_DST_MAC_BYTE_OFFSET + 6])
            .copy_from_slice(&TEST_ADDRS_V4.local_mac.bytes()[..]);
        let ipv4_buf = &mut buf[ETHERNET_HDR_LEN_NO_TAG..];
        ipv4_buf[IPV4_TTL_OFFSET] = TTL;
        ipv4_buf[IPV4_CHECKSUM_OFFSET..IPV4_CHECKSUM_OFFSET + 2]
            .copy_from_slice(&ipv4_checksum[..]);
    });
}

bench!(bench_forward_minimum_64, |b| bench_forward_minimum(b, 64));
bench!(bench_forward_minimum_128, |b| bench_forward_minimum(b, 128));
bench!(bench_forward_minimum_256, |b| bench_forward_minimum(b, 256));
bench!(bench_forward_minimum_512, |b| bench_forward_minimum(b, 512));
bench!(bench_forward_minimum_1024, |b| bench_forward_minimum(b, 1024));

#[cfg(benchmark)]
/// Returns a benchmark group for all Netstack3 Core microbenchmarks.
pub fn get_benchmark() -> criterion::Benchmark {
    // TODO(https://fxbug.dev/42051624) Find an automatic way to add benchmark
    // functions to the `Criterion::Benchmark`, ideally as part of `bench!`.
    let mut b = criterion::Benchmark::new("ForwardIpv4/64", bench_forward_minimum_64);
    b = b.with_function("ForwardIpv4/128", bench_forward_minimum_128);
    b = b.with_function("ForwardIpv4/256", bench_forward_minimum_256);
    b = b.with_function("ForwardIpv4/512", bench_forward_minimum_512);
    b = b.with_function("ForwardIpv4/1024", bench_forward_minimum_1024);

    // Add additional microbenchmarks defined elsewhere.
    crate::data_structures::token_bucket::tests::add_benches(b)
}
