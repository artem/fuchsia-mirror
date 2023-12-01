// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::uapi::dev_t;
use std::fmt;

pub const MEM_MAJOR: u32 = 1;
pub const TTY_ALT_MAJOR: u32 = 5;
pub const LOOP_MAJOR: u32 = 7;
pub const MISC_MAJOR: u32 = 10;
pub const INPUT_MAJOR: u32 = 13;
pub const FB_MAJOR: u32 = 29;
// TODO(tbodt): Use the rest of the range of majors marked as RESERVED FOR DYNAMIC ASSIGMENT in
// devices.txt.
pub const DYN_MAJOR: u32 = 234;

// Unclear if this device number is assigned dynamically, but this value is what abarth observed
// once for /dev/block/zram0.
pub const ZRAM_MAJOR: u32 = 252;

#[derive(Copy, Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeviceType(dev_t);

impl DeviceType {
    pub const NONE: DeviceType = DeviceType(0);

    // MEM
    pub const NULL: DeviceType = DeviceType::new(MEM_MAJOR, 3);
    pub const ZERO: DeviceType = DeviceType::new(MEM_MAJOR, 5);
    pub const FULL: DeviceType = DeviceType::new(MEM_MAJOR, 7);
    pub const RANDOM: DeviceType = DeviceType::new(MEM_MAJOR, 8);
    pub const URANDOM: DeviceType = DeviceType::new(MEM_MAJOR, 9);
    pub const KMSG: DeviceType = DeviceType::new(MEM_MAJOR, 11);

    // TTY_ALT
    pub const TTY: DeviceType = DeviceType::new(TTY_ALT_MAJOR, 0);
    pub const PTMX: DeviceType = DeviceType::new(TTY_ALT_MAJOR, 2);

    // MISC
    pub const HW_RANDOM: DeviceType = DeviceType::new(MISC_MAJOR, 183);
    pub const UINPUT: DeviceType = DeviceType::new(MISC_MAJOR, 223);
    pub const FUSE: DeviceType = DeviceType::new(MISC_MAJOR, 229);
    pub const DEVICE_MAPPER: DeviceType = DeviceType::new(MISC_MAJOR, 236);
    pub const LOOP_CONTROL: DeviceType = DeviceType::new(MISC_MAJOR, 237);

    // Frame buffer
    pub const FB0: DeviceType = DeviceType::new(FB_MAJOR, 0);

    pub const fn new(major: u32, minor: u32) -> DeviceType {
        // This encoding is part of the Linux UAPI. The encoded value is
        // returned to userspace in the stat struct.
        // See <https://man7.org/linux/man-pages/man3/makedev.3.html>.
        DeviceType(
            (((major & 0xfffff000) as u64) << 32)
                | (((major & 0xfff) as u64) << 8)
                | (((minor & 0xffffff00) as u64) << 12)
                | ((minor & 0xff) as u64),
        )
    }

    pub const fn from_bits(dev: dev_t) -> DeviceType {
        DeviceType(dev)
    }

    pub const fn bits(&self) -> dev_t {
        self.0
    }

    pub const fn major(&self) -> u32 {
        ((self.0 >> 32 & 0xfffff000) | ((self.0 >> 8) & 0xfff)) as u32
    }

    pub const fn minor(&self) -> u32 {
        ((self.0 >> 12 & 0xffffff00) | (self.0 & 0xff)) as u32
    }
}

impl fmt::Display for DeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "{}:{}", self.major(), self.minor())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn test_device_type() {
        let dev = DeviceType::new(21, 17);
        assert_eq!(dev.major(), 21);
        assert_eq!(dev.minor(), 17);

        let dev = DeviceType::new(0x83af83fe, 0xf98ecba1);
        assert_eq!(dev.major(), 0x83af83fe);
        assert_eq!(dev.minor(), 0xf98ecba1);
    }
}
