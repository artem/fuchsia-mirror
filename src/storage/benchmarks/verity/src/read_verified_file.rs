// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use storage_verity_benchmarks_lib::{read_verity_benchmark, run_benchmark, READ_BENCHMARK_NAME};

fn main() {
    run_benchmark(read_verity_benchmark, READ_BENCHMARK_NAME);
}
