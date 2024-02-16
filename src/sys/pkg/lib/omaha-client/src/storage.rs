// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::time::system_time_conversion::{
    checked_system_time_to_micros_from_epoch, micros_from_epoch_to_system_time,
};
use futures::future::{BoxFuture, FutureExt, TryFutureExt};
use std::time::SystemTime;
use tracing::error;

mod memory;
pub use memory::MemStorage;

#[cfg(test)]
mod stub;
#[cfg(test)]
pub use stub::StubStorage;

/// The Storage trait is used to access typed key=value storage, for persisting protocol state and
/// other data between runs of the update check process.
///
/// Implementations of this trait should cache values until commit() is called, and then perform
/// an atomic committing of all outstanding value changes.  On a given instance of Storage, a
/// get() following a set(), but before a commit() should also return the set() value (not the
/// previous value).
///
/// However, the expected usage of this trait within the library is to perform a series of get()'s
/// at startup, and then only set()+commit() after that.  The expectation being that this is being
/// used to persist state that needs to be maintained for continuity over a reboot (or power
/// outage).
///
/// A note on using the wrong type with a key: the result should be as if there is no value for
/// the key.  This is so that the Result::uwrap_or(<...default...>) pattern will work.
///
/// ```
/// # futures::executor::block_on(async {
/// use omaha_client::storage::{MemStorage, Storage};
/// use omaha_client::unless::Unless;
/// let mut storage = MemStorage::new();
/// storage.set_int("key", 345);
///
/// // value should be None:
/// let value: Option<String> = storage.get_string("key").await;
/// assert_eq!(None, value);
///
/// // value should be "default":
/// let value: String = storage.get_string("key").await.unwrap_or("default".to_string());
/// assert_eq!("default", value);
/// # });
/// ```
pub trait Storage {
    type Error: std::error::Error;

    /// Get a string from the backing store.  Returns None if there is no value for the given key,
    /// or if the value for the key has a different type.
    fn get_string<'a>(&'a self, key: &'a str) -> BoxFuture<'_, Option<String>>;

    /// Get an int from the backing store.  Returns None if there is no value for the given key,
    /// or if the value for the key has a different type.
    fn get_int<'a>(&'a self, key: &'a str) -> BoxFuture<'_, Option<i64>>;

    /// Get a boolean from the backing store.  Returns None if there is no value for the given key,
    /// or if the value for the key has a different type.
    fn get_bool<'a>(&'a self, key: &'a str) -> BoxFuture<'_, Option<bool>>;

    /// Set a value to be stored in the backing store.  The implementation should cache the value
    /// until the |commit()| fn is called, and then persist all cached values at that time.
    fn set_string<'a>(
        &'a mut self,
        key: &'a str,
        value: &'a str,
    ) -> BoxFuture<'_, Result<(), Self::Error>>;

    /// Set a value to be stored in the backing store.  The implementation should cache the value
    /// until the |commit()| fn is called, and then persist all cached values at that time.
    fn set_int<'a>(
        &'a mut self,
        key: &'a str,
        value: i64,
    ) -> BoxFuture<'_, Result<(), Self::Error>>;

    /// Set a value to be stored in the backing store.  The implementation should cache the value
    /// until the |commit()| fn is called, and then persist all cached values at that time.
    fn set_bool<'a>(
        &'a mut self,
        key: &'a str,
        value: bool,
    ) -> BoxFuture<'_, Result<(), Self::Error>>;

    /// Remove the value for |key| from the backing store.  The implementation should cache that
    /// the value has been removed until the |commit()| fn is called, and then persist all changes
    /// at that time.
    ///
    /// If there is no value for the key, this should return without error.
    fn remove<'a>(&'a mut self, key: &'a str) -> BoxFuture<'_, Result<(), Self::Error>>;

    /// Persist all cached values to storage.
    fn commit(&mut self) -> BoxFuture<'_, Result<(), Self::Error>>;
}

/// Extension trait that adds some features to Storage that can be implemented using the base
/// `Storage` implementation.
pub trait StorageExt: Storage {
    /// Set an Option<i64> to be stored in the backing store.  The implementation should cache the
    /// value until the |commit()| fn is called, and then persist all cached values at that time.
    /// If the Option is None, the implementation should call |remove()| for the key.
    fn set_option_int<'a>(
        &'a mut self,
        key: &'a str,
        value: Option<i64>,
    ) -> BoxFuture<'_, Result<(), Self::Error>> {
        match value {
            Some(value) => self.set_int(key, value),
            None => self.remove(key),
        }
    }

    /// Get a SystemTime from the backing store.  Returns None if there is no value for the given
    /// key, or if the value for the key has a different type.
    fn get_time<'a>(&'a self, key: &'a str) -> BoxFuture<'_, Option<SystemTime>> {
        self.get_int(key).map(|option| option.map(micros_from_epoch_to_system_time)).boxed()
    }

    /// Set a SystemTime to be stored in the backing store.  The implementation should cache the
    /// value until the |commit()| fn is called, and then persist all cached values at that time.
    /// Note that submicrosecond will be dropped.
    fn set_time<'a>(
        &'a mut self,
        key: &'a str,
        value: impl Into<SystemTime>,
    ) -> BoxFuture<'_, Result<(), Self::Error>> {
        self.set_option_int(key, checked_system_time_to_micros_from_epoch(value.into()))
    }

    /// Remove the value for |key| from the backing store, log an error message on error.
    fn remove_or_log<'a>(&'a mut self, key: &'a str) -> BoxFuture<'_, ()> {
        self.remove(key).unwrap_or_else(move |e| error!("Unable to remove {}: {}", key, e)).boxed()
    }

    /// Persist all cached values to storage, log an error message on error.
    fn commit_or_log(&mut self) -> BoxFuture<'_, ()> {
        self.commit().unwrap_or_else(|e| error!("Unable to commit persisted data: {}", e)).boxed()
    }
}

impl<T> StorageExt for T where T: Storage {}

/// The storage::tests module contains test vectors that implementations of the Storage trait should
/// pass.  These can be called with a Storage implementation as part of a test.
///
/// These are public so that implementors of the Storage trait (in other libraries or binaries) can
/// call them.
pub mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    /// These are tests for verifying that a given Storage implementation acts as expected.

    /// Test that the implementation stores, retrieves, and clears String values correctly.
    pub async fn do_test_set_get_remove_string<S: Storage>(storage: &mut S) {
        assert_eq!(None, storage.get_string("some key").await);

        storage.set_string("some key", "some value").await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some("some value".to_string()), storage.get_string("some key").await);

        storage.set_string("some key", "some other value").await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some("some other value".to_string()), storage.get_string("some key").await);

        storage.remove("some key").await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(None, storage.get_string("some key").await);
    }

    /// Test that the implementation stores, retrieves, and clears int values correctly.
    pub async fn do_test_set_get_remove_int<S: Storage>(storage: &mut S) {
        assert_eq!(None, storage.get_int("some int key").await);

        storage.set_int("some int key", 42).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some(42), storage.get_int("some int key").await);

        storage.set_int("some int key", 1).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some(1), storage.get_int("some int key").await);

        storage.remove("some int key").await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(None, storage.get_int("some int key").await);
    }

    /// Test that the implementation stores, retrieves, and clears Option<i64> values correctly.
    pub async fn do_test_set_option_int<S: Storage>(storage: &mut S) {
        assert_eq!(None, storage.get_int("some int key").await);

        storage.set_option_int("some int key", Some(42)).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some(42), storage.get_int("some int key").await);

        storage.set_option_int("some int key", None).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(None, storage.get_int("some int key").await);
    }

    /// Test that the implementation stores, retrieves, and clears bool values correctly.
    pub async fn do_test_set_get_remove_bool<S: Storage>(storage: &mut S) {
        assert_eq!(None, storage.get_bool("some bool key").await);

        storage.set_bool("some bool key", false).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some(false), storage.get_bool("some bool key").await);

        storage.set_bool("some bool key", true).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some(true), storage.get_bool("some bool key").await);

        storage.remove("some bool key").await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(None, storage.get_bool("some bool key").await);
    }

    /// Test that the implementation stores, retrieves, and clears bool values correctly.
    pub async fn do_test_set_get_remove_time<S: Storage>(storage: &mut S) {
        assert_eq!(None, storage.get_time("some time key").await);

        storage.set_time("some time key", SystemTime::UNIX_EPOCH).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some(SystemTime::UNIX_EPOCH), storage.get_time("some time key").await);

        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1234);
        storage.set_time("some time key", time).await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(Some(time), storage.get_time("some time key").await);

        storage.remove("some time key").await.unwrap();
        storage.commit().await.unwrap();
        assert_eq!(None, storage.get_time("some time key").await);
    }

    /// Test that the implementation returns None for a mismatch between value types
    pub async fn do_return_none_for_wrong_value_type<S: Storage>(storage: &mut S) {
        storage.set_int("some int key", 42).await.unwrap();
        assert_eq!(None, storage.get_string("some int key").await);
    }

    /// Test that a remove of a non-existent key causes no errors
    pub async fn do_ensure_no_error_remove_nonexistent_key<S: Storage>(storage: &mut S) {
        storage.set_string("some key", "some value").await.unwrap();
        storage.commit().await.unwrap();

        storage.remove("some key").await.unwrap();
        storage.remove("some key").await.unwrap();
    }
}
