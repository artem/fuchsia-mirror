// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;

/// Set of all rights that are valid to use with the conformance test harness.
const ALL_RIGHTS_FLAGS: fio::OpenFlags = fio::OpenFlags::empty()
    .union(fio::OpenFlags::RIGHT_READABLE)
    .union(fio::OpenFlags::RIGHT_WRITABLE)
    .union(fio::OpenFlags::RIGHT_EXECUTABLE);

/// Helper struct that encapsulates generation of valid/invalid sets of flags based on
/// which rights are supported by a particular node type.
pub struct Rights {
    rights: fio::OpenFlags,
}

impl Rights {
    /// Creates a new Rights struct based on a bitset of OPEN_RIGHT_* flags OR'd together.
    pub fn new(rights: fio::OpenFlags) -> Rights {
        assert_eq!(rights & !ALL_RIGHTS_FLAGS, fio::OpenFlags::empty());
        Rights { rights }
    }

    /// Returns the bitset of all supported OPEN_RIGHT_* flags.
    pub fn all(&self) -> fio::OpenFlags {
        self.rights
    }

    /// Returns a vector of all valid flag combinations.
    pub fn valid_combos(&self) -> Vec<fio::OpenFlags> {
        vfs::test_utils::build_flag_combinations(fio::OpenFlags::empty(), self.rights)
    }

    /// Returns a vector of all valid flag combinations that include the specified `with_flags`.
    /// Will be empty if none of the requested rights are supported.
    pub fn valid_combos_with(&self, with_flags: fio::OpenFlags) -> Vec<fio::OpenFlags> {
        let mut flag_combos = self.valid_combos();
        flag_combos.retain(|&flags| flags.contains(with_flags));
        flag_combos
    }

    /// Returns a vector of all valid flag combinations that exclude the specified `without_flags`.
    /// Will be empty if none are supported.
    pub fn valid_combos_without(&self, without_flags: fio::OpenFlags) -> Vec<fio::OpenFlags> {
        let mut flag_combos = self.valid_combos();
        flag_combos.retain(|&flags| !flags.intersects(without_flags));
        flag_combos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rights_combos() {
        const TEST_RIGHTS: fio::OpenFlags = fio::OpenFlags::empty()
            .union(fio::OpenFlags::RIGHT_READABLE)
            .union(fio::OpenFlags::RIGHT_WRITABLE)
            .union(fio::OpenFlags::RIGHT_EXECUTABLE);
        // We should get 0, R, W, X, RW, RX, WX, RWX (8 in total).
        const EXPECTED_COMBOS: [fio::OpenFlags; 8] = [
            fio::OpenFlags::empty(),
            fio::OpenFlags::RIGHT_READABLE,
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::RIGHT_EXECUTABLE,
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_READABLE)
                .union(fio::OpenFlags::RIGHT_WRITABLE),
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_READABLE)
                .union(fio::OpenFlags::RIGHT_EXECUTABLE),
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_WRITABLE)
                .union(fio::OpenFlags::RIGHT_EXECUTABLE),
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_READABLE)
                .union(fio::OpenFlags::RIGHT_WRITABLE)
                .union(fio::OpenFlags::RIGHT_EXECUTABLE),
        ];
        let rights = Rights::new(TEST_RIGHTS);
        assert_eq!(rights.all(), TEST_RIGHTS);

        // Test that all possible combinations are generated correctly.
        let all_combos = rights.valid_combos();
        assert_eq!(all_combos.len(), EXPECTED_COMBOS.len());
        for expected_rights in EXPECTED_COMBOS {
            assert!(all_combos.contains(&expected_rights));
        }

        // Test that combinations including READABLE are generated correctly.
        // We should get R, RW, RX, and RWX (4 in total).
        const EXPECTED_READABLE_COMBOS: [fio::OpenFlags; 4] = [
            fio::OpenFlags::RIGHT_READABLE,
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_READABLE)
                .union(fio::OpenFlags::RIGHT_WRITABLE),
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_READABLE)
                .union(fio::OpenFlags::RIGHT_EXECUTABLE),
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_READABLE)
                .union(fio::OpenFlags::RIGHT_WRITABLE)
                .union(fio::OpenFlags::RIGHT_EXECUTABLE),
        ];
        let readable_combos = rights.valid_combos_with(fio::OpenFlags::RIGHT_READABLE);
        assert_eq!(readable_combos.len(), EXPECTED_READABLE_COMBOS.len());
        for expected_rights in EXPECTED_READABLE_COMBOS {
            assert!(readable_combos.contains(&expected_rights));
        }

        // Test that combinations excluding READABLE are generated correctly.
        // We should get 0, W, X, and WX (4 in total).
        const EXPECTED_NONREADABLE_COMBOS: [fio::OpenFlags; 4] = [
            fio::OpenFlags::empty(),
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::RIGHT_EXECUTABLE,
            fio::OpenFlags::empty()
                .union(fio::OpenFlags::RIGHT_WRITABLE)
                .union(fio::OpenFlags::RIGHT_EXECUTABLE),
        ];
        let nonreadable_combos = rights.valid_combos_without(fio::OpenFlags::RIGHT_READABLE);
        assert_eq!(nonreadable_combos.len(), EXPECTED_NONREADABLE_COMBOS.len());
        for expected_rights in EXPECTED_NONREADABLE_COMBOS {
            assert!(nonreadable_combos.contains(&expected_rights));
        }
    }

    #[test]
    #[should_panic]
    fn test_rights_unsupported() {
        // Passing anything other than OPEN_RIGHT_* should fail.
        Rights::new(fio::OpenFlags::CREATE);
    }
}
