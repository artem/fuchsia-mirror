// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::x86_64::_rdtsc;
use fuchsia_zircon as zx;

use crate::types::Errno;

pub const HAS_VDSO: bool = true;

pub fn calculate_ticks_offset() -> i64 {
    let mut ticks_offset: i64 = i64::MIN;
    let mut min_read_diff: i64 = i64::MAX;
    // Assuming zx_get_ticks() is based on the TSC, estimate the offset between the raw value
    // from `rdtsc` and the ticks as returned by zx_get_ticks(). Since the reads will not be
    // made at the same time, the result will not be presice, so do the estimation several
    // times and choose the measurement with the smallest error bars.
    // TODO(fxb/127692): Obtain this value from Zircon
    for _i in 0..5 {
        let raw_ticks;
        let zx_read_first = zx::ticks_get();
        unsafe {
            raw_ticks = _rdtsc();
        }
        let zx_read_second = zx::ticks_get();
        let read_diff = zx_read_second - zx_read_first;
        if read_diff < min_read_diff {
            min_read_diff = read_diff;
            let midpoint = zx_read_first + read_diff / 2;
            ticks_offset = midpoint - raw_ticks as i64;
        }
    }
    ticks_offset
}

pub fn get_sigreturn_offset(_vdso_vmo: &zx::Vmo) -> Result<Option<u64>, Errno> {
    Ok(None)
}
