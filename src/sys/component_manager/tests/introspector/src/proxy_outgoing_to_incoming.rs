// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use fuchsia_component::server::ServiceFs;
use futures_util::StreamExt;

#[fuchsia::main]
async fn main() {
    let mut fs = ServiceFs::new();
    let svc = fuchsia_fs::directory::open_in_namespace("/svc", fio::OpenFlags::empty()).unwrap();
    fs.add_remote("svc", svc).take_and_serve_directory_handle().unwrap().collect().await
}
