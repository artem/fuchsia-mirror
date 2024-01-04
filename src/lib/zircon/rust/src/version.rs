// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon version-string call.

use fuchsia_zircon_sys as sys;
use std::slice;
use std::str;

/// Return a version string describing the system (kernel).
/// This string never changes.
///
/// Wraps the
/// [zx_system_get_version_string](https://fuchsia.dev/fuchsia-src/reference/syscalls/system_get_version_string.md)
/// syscall.
pub fn system_get_version_string() -> &'static str {
    unsafe {
        let sv = sys::zx_system_get_version_string();
        let slice: &'static [u8] = slice::from_raw_parts(sv.c_str, sv.length);
        str::from_utf8_unchecked(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Status;

    #[test]
    fn get_version_string() -> Result<(), Status> {
        let sv = system_get_version_string();
        let s = sv.to_string();

        // Incremental builders set kernel version string to "nostamp".
        // See https://fxbug.dev/74797 for details.
        if s != "nostamp" {
            assert!(
                sv.len() > 20,
                "system version string [{}] length {} should be > 20",
                s,
                sv.len()
            );
            assert!(
                sv.len() < 100,
                "system version string [{}] length {} should be < 100",
                s,
                sv.len()
            );
            assert_eq!(s, sv);
        }

        Ok(())
    }
}
