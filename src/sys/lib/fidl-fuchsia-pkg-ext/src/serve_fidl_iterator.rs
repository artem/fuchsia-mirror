// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::Measurable,
    anyhow::{Context as _, Result},
    fidl_fuchsia_pkg::{
        BlobIdIteratorNextResponder, BlobIdIteratorRequest, BlobIdIteratorRequestStream,
        BlobInfoIteratorNextResponder, BlobInfoIteratorRequest, BlobInfoIteratorRequestStream,
        PackageIndexEntry, PackageIndexIteratorNextResponder, PackageIndexIteratorRequest,
        PackageIndexIteratorRequestStream,
    },
    fuchsia_zircon_types::ZX_CHANNEL_MAX_MSG_BYTES,
    futures::prelude::*,
};

/// Serves fidl iterators like:
///
/// protocol PayloadIterator {
///    Next() -> (vector<Payload>:MAX payloads);
/// };
///
/// from a slice of `Payload`s and what is effectively a stream of `PayloadIterator` requests.
///
/// Fills each response to `Next()` with as many entries as will fit in a fidl message. The
/// returned future completes after `Next()` yields an empty response or the iterator
/// is interrupted (client closes the channel or the task encounters a FIDL layer error).
///
/// To use with a new protocol (e.g. `PayloadIterator`), in this crate:
///   1. implement `FidlIteratorRequestStream` for `PayloadIteratorRequestStream`
///   2. implement `FidlIteratorNextResponder` for `PayloadIteratorNextResponder`
///   3. implement `Measurable` for `Payload` using functions generated by
///      //tools/fidl/measure-tape
pub async fn serve_fidl_iterator<I>(
    mut items: impl AsMut<[<I::Responder as FidlIteratorNextResponder>::Item]>,
    mut stream: I,
) -> Result<()>
where
    I: FidlIteratorRequestStream,
{
    let mut items = Chunker::new(items.as_mut());

    loop {
        let mut chunk = items.next();

        let responder = match stream.try_next().await.context("while waiting for next() request")? {
            None => break,
            Some(request) => I::request_to_responder(request),
        };

        let () = responder.send_chunk(&mut chunk).context("while responding")?;

        // Yield a single empty chunk, then stop serving the protocol.
        if chunk.is_empty() {
            break;
        }
    }

    Ok(())
}

/// A FIDL request stream for a FIDL protocol following the iterator pattern.
pub trait FidlIteratorRequestStream:
    fidl::endpoints::RequestStream + TryStream<Error = fidl::Error>
{
    type Responder: FidlIteratorNextResponder;

    fn request_to_responder(request: <Self as TryStream>::Ok) -> Self::Responder;
}

/// A responder to a Next() request for a FIDL iterator.
pub trait FidlIteratorNextResponder {
    type Item: Measurable + fidl::encoding::Encodable;

    fn send_chunk(self, chunk: &mut [Self::Item]) -> Result<(), fidl::Error>;
}

impl FidlIteratorRequestStream for PackageIndexIteratorRequestStream {
    type Responder = PackageIndexIteratorNextResponder;

    fn request_to_responder(request: PackageIndexIteratorRequest) -> Self::Responder {
        let PackageIndexIteratorRequest::Next { responder } = request;
        responder
    }
}

impl FidlIteratorNextResponder for PackageIndexIteratorNextResponder {
    type Item = PackageIndexEntry;

    fn send_chunk(self, chunk: &mut [Self::Item]) -> Result<(), fidl::Error> {
        self.send(&mut chunk.iter_mut())
    }
}

impl FidlIteratorRequestStream for BlobInfoIteratorRequestStream {
    type Responder = BlobInfoIteratorNextResponder;

    fn request_to_responder(request: BlobInfoIteratorRequest) -> Self::Responder {
        let BlobInfoIteratorRequest::Next { responder } = request;
        responder
    }
}

impl FidlIteratorRequestStream for BlobIdIteratorRequestStream {
    type Responder = BlobIdIteratorNextResponder;

    fn request_to_responder(request: BlobIdIteratorRequest) -> Self::Responder {
        let BlobIdIteratorRequest::Next { responder } = request;
        responder
    }
}

impl FidlIteratorNextResponder for BlobInfoIteratorNextResponder {
    type Item = fidl_fuchsia_pkg::BlobInfo;

    fn send_chunk(self, chunk: &mut [Self::Item]) -> Result<(), fidl::Error> {
        self.send(&mut chunk.iter_mut())
    }
}

impl FidlIteratorNextResponder for BlobIdIteratorNextResponder {
    type Item = fidl_fuchsia_pkg::BlobId;

    fn send_chunk(self, chunk: &mut [Self::Item]) -> Result<(), fidl::Error> {
        self.send(&mut chunk.iter_mut())
    }
}

// FIXME(52297) This constant would ideally be exported by the `fidl` crate.
// sizeof(TransactionHeader) + sizeof(VectorHeader)
const FIDL_VEC_RESPONSE_OVERHEAD_BYTES: usize = 32;

/// Helper to split a slice of items into chunks that will fit in a single FIDL vec response.
///
/// Note, Chunker assumes the fixed overhead of a single fidl response header and a single vec
/// header per chunk.  It must not be used with more complex responses.
struct Chunker<'a, I> {
    items: &'a mut [I],
}

impl<'a, I> Chunker<'a, I>
where
    I: Measurable,
{
    fn new(items: &'a mut [I]) -> Self {
        Self { items }
    }

    /// Produce the next chunk of items to respond with. Iteration stops when this method returns
    /// an empty slice, which occurs when either:
    /// * All items have been returned
    /// * Chunker encounters an item so large that it cannot even be stored in a response
    ///   dedicated to just that one item.
    ///
    /// Once next() returns an empty slice, it will continue to do so in future calls.
    fn next(&mut self) -> &'a mut [I] {
        let mut bytes_used: usize = FIDL_VEC_RESPONSE_OVERHEAD_BYTES;
        let mut entry_count = 0;

        for entry in &*self.items {
            bytes_used += entry.measure();
            if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize {
                break;
            }
            entry_count += 1;
        }

        // tmp/swap dance to appease the borrow checker.
        let tmp = std::mem::replace(&mut self.items, &mut []);
        let (chunk, rest) = tmp.split_at_mut(entry_count);
        self.items = rest;
        chunk
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl_fuchsia_pkg::{BlobInfoIteratorMarker, PackageIndexIteratorMarker},
        fuchsia_async::Task,
        fuchsia_hash::HashRangeFull,
        fuchsia_pkg::PackagePath,
        proptest::prelude::*,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Byte(u8);

    impl Measurable for Byte {
        fn measure(&self) -> usize {
            1
        }
    }

    #[test]
    fn chunker_fuses() {
        let items = &mut [Byte(42)];
        let mut chunker = Chunker::new(items);

        assert_eq!(chunker.next(), &mut [Byte(42)]);
        assert_eq!(chunker.next(), &mut []);
        assert_eq!(chunker.next(), &mut []);
    }

    #[test]
    fn chunker_chunks_at_expected_boundary() {
        const BYTES_PER_CHUNK: usize =
            ZX_CHANNEL_MAX_MSG_BYTES as usize - FIDL_VEC_RESPONSE_OVERHEAD_BYTES;

        // Expect to fill 2 full chunks with 1 item left over.
        let mut items =
            (0..=(BYTES_PER_CHUNK as u64 * 2)).map(|n| Byte(n as u8)).collect::<Vec<Byte>>();
        let expected = items.clone();
        let mut chunker = Chunker::new(&mut items);

        let mut actual: Vec<Byte> = vec![];

        for _ in 0..2 {
            let chunk = chunker.next();
            assert_eq!(chunk.len(), BYTES_PER_CHUNK);

            actual.extend(&*chunk);
        }

        let chunk = chunker.next();
        assert_eq!(chunk.len(), 1);
        actual.extend(&*chunk);

        assert_eq!(actual, expected);
    }

    #[test]
    fn chunker_terminates_at_too_large_item() {
        #[derive(Debug, PartialEq, Eq)]
        struct TooBig;
        impl Measurable for TooBig {
            fn measure(&self) -> usize {
                ZX_CHANNEL_MAX_MSG_BYTES as usize
            }
        }

        let items = &mut [TooBig];
        let mut chunker = Chunker::new(items);
        assert_eq!(chunker.next(), &mut []);
    }

    #[test]
    fn verify_fidl_vec_response_overhead() {
        let vec_response_overhead = {
            use fidl::encoding::{DynamicFlags, TransactionHeader, TransactionMessage};

            let mut nop: Vec<()> = vec![];
            let mut msg = TransactionMessage {
                header: TransactionHeader::new(0, 0, DynamicFlags::empty()),
                body: &mut nop,
            };

            fidl::encoding::with_tls_encoded::<_, _, false>(&mut msg, |bytes, _handles| {
                Result::<_, fidl::Error>::Ok(bytes.len())
            })
            .unwrap()
        };
        assert_eq!(vec_response_overhead, FIDL_VEC_RESPONSE_OVERHEAD_BYTES);
    }

    proptest! {
        #![proptest_config(ProptestConfig{
            // Disable persistence to avoid the warning for not running in the
            // source code directory (since we're running on a Fuchsia target)
            failure_persistence: None,
            .. ProptestConfig::default()
        })]

        #[test]
        fn serve_fidl_iterator_yields_expected_entries(items: Vec<crate::BlobInfo>) {
            let mut executor = fuchsia_async::TestExecutor::new().unwrap();
            executor.run_singlethreaded(async move {
                let (proxy, stream) =
                    fidl::endpoints::create_proxy_and_stream::<BlobInfoIteratorMarker>().unwrap();
                let mut actual_items = vec![];

                let ((), ()) = futures::future::join(
                    async {
                        let items = items
                            .iter()
                            .cloned()
                            .map(fidl_fuchsia_pkg::BlobInfo::from)
                            .collect::<Vec<_>>();
                            serve_fidl_iterator(items, stream).await.unwrap()
                    },
                    async {
                        loop {
                            let chunk = proxy.next().await.unwrap();
                            if chunk.is_empty() {
                                break;
                            }
                            let chunk = chunk.into_iter().map(crate::BlobInfo::from);
                            actual_items.extend(chunk);
                        }
                    },
                )
                .await;

                assert_eq!(items, actual_items);
            })
        }
    }

    const PACKAGE_INDEX_CHUNK_SIZE_MAX: usize = 818;

    // FIDL message is at most 65,536 bytes because of zx_channel_write [1].
    // `PackageIndexIterator.Next()` return value size, encoded [2], is:
    // 16 bytes FIDK transaction header +
    // 16 bytes vector header +
    // N * (16 bytes string header (from url field of struct PackageUrl) +
    // L bytes string content +
    // 32 bytes array.
    // This totals in 32 + N * (48 + L), where L is 8-byte aligned
    // because secondary objects (e.g. string contents) are 8-byte aligned.
    //
    // The shortest possible package url is 29 bytes "fuchsia-pkg://fuchsia.com/a/0".
    //
    // And the longest is 283 bytes, which is 288 bytes with 8-byte alignment, so
    // PACKAGE_INDEX_CHUNK_SIZE_MIN => 65536 <= 32 + N * (48 + 288) => N = 194
    //
    // [1] https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_write
    // [2] https://fuchsia.dev/fuchsia-src/reference/fidl/language/wire-format
    const PACKAGE_INDEX_CHUNK_SIZE_MIN: usize = 194;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn package_index_iterator_paginates_shortest_entries() {
        let names = ('a'..='z').cycle().map(|c| c.to_string());
        let paths = names.map(|name| {
            PackagePath::from_name_and_variant(name.parse().unwrap(), "0".parse().unwrap())
        });

        verify_package_index_iterator_pagination(paths, PACKAGE_INDEX_CHUNK_SIZE_MAX).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn package_index_iterator_paginates_longest_entries() {
        let names = ('a'..='z')
            .map(|c| std::iter::repeat(c).take(PackagePath::MAX_NAME_BYTES).collect::<String>())
            .cycle();
        let paths = names.map(|name| {
            PackagePath::from_name_and_variant(name.parse().unwrap(), "0".parse().unwrap())
        });

        verify_package_index_iterator_pagination(paths, PACKAGE_INDEX_CHUNK_SIZE_MIN).await;
    }

    async fn verify_package_index_iterator_pagination(
        paths: impl Iterator<Item = PackagePath>,
        expected_chunk_size: usize,
    ) {
        let package_entries: Vec<fidl_fuchsia_pkg::PackageIndexEntry> = paths
            .zip(HashRangeFull::default())
            .take(expected_chunk_size * 2)
            .map(|(path, hash)| fidl_fuchsia_pkg::PackageIndexEntry {
                package_url: fidl_fuchsia_pkg::PackageUrl {
                    url: format!("fuchsia-pkg://fuchsia.com/{}", path),
                },
                meta_far_blob_id: crate::BlobId::from(hash).into(),
            })
            .collect();

        let (iter, stream) =
            fidl::endpoints::create_proxy_and_stream::<PackageIndexIteratorMarker>().unwrap();
        let task = Task::local(serve_fidl_iterator(package_entries, stream));

        let chunk = iter.next().await.unwrap();
        assert_eq!(chunk.len(), expected_chunk_size);

        let chunk = iter.next().await.unwrap();
        assert_eq!(chunk.len(), expected_chunk_size);

        let chunk = iter.next().await.unwrap();
        assert_eq!(chunk.len(), 0);

        let () = task.await.unwrap();
    }
}
