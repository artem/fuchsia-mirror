// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::types::{Key, Value},
    std::fmt,
};

pub trait ObjectCachePlaceholder<V: Value>: Send + Sync {
    /// Consumes itself in delivering the cache value for which the placeholder was reserved.
    /// Passing None for value should not be inserted into the cache, but interpreted as an
    /// incomplete search.
    fn complete(self: Box<Self>, value: Option<&V>);
}

/// Possible results for a cache `lookup_or_reserve()`
pub enum ObjectCacheResult<'a, V: Value> {
    /// Contains the value successfully retrieved from the cache.
    Value(V),
    /// The object was not found in the cache, so this placeholder can be used to insert the
    /// calculated result.
    Placeholder(Box<dyn ObjectCachePlaceholder<V> + 'a>),
    /// Returned for items that are not wanted to be inserted into the cache.
    NoCache,
}

impl<'a, V: Value> fmt::Debug for ObjectCacheResult<'a, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, contents) = match self {
            Self::Value(v) => ("Value", Some(format!("{:?}", v))),
            Self::NoCache => ("NoCache", None),
            Self::Placeholder(_) => ("Placeholder", None),
        };
        if contents.is_some() {
            f.debug_struct("ObjectCacheResult").field(name, &contents.unwrap()).finish()
        } else {
            f.debug_struct("ObjectCacheResult").field(name, &"").finish()
        }
    }
}

pub trait ObjectCache<K: Key, V: Value>: Send + Sync {
    /// Looks up a key in the cache and may return a cached value for it. See `ObjectCacheResult`.
    fn lookup_or_reserve<'a>(&'a self, key: &K) -> ObjectCacheResult<'_, V>;

    /// Removes key from cache if `value` is None, invalidates the results of placeholders that have
    /// not been resolved. When `value` is provided then the value may be inserted, and may replace
    /// an existing value.
    fn invalidate(&self, key: K, value: Option<V>);
}

/// A cache that will always return NoCache in lookups, and does no actual work.
pub struct NullCache {}

impl<K: Key, V: Value> ObjectCache<K, V> for NullCache {
    fn lookup_or_reserve(&self, _key: &K) -> ObjectCacheResult<'_, V> {
        ObjectCacheResult::NoCache
    }

    fn invalidate(&self, _key: K, _value: Option<V>) {}
}
