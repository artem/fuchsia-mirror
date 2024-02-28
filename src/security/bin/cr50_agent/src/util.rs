// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use std::mem::size_of;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum DeserializeError {
    #[error("Not enough bytes left")]
    NotEnoughBytes,
    #[error("Invalid value")]
    InvalidValue,
}

/// A helper to make deserialising TPM responses a bit nicer.
pub struct Deserializer {
    data: Vec<u8>,
    pos: usize,
}

impl Deserializer {
    pub fn new(data: Vec<u8>) -> Deserializer {
        Deserializer { data, pos: 0 }
    }

    pub fn take(&mut self, bytes: usize) -> Result<&[u8], DeserializeError> {
        let max = match bytes.checked_add(self.pos) {
            Some(max) => {
                if max > self.data.len() {
                    return Err(DeserializeError::NotEnoughBytes);
                } else {
                    max
                }
            }
            None => return Err(DeserializeError::NotEnoughBytes),
        };

        let start = self.pos;
        self.pos = max;
        Ok(&self.data[start..max])
    }

    pub fn take_be_u32(&mut self) -> Result<u32, DeserializeError> {
        Ok(u32::from_be_bytes(self.take(size_of::<u32>())?.try_into().unwrap()))
    }

    pub fn take_be_u16(&mut self) -> Result<u16, DeserializeError> {
        Ok(u16::from_be_bytes(self.take(size_of::<u16>())?.try_into().unwrap()))
    }

    pub fn take_u8(&mut self) -> Result<u8, DeserializeError> {
        Ok(self.take(1)?[0])
    }

    pub fn take_le_u64(&mut self) -> Result<u64, DeserializeError> {
        Ok(u64::from_le_bytes(self.take(size_of::<u64>())?.try_into().unwrap()))
    }

    pub fn take_le_u32(&mut self) -> Result<u32, DeserializeError> {
        Ok(u32::from_le_bytes(self.take(size_of::<u32>())?.try_into().unwrap()))
    }

    pub fn take_le_u16(&mut self) -> Result<u16, DeserializeError> {
        Ok(u16::from_le_bytes(self.take(size_of::<u16>())?.try_into().unwrap()))
    }

    pub fn available(&self) -> usize {
        self.data.len() - self.pos
    }
}

/// A helper to make serialising TPM commands a bit nicer.
pub struct Serializer {
    data: Vec<u8>,
}

impl Serializer {
    pub fn new() -> Self {
        Serializer { data: Vec::new() }
    }

    pub fn put(&mut self, data: &[u8]) {
        self.data.extend(data.into_iter())
    }

    #[allow(dead_code)]
    pub fn put_be_u32(&mut self, data: u32) {
        self.put(&data.to_be_bytes())
    }

    pub fn put_be_u16(&mut self, data: u16) {
        self.put(&data.to_be_bytes())
    }

    pub fn put_u8(&mut self, data: u8) {
        self.put(&[data])
    }

    pub fn put_le_u16(&mut self, data: u16) {
        self.put(&data.to_le_bytes())
    }

    pub fn put_le_u32(&mut self, data: u32) {
        self.put(&data.to_le_bytes())
    }

    pub fn put_le_u64(&mut self, data: u64) {
        self.put(&data.to_le_bytes())
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn serializer_put_data() {
        let mut s = Serializer::new();
        s.put_be_u32(0xd00dfeed);
        s.put_be_u16(0xface);
        s.put_u8(0xaa);
        s.put_le_u16(0xface);
        let vec = s.into_vec();

        assert_eq!(vec, vec![0xd0, 0x0d, 0xfe, 0xed, 0xfa, 0xce, 0xaa, 0xce, 0xfa]);
    }

    #[fuchsia::test]
    fn deserializer_take_data() {
        let data = vec![0xd0, 0x0d, 0xfe, 0xed, 0xfa, 0xce, 0xaa];
        let mut d = Deserializer::new(data);
        assert_eq!(d.available(), 7);
        assert_eq!(d.take_be_u32().unwrap(), 0xd00dfeed);
        assert_eq!(d.available(), 3);
        assert_eq!(d.take_be_u16().unwrap(), 0xface);
        assert_eq!(d.available(), 1);
        assert_eq!(d.take_u8().unwrap(), 0xaa);
        assert_eq!(d.available(), 0);
        assert_eq!(d.take_u8().unwrap_err(), DeserializeError::NotEnoughBytes);
    }
    #[fuchsia::test]
    fn deserializer_take_data_le() {
        let data = vec![0xd0, 0x0d, 0xfe, 0xed, 0xfa, 0xce, 0xaa];
        let mut d = Deserializer::new(data);
        assert_eq!(d.available(), 7);
        assert_eq!(d.take_le_u32().unwrap(), 0xedfe0dd0);
        assert_eq!(d.available(), 3);
        assert_eq!(d.take_le_u16().unwrap(), 0xcefa);
        assert_eq!(d.available(), 1);
        assert_eq!(d.take_u8().unwrap(), 0xaa);
        assert_eq!(d.available(), 0);
        assert_eq!(d.take_u8().unwrap_err(), DeserializeError::NotEnoughBytes);
    }
}
