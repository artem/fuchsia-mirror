// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub type FsString = Vec<u8>;
pub type FsStr = [u8];

// Helper that can be used to build paths backwards, from the tail to head.
pub struct PathBuilder {
    // The path is kept in `data[pos..]`.
    data: FsString,
    pos: usize,
}

impl PathBuilder {
    const INITIAL_CAPACITY: usize = 32;

    pub fn new() -> Self {
        Self { data: vec![], pos: 0 }
    }

    pub fn prepend_element(&mut self, element: &FsStr) {
        self.ensure_capacity(element.len() + 1);
        let old_pos = self.pos;
        self.pos -= element.len() + 1;
        self.data[self.pos + 1..old_pos].copy_from_slice(element);
        self.data[self.pos] = b'/';
    }

    /// Build the absolute path string.
    pub fn build_absolute(mut self) -> FsString {
        if self.pos == self.data.len() {
            return b"/".to_vec();
        }
        self.data.drain(..self.pos);
        self.data
    }

    /// Build the relative path string.
    pub fn build_relative(self) -> FsString {
        let mut absolute = self.build_absolute();
        // Remove the prefix slash.
        absolute.remove(0);
        absolute
    }

    fn ensure_capacity(&mut self, capacity_needed: usize) {
        if capacity_needed > self.pos {
            let current_size = self.data.len();
            let len = current_size - self.pos;
            let min_size = len + capacity_needed;
            let mut new_size = std::cmp::max(current_size * 2, Self::INITIAL_CAPACITY);
            while new_size < min_size {
                new_size *= 2;
            }
            self.data.reserve(new_size - current_size);
            self.data.resize(new_size - len, 0);
            self.data.extend_from_within(self.pos..(self.pos + len));
            self.pos = new_size - len;
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[::fuchsia::test]
    fn test_path_builder() {
        let p = PathBuilder::new();
        assert_eq!(p.build_absolute(), b"/");

        let p = PathBuilder::new();
        assert_eq!(p.build_relative(), b"");

        let mut p = PathBuilder::new();
        p.prepend_element(b"foo");
        assert_eq!(p.build_absolute(), b"/foo");

        let mut p = PathBuilder::new();
        p.prepend_element(b"foo");
        assert_eq!(p.build_relative(), b"foo");

        let mut p = PathBuilder::new();
        p.prepend_element(b"foo");
        p.prepend_element(b"bar");
        assert_eq!(p.build_absolute(), b"/bar/foo");

        let mut p = PathBuilder::new();
        p.prepend_element(b"foo");
        p.prepend_element(b"1234567890123456789012345678901234567890");
        p.prepend_element(b"bar");
        assert_eq!(p.build_absolute(), b"/bar/1234567890123456789012345678901234567890/foo");
    }
}
