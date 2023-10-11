// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    aes::{
        cipher::{
            generic_array::GenericArray, inout::InOut, typenum::consts::U16, BlockBackend,
            BlockClosure, BlockDecrypt, BlockEncrypt, BlockSizeUser, KeyInit, KeyIvInit,
            StreamCipher as _, StreamCipherSeek,
        },
        Aes256,
    },
    anyhow::{anyhow, Error},
    async_trait::async_trait,
    chacha20::{ChaCha20, Key},
    fprint::TypeFingerprint,
    serde::{
        de::{Error as SerdeError, Visitor},
        Deserialize, Deserializer, Serialize, Serializer,
    },
    static_assertions::assert_cfg,
    std::convert::TryInto,
    zerocopy::{AsBytes, FromBytes, FromZeroes},
};

pub mod ff1;

// Copied from //src/storage/fxfs/src/trace.rs.
// TODO(fxbug.dev/117467): Consider refactoring to a common shared crate.
#[macro_export]
macro_rules! trace_duration {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(feature = "tracing")]
        ::fuchsia_trace::duration!("fxfs", $name $(,$key => $val)*);
    }
}

pub const KEY_SIZE: usize = 256 / 8;
pub const WRAPPED_KEY_SIZE: usize = KEY_SIZE + 16;

// Fxfs will always use a block size >= 512 bytes, so we just assume a sector size of 512 bytes,
// which will work fine even if a different block size is used by Fxfs or the underlying device.
const SECTOR_SIZE: u64 = 512;

pub type KeyBytes = [u8; KEY_SIZE];

pub struct UnwrappedKey {
    key: KeyBytes,
}

impl UnwrappedKey {
    pub fn new(key: KeyBytes) -> Self {
        UnwrappedKey { key }
    }

    pub fn key(&self) -> &KeyBytes {
        &self.key
    }
}

pub type UnwrappedKeys = Vec<(u64, UnwrappedKey)>;

#[repr(transparent)]
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedKeyBytes(pub [u8; WRAPPED_KEY_SIZE]);

impl Default for WrappedKeyBytes {
    fn default() -> Self {
        Self([0u8; WRAPPED_KEY_SIZE])
    }
}

impl TypeFingerprint for WrappedKeyBytes {
    fn fingerprint() -> String {
        "WrappedKeyBytes".to_owned()
    }
}

impl std::ops::Deref for WrappedKeyBytes {
    type Target = [u8; WRAPPED_KEY_SIZE];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for WrappedKeyBytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// Because default impls of Serialize/Deserialize for [T; N] are only defined for N in 0..=32, we
// have to define them ourselves.
impl Serialize for WrappedKeyBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self[..])
    }
}

impl<'de> Deserialize<'de> for WrappedKeyBytes {
    fn deserialize<D>(deserializer: D) -> Result<WrappedKeyBytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct WrappedKeyVisitor;

        impl<'d> Visitor<'d> for WrappedKeyVisitor {
            type Value = WrappedKeyBytes;

            fn expecting(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_str("Expected wrapped keys to be 48 bytes")
            }

            fn visit_bytes<E>(self, bytes: &[u8]) -> Result<WrappedKeyBytes, E>
            where
                E: SerdeError,
            {
                self.visit_byte_buf(bytes.to_vec())
            }

            fn visit_byte_buf<E>(self, bytes: Vec<u8>) -> Result<WrappedKeyBytes, E>
            where
                E: SerdeError,
            {
                let orig_len = bytes.len();
                let bytes: [u8; WRAPPED_KEY_SIZE] =
                    bytes.try_into().map_err(|_| SerdeError::invalid_length(orig_len, &self))?;
                Ok(WrappedKeyBytes(bytes))
            }
        }
        deserializer.deserialize_byte_buf(WrappedKeyVisitor)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TypeFingerprint, PartialEq)]
pub struct WrappedKey {
    /// The identifier of the wrapping key.  The identifier has meaning to whatever is doing the
    /// unwrapping.
    pub wrapping_key_id: u64,
    /// AES 256 requires a 512 bit key, which is made of two 256 bit keys, one for the data and one
    /// for the tweak.  It is safe to use the same 256 bit key for both (see
    /// https://csrc.nist.gov/CSRC/media/Projects/Block-Cipher-Techniques/documents/BCM/Comments/XTS/follow-up_XTS_comments-Ball.pdf)
    /// which is what we do here.  Since the key is wrapped with AES-GCM-SIV, there are an
    /// additional 16 bytes paid per key (so the actual key material is 32 bytes once unwrapped).
    pub key: WrappedKeyBytes,
}

/// To support key rolling and clones, a file can have more than one key.  Each key has an ID that
/// unique to the file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
pub struct WrappedKeys(pub Vec<(u64, WrappedKey)>);

impl std::ops::Deref for WrappedKeys {
    type Target = [(u64, WrappedKey)];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

struct XtsCipher {
    id: u64,
    cipher: Aes256,
}

pub struct XtsCipherSet(Vec<XtsCipher>);

impl XtsCipherSet {
    pub fn new(keys: &UnwrappedKeys) -> Self {
        Self(
            keys.iter()
                .map(|(id, k)| XtsCipher {
                    id: *id,
                    cipher: Aes256::new(GenericArray::from_slice(k.key())),
                })
                .collect(),
        )
    }

    /// Decrypt the data in `buffer`.
    ///
    /// * `offset` is the byte offset within the file.
    /// * `key_id` specifies which of the unwrapped keys to use.
    /// * `buffer` is mutated in place.
    ///
    /// `buffer` *must* be 16 byte aligned.
    pub fn decrypt(&self, offset: u64, key_id: u64, buffer: &mut [u8]) -> Result<(), Error> {
        trace_duration!("decrypt");
        assert_eq!(offset % SECTOR_SIZE, 0);
        let cipher = &self
            .0
            .iter()
            .find(|cipher| cipher.id == key_id)
            .ok_or(anyhow!("Key not found"))?
            .cipher;
        let mut sector_offset = offset / SECTOR_SIZE;
        for sector in buffer.chunks_exact_mut(SECTOR_SIZE as usize) {
            let mut tweak = Tweak(sector_offset as u128);
            // The same key is used for encrypting the data and computing the tweak.
            cipher.encrypt_block(GenericArray::from_mut_slice(tweak.as_bytes_mut()));
            cipher.decrypt_with_backend(XtsProcessor::new(tweak, sector));
            sector_offset += 1;
        }
        Ok(())
    }

    /// Encrypts data in the `buffer`.
    ///
    /// * `offset` is the byte offset within the file.
    /// * `key_id` specifies which of the unwrapped keys to use.
    /// * `buffer` is mutated in place.
    ///
    /// `buffer` *must* be 16 byte aligned.
    pub fn encrypt(&self, offset: u64, key_id: u64, buffer: &mut [u8]) -> Result<(), Error> {
        trace_duration!("encrypt");
        assert_eq!(offset % SECTOR_SIZE, 0);
        let cipher = &self
            .0
            .iter()
            .find(|cipher| cipher.id == key_id)
            .ok_or(anyhow!("Key not found"))?
            .cipher;
        let mut sector_offset = offset / SECTOR_SIZE;
        for sector in buffer.chunks_exact_mut(SECTOR_SIZE as usize) {
            let mut tweak = Tweak(sector_offset as u128);
            // The same key is used for encrypting the data and computing the tweak.
            cipher.encrypt_block(GenericArray::from_mut_slice(tweak.as_bytes_mut()));
            cipher.encrypt_with_backend(XtsProcessor::new(tweak, sector));
            sector_offset += 1;
        }
        Ok(())
    }
}

/// A thin wrapper around a ChaCha20 stream cipher.  This will use a zero nonce. **NOTE**: Great
/// care must be taken not to encrypt different plaintext with the same key and offset (even across
/// multiple boots), so consider if this suits your purpose before using it.
pub struct StreamCipher(ChaCha20);

impl StreamCipher {
    pub fn new(key: &UnwrappedKey, offset: u64) -> Self {
        let mut cipher =
            Self(ChaCha20::new(Key::from_slice(&key.key), /* nonce: */ &[0; 12].into()));
        cipher.0.seek(offset);
        cipher
    }

    pub fn encrypt(&mut self, buffer: &mut [u8]) {
        trace_duration!("StreamCipher::encrypt");
        self.0.apply_keystream(buffer);
    }

    pub fn decrypt(&mut self, buffer: &mut [u8]) {
        trace_duration!("StreamCipher::decrypt");
        self.0.apply_keystream(buffer);
    }

    pub fn offset(&self) -> u64 {
        self.0.current_pos()
    }
}

/// Different keys are used for metadata and data in order to make certain operations requiring a
/// metadata key rotation (e.g. secure erase) more efficient.
pub enum KeyPurpose {
    /// The key will be used to wrap user data.
    Data,
    /// The key will be used to wrap internal metadata.
    Metadata,
}

/// An interface trait with the ability to wrap and unwrap encryption keys.
///
/// Note that existence of this trait does not imply that an object will **securely**
/// wrap and unwrap keys; rather just that it presents an interface for wrapping operations.
#[async_trait]
pub trait Crypt: Send + Sync {
    /// `owner` is intended to be used such that when the key is wrapped, it appears to be different
    /// to that of the same key wrapped by a different owner.  In this way, keys can be shared
    /// amongst different filesystem objects (e.g. for clones), but it is not possible to tell just
    /// by looking at the wrapped keys.
    async fn create_key(
        &self,
        owner: u64,
        purpose: KeyPurpose,
    ) -> Result<(WrappedKey, UnwrappedKey), Error>;

    // Unwraps a single key.
    async fn unwrap_key(&self, wrapped_key: &WrappedKey, owner: u64)
        -> Result<UnwrappedKey, Error>;

    /// Unwraps the keys and stores the result in UnwrappedKeys.
    async fn unwrap_keys(&self, keys: &WrappedKeys, owner: u64) -> Result<UnwrappedKeys, Error> {
        let mut futures = vec![];
        for (key_id, key) in keys.iter() {
            futures.push(async move { Ok((*key_id, self.unwrap_key(key, owner).await?)) });
        }
        futures::future::try_join_all(futures).await
    }
}

// This assumes little-endianness which is likely to always be the case.
assert_cfg!(target_endian = "little");
#[derive(AsBytes, FromBytes, FromZeroes)]
#[repr(C)]
struct Tweak(u128);

// To be used with encrypt|decrypt_with_backend.
struct XtsProcessor<'a> {
    tweak: Tweak,
    data: &'a mut [u8],
}

impl<'a> XtsProcessor<'a> {
    // `tweak` should be encrypted.  `data` should be a single sector and *must* be 16 byte aligned.
    fn new(tweak: Tweak, data: &'a mut [u8]) -> Self {
        assert_eq!(data.as_ptr() as usize & 15, 0, "data must be 16 byte aligned");
        Self { tweak, data }
    }
}

impl BlockSizeUser for XtsProcessor<'_> {
    type BlockSize = U16;
}

impl BlockClosure for XtsProcessor<'_> {
    fn call<B: BlockBackend<BlockSize = Self::BlockSize>>(self, backend: &mut B) {
        let Self { mut tweak, data } = self;
        for chunk in data.chunks_exact_mut(16) {
            let ptr = chunk.as_mut_ptr() as *mut u128;
            // SAFETY: We know each chunk is exactly 16 bytes and it should be safe to transmute to
            // u128 and GenericArray<u8, U16>.  There are safe ways of doing the following, but this
            // is extremely performance sensitive, and even seemingly innocuous changes here can
            // have an order-of-maginature impact on what the compiler produces and that can be seen
            // in our benchmarks.  This assumes little-endianness which is likely to always be the
            // case.
            unsafe {
                *ptr ^= tweak.0;
                let chunk = ptr as *mut GenericArray<u8, U16>;
                backend.proc_block(InOut::from_raw(chunk, chunk));
                *ptr ^= tweak.0;
            }
            tweak.0 = (tweak.0 << 1) ^ ((tweak.0 as i128 >> 127) as u128 & 0x87);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamCipher, UnwrappedKey};

    #[test]
    fn test_stream_cipher_offset() {
        let key = UnwrappedKey::new([
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32,
        ]);
        let mut cipher1 = StreamCipher::new(&key, 0);
        let mut p1 = [1, 2, 3, 4];
        let mut c1 = p1.clone();
        cipher1.encrypt(&mut c1);

        let mut cipher2 = StreamCipher::new(&key, 1);
        let p2 = [5, 6, 7, 8];
        let mut c2 = p2.clone();
        cipher2.encrypt(&mut c2);

        let xor_fn = |buf1: &mut [u8], buf2| {
            for (b1, b2) in buf1.iter_mut().zip(buf2) {
                *b1 ^= b2;
            }
        };

        // Check that c1 ^ c2 != p1 ^ p2 (which would be the case if the same offset was used for
        // both ciphers).
        xor_fn(&mut c1, &c2);
        xor_fn(&mut p1, &p2);
        assert_ne!(c1, p1);
    }
}
