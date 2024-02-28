// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{collections::vec_deque, time::Duration};

pub trait SizeOf {
    /// Size in bytes of an object. Makes no distinction between stack and heap allocated memory.
    fn size_of(&self) -> usize;
}

/// An implementor has some concept of the time at which it was created and knows how to retrieve
/// that time.
pub trait CreatedAt {
    /// Return the `Duration` since the Unix Epoch that represents when `self` was created.
    fn created_at(&self) -> Duration;
}

/// Mutable Iterator over the elements in a `BoundedQueue`
type IterMut<'a, T> = vec_deque::IterMut<'a, T>;

/// A `BoundedQueue` is a collection that holds elements with eviction minimums on the size (in
/// bytes) of the collection and how long it will hold elements. If both the `eviction_size_minimum`
/// and the `eviction_age_minimum` are exceeded, the oldest elements will be dropped until either
/// the size of the collection or the age of the oldest element no longer exceed the requested
/// minimum. `eviction_size_maximum` is the maximum possible size of all data stored in a
/// `BoundedQueue` regardless of age or number of elements stored. This value can never be
/// exceeded, even if this results in zero packets being stored. A `eviction_size_maximum` of
/// 0 indicates no maximum.
///
/// The `BoundedQueue` only supports inserting new elements to the end of the queue.
pub(crate) struct BoundedQueue<T> {
    eviction_age_minimum: Duration,
    eviction_size_minimum: usize,
    eviction_size_maximum: usize,
    current_size: usize,
    inner: vec_deque::VecDeque<T>,
}

impl<T> BoundedQueue<T>
where
    T: SizeOf + CreatedAt,
{
    /// Create an empty `BoundedQueue` with the specified eviction minimums.
    pub fn new(
        eviction_size_minimum: usize,
        eviction_size_maximum: usize,
        eviction_age_minimum: Duration,
    ) -> BoundedQueue<T> {
        BoundedQueue {
            eviction_age_minimum,
            eviction_size_minimum,
            eviction_size_maximum,
            current_size: 0,
            inner: vec_deque::VecDeque::new(),
        }
    }

    /// Calculate whether adding `item` would make this queue large enough to evict old packets.
    fn eviction_size_minimum_reached(&self, item: &T) -> bool {
        self.current_size + item.size_of() > self.eviction_size_minimum
    }

    /// Calculate whether adding `item` would make this queue large enough to force eviction of
    /// old packets.
    fn eviction_size_maximum_reached(&self, item: &T) -> bool {
        if self.eviction_size_maximum == 0 {
            false
        } else {
            self.current_size + item.size_of() > self.eviction_size_maximum
        }
    }

    /// When `item` is added, returns true if the oldest item will be too old.
    fn oldest_item_will_expire(&self, item: &T) -> bool {
        if self.inner.is_empty() {
            return false;
        }
        (item.created_at() - self.inner[0].created_at()) > self.eviction_age_minimum
    }

    /// Insert a new item to the end of the `BoundedQueue`, removing items as needed to make room.
    pub fn insert(&mut self, item: T) {
        // Keep removing items until the new one fits.
        while self.eviction_size_maximum_reached(&item)
            || (self.eviction_size_minimum_reached(&item) && self.oldest_item_will_expire(&item))
        {
            // return early without insertion if the buffer is empty but the item still will not
            // fit.
            if self.inner.is_empty() {
                return;
            }
            self.current_size -= self.inner.pop_front().map(|item| item.size_of()).unwrap_or(0);
        }
        self.current_size += item.size_of();
        self.inner.push_back(item);
    }

    /// Return an `Iterator` over mutable references to elements ordered from oldest to newest.
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        self.inner.iter_mut()
    }

    /// Returns the number of elements in the queue
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// The total size in bytes of all elements stored in the queue.
    pub fn byte_len(&self) -> usize {
        self.current_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Dummy struct that implements the SizeOf and CreatedAt traits required for elements of
    // a `BoundedQueue`
    #[derive(Debug)]
    struct Record {
        created_at: Duration,
        value: u64,
    }

    impl From<u64> for Record {
        fn from(value: u64) -> Self {
            Record { created_at: Duration::from_secs(value), value: value }
        }
    }

    impl SizeOf for Record {
        fn size_of(&self) -> usize {
            std::mem::size_of::<Record>()
        }
    }

    impl CreatedAt for Record {
        fn created_at(&self) -> Duration {
            self.created_at
        }
    }

    // Return a vec of the values of a record. For use in asserting the state of a `BoundedQueue` in
    // tests.
    fn to_values(queue: &mut BoundedQueue<Record>) -> Vec<u64> {
        queue.iter_mut().map(|n| n.value).collect()
    }

    #[test]
    fn test_bounded_queue() {
        // create a queue with age 0 and test inserting elements into it
        let mut queue: BoundedQueue<Record> = BoundedQueue::new(
            3 * std::mem::size_of::<Record>(),
            3 * std::mem::size_of::<Record>(),
            Duration::new(0, 0),
        );
        assert!(!queue.eviction_size_minimum_reached(&0.into()));
        assert_eq!(to_values(&mut queue), Vec::<u64>::new());
        assert_eq!(queue.len(), 0);
        queue.insert(0.into());
        assert!(!queue.eviction_size_minimum_reached(&1.into()));
        queue.insert(1.into());
        queue.insert(2.into());
        assert!(queue.eviction_size_minimum_reached(&3.into()));
        assert_eq!(to_values(&mut queue), vec![0, 1, 2]);
        assert_eq!(queue.len(), 3);
        queue.insert(3.into());
        assert!(queue.eviction_size_minimum_reached(&4.into()));
        assert_eq!(to_values(&mut queue), vec![1, 2, 3]);
        queue.insert(4.into());
        assert_eq!(to_values(&mut queue), vec![2, 3, 4]);
        assert_eq!(queue.len(), 3);

        // create a queue with size 0 and test inserting elements into it
        let mut queue: BoundedQueue<Record> =
            BoundedQueue::new(0, 10 * std::mem::size_of::<Record>(), Duration::new(2, 0));
        assert!(!queue.oldest_item_will_expire(&0.into()));
        assert_eq!(to_values(&mut queue), Vec::<u64>::new());
        queue.insert(0.into());
        assert!(!queue.oldest_item_will_expire(&1.into()));
        queue.insert(1.into());
        queue.insert(2.into());
        assert!(queue.oldest_item_will_expire(&3.into()));
        assert_eq!(to_values(&mut queue), vec![0, 1, 2]);
        queue.insert(3.into());
        assert!(queue.oldest_item_will_expire(&4.into()));
        assert_eq!(to_values(&mut queue), vec![1, 2, 3]);
        queue.insert(4.into());
        assert_eq!(to_values(&mut queue), vec![2, 3, 4]);

        // Create a queue with some size and some age test inserting elements into it
        let mut queue: BoundedQueue<Record> = BoundedQueue::new(
            3 * std::mem::size_of::<Record>(),
            5 * std::mem::size_of::<Record>(),
            Duration::new(3, 0),
        );
        assert_eq!(to_values(&mut queue), Vec::<u64>::new());
        queue.insert(0.into());
        queue.insert(1.into());
        queue.insert(2.into());
        assert_eq!(to_values(&mut queue), vec![0, 1, 2]);
        queue.insert(3.into());
        assert_eq!(to_values(&mut queue), vec![0, 1, 2, 3]);

        // Both space and age limits are exceeded.
        // The expected behavior is to evict old items as new ones are added
        assert!(queue.eviction_size_minimum_reached(&4.into()));
        assert!(queue.oldest_item_will_expire(&4.into()));
        queue.insert(4.into());
        assert_eq!(to_values(&mut queue), vec![1, 2, 3, 4]);
        queue.insert(6.into());
        assert_eq!(to_values(&mut queue), vec![3, 4, 6]);
        queue.insert(Record { created_at: Duration::new(6, 1), value: 7 });
        assert_eq!(to_values(&mut queue), vec![4, 6, 7]);

        // Only space limit is exceeded.
        // The expected behavior is to keep all items even as new items are added.
        // Space is allowed to grow to contain elements that are within the time limit.
        let record = Record { created_at: Duration::new(6, 2), value: 8 };
        assert!(queue.eviction_size_minimum_reached(&record));
        assert!(!queue.oldest_item_will_expire(&record));
        queue.insert(record);
        assert_eq!(to_values(&mut queue), vec![4, 6, 7, 8]);
        queue.insert(Record { created_at: Duration::new(6, 3), value: 9 });
        assert_eq!(to_values(&mut queue), vec![4, 6, 7, 8, 9]);

        // Both space and age limits are exceeded.
        // The expected behavior is to evict old items as new ones are added
        assert!(queue.eviction_size_minimum_reached(&10.into()));
        assert!(queue.oldest_item_will_expire(&10.into()));
        queue.insert(10.into());
        assert_eq!(to_values(&mut queue), vec![8, 9, 10]);

        let mut queue: BoundedQueue<Record> = BoundedQueue::new(
            2 * std::mem::size_of::<Record>(),
            2 * std::mem::size_of::<Record>(),
            Duration::new(3, 0),
        );
        queue.insert(0.into());
        // Only age limit is exceeded.
        // The expected behavior is to keep all items even as new items are added.
        // Space is allowed to grow to contain elements that are still within the space limit.
        assert!(!queue.eviction_size_minimum_reached(&4.into()));
        assert!(queue.oldest_item_will_expire(&4.into()));
        queue.insert(4.into());
        assert_eq!(to_values(&mut queue), vec![0, 4]);

        // Both space and age limit is exceeded.
        // The expected behavior is to evict old items as new ones are added
        assert!(queue.eviction_size_minimum_reached(&5.into()));
        assert!(queue.oldest_item_will_expire(&5.into()));
        queue.insert(5.into());
        assert_eq!(to_values(&mut queue), vec![4, 5]);

        let mut queue: BoundedQueue<Record> =
            BoundedQueue::new(0, 2 * std::mem::size_of::<Record>(), Duration::new(3, 0));
        queue.insert(0.into());

        // Maximum space limit is exceeded.
        // The expected behavior is that old items are evicted as new ones are added.
        // Space is not allowed to grow larger than the maximum space limit regardless
        // of age.
        let record = Record { created_at: Duration::new(1, 0), value: 1 };
        assert!(queue.eviction_size_minimum_reached(&record));
        assert!(!queue.eviction_size_maximum_reached(&record));
        assert!(!queue.oldest_item_will_expire(&record));
        queue.insert(record);
        assert_eq!(to_values(&mut queue), vec![0, 1]);
        queue.insert(Record { created_at: Duration::new(1, 1), value: 2 });
        assert_eq!(to_values(&mut queue), vec![1, 2]);

        // A queue with a hard max size of 1 will not accept any items inserted into it
        // because it is too small to fit any.
        let mut queue: BoundedQueue<Record> = BoundedQueue::new(0, 1, Duration::new(3, 0));

        let record = 0.into();
        assert!(queue.eviction_size_minimum_reached(&record));
        assert!(queue.eviction_size_maximum_reached(&record));

        // This element will be evicted immediately a.k.a. not inserted.
        queue.insert(record);
        assert_eq!(queue.len(), 0);
    }
}
