
use std::iter::Peekable;
use PutBack;
#[cfg(feature = "use_std")]
use PutBackN;

/// An iterator that allows peeking at an element before deciding to accept it.
///
/// See [`.peeking_take_while()`](trait.Itertools.html#method.peeking_take_while)
/// for more information.
///
/// This is implemented by peeking adaptors like peekable and put back,
/// but also by a few iterators that can be peeked natively, like the slice’s
/// by reference iterator (`std::slice::Iter`).
pub trait PeekingNext : Iterator {
    /// Pass a reference to the next iterator element to the closure `accept`;
    /// if `accept` returns true, return it as the next element,
    /// else None.
    fn peeking_next<F>(&mut self, accept: F) -> Option<Self::Item>
        where F: FnOnce(&Self::Item) -> bool;
}

impl<I> PeekingNext for Peekable<I>
    where I: Iterator,
{
    fn peeking_next<F>(&mut self, accept: F) -> Option<Self::Item>
        where F: FnOnce(&Self::Item) -> bool
    {
        if let Some(r) = self.peek() {
            if !accept(r) {
                return None;
            }
        }
        self.next()
    }
}

impl<I> PeekingNext for PutBack<I>
    where I: Iterator,
{
    fn peeking_next<F>(&mut self, accept: F) -> Option<Self::Item>
        where F: FnOnce(&Self::Item) -> bool
    {
        if let Some(r) = self.next() {
            if !accept(&r) {
                self.put_back(r);
                return None;
            }
            Some(r)
        } else {
            None
        }
    }
}

#[cfg(feature = "use_std")]
impl<I> PeekingNext for PutBackN<I>
    where I: Iterator,
{
    fn peeking_next<F>(&mut self, accept: F) -> Option<Self::Item>
        where F: FnOnce(&Self::Item) -> bool
    {
        if let Some(r) = self.next() {
            if !accept(&r) {
                self.put_back(r);
                return None;
            }
            Some(r)
        } else {
            None
        }
    }
}

/// An iterator adaptor that takes items while a closure returns `true`.
///
/// See [`.peeking_take_while()`](../trait.Itertools.html#method.peeking_take_while)
/// for more information.
#[must_use = "iterator adaptors are lazy and do nothing unless consumed"]
pub struct PeekingTakeWhile<'a, I: 'a, F>
    where I: Iterator,
{
    iter: &'a mut I,
    f: F,
}

/// Create a PeekingTakeWhile
pub fn peeking_take_while<I, F>(iter: &mut I, f: F) -> PeekingTakeWhile<I, F>
    where I: Iterator,
{
    PeekingTakeWhile {
        iter: iter,
        f: f,
    }
}

impl<'a, I, F> Iterator for PeekingTakeWhile<'a, I, F>
    where I: PeekingNext,
          F: FnMut(&I::Item) -> bool,

{
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.peeking_next(&mut self.f)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (_, hi) = self.iter.size_hint();
        (0, hi)
    }
}

// Some iterators are so lightweight we can simply clone them to save their
// state and use that for peeking.
macro_rules! peeking_next_by_clone {
    ([$($typarm:tt)*] $type_:ty) => {
        impl<$($typarm)*> PeekingNext for $type_ {
            fn peeking_next<F>(&mut self, accept: F) -> Option<Self::Item>
                where F: FnOnce(&Self::Item) -> bool
            {
                let saved_state = self.clone();
                if let Some(r) = self.next() {
                    if !accept(&r) {
                        *self = saved_state;
                    } else {
                        return Some(r)
                    }
                }
                None
            }
        }
    }
}

peeking_next_by_clone! { ['a, T] ::std::slice::Iter<'a, T> }
peeking_next_by_clone! { ['a] ::std::str::Chars<'a> }
peeking_next_by_clone! { ['a] ::std::str::CharIndices<'a> }
peeking_next_by_clone! { ['a] ::std::str::Bytes<'a> }
peeking_next_by_clone! { ['a, T] ::std::option::Iter<'a, T> }
peeking_next_by_clone! { ['a, T] ::std::result::Iter<'a, T> }
peeking_next_by_clone! { [T] ::std::iter::Empty<T> }
#[cfg(feature = "use_std")]
peeking_next_by_clone! { ['a, T] ::std::collections::linked_list::Iter<'a, T> }
#[cfg(feature = "use_std")]
peeking_next_by_clone! { ['a, T] ::std::collections::vec_deque::Iter<'a, T> }

// cloning a Rev has no extra overhead; peekable and put backs are never DEI.
peeking_next_by_clone! { [I: Clone + PeekingNext + DoubleEndedIterator]
                         ::std::iter::Rev<I> }
