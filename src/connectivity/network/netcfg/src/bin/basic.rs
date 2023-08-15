// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[fuchsia_async::run_singlethreaded]
pub async fn main() {
    netcfg::run::<netcfg::BasicMode>().await.expect("netcfg exited with an error")
}
