// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use named_timer::{DeadlineId, NamedTimer};

pub const DEADLINE_ID: DeadlineId<'static> =
    DeadlineId::new("power-manager", "thermal-policy-timer");

pub fn get_periodic_timer_stream(
    duration: fuchsia_zircon::Duration,
) -> std::pin::Pin<Box<impl futures::Stream<Item = ()>>> {
    let next = fuchsia_zircon::Time::get_monotonic() + duration;
    let stream = futures::stream::unfold((next, duration), |(n, d)| async move {
        NamedTimer::new(&DEADLINE_ID, n - fuchsia_zircon::Time::get_monotonic()).await;
        Some(((), (n + d, d)))
    });
    Box::pin(stream)
}
