// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{repository::Repository, repository_manager::Stats, TCP_KEEPALIVE_TIMEOUT},
    cobalt_sw_delivery_registry as metrics,
    delivery_blob::DeliveryBlobType,
    fidl_contrib::protocol_connector::ProtocolSender,
    fidl_fuchsia_metrics::MetricEvent,
    fidl_fuchsia_pkg::{self as fpkg},
    fidl_fuchsia_pkg_ext::{self as pkg, BlobId, BlobInfo, MirrorConfig, RepositoryConfig},
    fuchsia_cobalt_builders::MetricEventExt as _,
    fuchsia_pkg::PackageDirectory,
    fuchsia_sync::Mutex,
    fuchsia_trace as ftrace,
    fuchsia_url::AbsolutePackageUrl,
    fuchsia_zircon::Status,
    futures::{lock::Mutex as AsyncMutex, prelude::*, stream::FuturesUnordered},
    http_uri_ext::HttpUriExt as _,
    hyper::StatusCode,
    std::{
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc,
        },
        time::Duration,
    },
    tuf::metadata::{MetadataPath, MetadataVersion, TargetPath},
};

pub use fidl_fuchsia_pkg_ext::BasePackageIndex;

mod inspect;
mod resume;
mod retry;

#[derive(Clone, Copy, Debug, typed_builder::TypedBuilder)]
pub struct BlobFetchParams {
    header_network_timeout: Duration,
    body_network_timeout: Duration,
    download_resumption_attempts_limit: u64,
    blob_type: DeliveryBlobType,
}

impl BlobFetchParams {
    pub fn header_network_timeout(&self) -> Duration {
        self.header_network_timeout
    }

    pub fn body_network_timeout(&self) -> Duration {
        self.body_network_timeout
    }

    pub fn download_resumption_attempts_limit(&self) -> u64 {
        self.download_resumption_attempts_limit
    }

    pub fn blob_type(&self) -> DeliveryBlobType {
        self.blob_type
    }
}

pub async fn cache_package<'a>(
    repo: Arc<AsyncMutex<Repository>>,
    config: &'a RepositoryConfig,
    url: &'a AbsolutePackageUrl,
    gc_protection: fpkg::GcProtection,
    cache: &'a pkg::cache::Client,
    blob_fetcher: &'a BlobFetcher,
    cobalt_sender: ProtocolSender<MetricEvent>,
    trace_id: ftrace::Id,
) -> Result<(BlobId, PackageDirectory), CacheError> {
    let merkle = merkle_for_url(repo, url, cobalt_sender).await.map_err(CacheError::MerkleFor)?;
    // If a merkle pin was specified, use it, but only after having verified that the name and
    // variant exist in the TUF repo.  Note that this doesn't guarantee that the merkle pinned
    // package ever actually existed in the repo or that the merkle pin refers to the named
    // package.
    let merkle = if let Some(merkle_pin) = url.hash() { BlobId::from(merkle_pin) } else { merkle };

    let meta_far_blob = BlobInfo { blob_id: merkle, length: 0 };

    let mut get = cache.get(meta_far_blob, gc_protection)?;

    let blob_fetch_res = async {
        let mirrors = config.mirrors().to_vec().into();

        // Do not add the meta.far fetch to the queue if we are sure that the meta.far is already
        // cached to avoid blocking resolves of fully cached packages behind blob fetches (in the
        // case where the blob fetch queue is already at its concurrency limit).
        //
        // The NeededBlob created by this call to `make_open_meta_blob().open()` cannot be reused by
        // the queue because the queue could already contain a fetch request for the blob, which
        // would result in the NeededBlob being invalid by the time it is processed.
        // Once c++blobfs is removed and fxblob is changed to support concurrent writes of the same
        // blob that both complete successfully, we can pass the NeededBlob in, which will avoid
        // making an additional pkg-resolver -> pkg-cache -> blobfs -> pkg-cache -> pkg-resolver
        // FIDL loop, at the expense of sometimes downloading meta.fars multiple times.
        //
        // With fxblob, it is ok to open the blob for write (i.e. call `open` on the
        // DeferredOpenBlob) outside of the queue because fxblob allows concurrent creation attempts
        // as long as only one succeeds.
        //
        // With c++blob, it is *not* okay to open the blob for write (or even read) outside of the
        // fetch queue to test for presence because open connections even to partially written blobs
        // keep the blob alive. This means that this open creates the possibility for the following
        // race condition:
        // 1. this open occurs for resolve A
        // 2. a concurrent resolve attempt B (perhaps for a package that has this package as a
        //    subpackage) opens the blob for write
        // 3. resolve B obtains the blob size from the network
        // 4. resolve B calls Resize on the blob
        // 5. resolve B encounters an error that qualifes for retrying the fetch and closes the blob
        // 6. resolve B tries to open the blob for write again, which fails
        //
        // This is very unlikely to occur in practice because resolve A closes the connection
        // immediately, so this would need to be delayed somehow until after resolve B has made a
        // number of network operations and FIDL calls to remote services.
        //
        // Note that if this open occurs after resolve B has called Resize, the open will fail
        // instead of creating a blocking connection.
        //
        // The race could be prevented entirely by adding a method to NeededBlobs that uses
        // ReadDirents on /blob to check for blob presence.
        //
        // fxblob will soon fully support concurrent blob creation, pending
        // https://fxbug.dev/335870456#comment9.
        let fetch_meta_far = match get.make_open_meta_blob().open().await {
            Ok(Some(pkg::cache::NeededBlob { blob: _, closer })) => {
                let () = closer.close().await;
                true
            }
            Ok(None) => false,
            // The open for write will fail on c++blob if the queue is writing the blob.
            Err(_) => true,
        };
        if fetch_meta_far {
            let () = blob_fetcher
                .push(
                    merkle,
                    FetchBlobContext {
                        opener: get.make_open_meta_blob(),
                        mirrors: Arc::clone(&mirrors),
                        parent_trace_id: trace_id,
                    },
                )
                .await
                .expect("processor exists")
                .map_err(|e| CacheError::FetchMetaFar(e, merkle))?;
        }

        let mut fetches = FuturesUnordered::new();
        let mut missing_blobs = get.get_missing_blobs().fuse();
        let mut first_closed_error: Option<Arc<FetchError>> = None;

        loop {
            futures::select! {
                fetch_res = fetches.select_next_some() => {
                    match Result::<Result<_, Arc<FetchError>>, _>::expect(
                        fetch_res,
                        "processor exists"
                    ) {
                        Ok(()) => {}
                        Err(e) if e.is_unexpected_pkg_cache_closed() => {
                            first_closed_error.get_or_insert(e);
                        }
                        Err(e) => return Err(CacheError::FetchContentBlob(e, merkle)),
                    }
                }
                chunk = missing_blobs.select_next_some() => {
                    #[allow(clippy::needless_collect)]
                    // Not sure if this collect is significant -- without it, the
                    // compiler believes we are double-borrowing |get|.
                    let chunk = chunk?
                        .into_iter()
                        .map(|need| {
                            (
                                need.blob_id,
                                FetchBlobContext {
                                    // TODO(b/303737132) Consider checking if the blob is cached
                                    // before adding to the queue, like is done for meta.fars.
                                    opener: get.make_open_blob(need.blob_id),
                                    mirrors: Arc::clone(&mirrors),
                                    parent_trace_id: trace_id,
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                    let () = fetches.extend(blob_fetcher.push_all(chunk.into_iter()));
                }
                complete => {
                    match first_closed_error {
                        None =>  return Ok(()),
                        Some(e) => return Err(CacheError::FetchContentBlob(e, merkle)),
                    }
                }
            }
        }
    }
    .await;

    match blob_fetch_res {
        Ok(()) => Ok((merkle, get.finish().await?)),
        Err(e) => {
            get.abort().await;
            Err(e)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("fidl error")]
    Fidl(#[from] fidl::Error),

    #[error("while looking up merkle root for package")]
    MerkleFor(#[source] MerkleForError),

    #[error("while listing needed blobs for package")]
    ListNeeds(#[from] pkg::cache::ListMissingBlobsError),

    #[error("while fetching the meta.far: {1}")]
    FetchMetaFar(#[source] Arc<FetchError>, BlobId),

    #[error("while fetching content blob for meta.far {1}")]
    FetchContentBlob(#[source] Arc<FetchError>, BlobId),

    #[error("Get() request failed")]
    Get(#[from] pkg::cache::GetError),

    #[error("opening a blob from the blobstore")]
    Open(#[from] pkg::cache::OpenBlobError),
}

pub(crate) trait ToResolveError {
    fn to_resolve_error(&self) -> pkg::ResolveError;
}

impl ToResolveError for Status {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        match *self {
            Status::ACCESS_DENIED => pkg::ResolveError::AccessDenied,
            Status::IO => pkg::ResolveError::Io,
            Status::NOT_FOUND => pkg::ResolveError::PackageNotFound,
            Status::NO_SPACE => pkg::ResolveError::NoSpace,
            Status::UNAVAILABLE => pkg::ResolveError::UnavailableBlob,
            Status::INVALID_ARGS => pkg::ResolveError::InvalidUrl,
            Status::INTERNAL => pkg::ResolveError::Internal,
            _ => pkg::ResolveError::Internal,
        }
    }
}

pub(crate) trait ToResolveStatus {
    fn to_resolve_status(&self) -> Status;
}
impl ToResolveStatus for pkg::ResolveError {
    fn to_resolve_status(&self) -> Status {
        use pkg::ResolveError::*;
        match *self {
            AccessDenied => Status::ACCESS_DENIED,
            Io => Status::IO,
            PackageNotFound | RepoNotFound | BlobNotFound => Status::NOT_FOUND,
            NoSpace => Status::NO_SPACE,
            UnavailableBlob | UnavailableRepoMetadata => Status::UNAVAILABLE,
            InvalidUrl | InvalidContext => Status::INVALID_ARGS,
            Internal => Status::INTERNAL,
        }
    }
}

// From resolver.fidl:
// * `ZX_ERR_INTERNAL` if the resolver encountered an otherwise unspecified error
//   while handling the request
// * `ZX_ERR_NOT_FOUND` if the package does not exist.
// * `ZX_ERR_ADDRESS_UNREACHABLE` if the resolver does not know about the repo.
impl ToResolveError for CacheError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        match self {
            CacheError::Fidl(_) => pkg::ResolveError::Io,
            CacheError::MerkleFor(err) => err.to_resolve_error(),
            CacheError::ListNeeds(err) => err.to_resolve_error(),
            CacheError::FetchMetaFar(err, ..) => err.to_resolve_error(),
            CacheError::FetchContentBlob(err, _) => err.to_resolve_error(),
            CacheError::Get(err) => err.to_resolve_error(),
            CacheError::Open(err) => err.to_resolve_error(),
        }
    }
}

impl ToResolveStatus for MerkleForError {
    fn to_resolve_status(&self) -> Status {
        match self {
            MerkleForError::MetadataNotFound { .. } => Status::INTERNAL,
            MerkleForError::TargetNotFound(_) => Status::NOT_FOUND,
            MerkleForError::InvalidTargetPath(_) => Status::INTERNAL,
            // FIXME(42326) when tuf::Error gets an HTTP error variant, this should be mapped to Status::UNAVAILABLE
            MerkleForError::FetchTargetDescription(..) => Status::INTERNAL,
            MerkleForError::NoCustomMetadata => Status::INTERNAL,
            MerkleForError::SerdeError(_) => Status::INTERNAL,
        }
    }
}

impl ToResolveError for MerkleForError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        match self {
            MerkleForError::MetadataNotFound { .. } => pkg::ResolveError::Internal,
            MerkleForError::TargetNotFound(_) => pkg::ResolveError::PackageNotFound,
            MerkleForError::InvalidTargetPath(_) => pkg::ResolveError::Internal,
            // FIXME(42326) when tuf::Error gets an HTTP error variant, this should be mapped to Status::UNAVAILABLE
            MerkleForError::FetchTargetDescription(..) => pkg::ResolveError::Internal,
            MerkleForError::NoCustomMetadata => pkg::ResolveError::Internal,
            MerkleForError::SerdeError(_) => pkg::ResolveError::Internal,
        }
    }
}

impl ToResolveError for pkg::cache::OpenError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        match self {
            pkg::cache::OpenError::NotFound => pkg::ResolveError::PackageNotFound,
            pkg::cache::OpenError::UnexpectedResponse(_) | pkg::cache::OpenError::Fidl(_) => {
                pkg::ResolveError::Internal
            }
        }
    }
}

impl ToResolveError for pkg::cache::GetError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        match self {
            pkg::cache::GetError::UnexpectedResponse(_) => pkg::ResolveError::Internal,
            pkg::cache::GetError::Fidl(_) => pkg::ResolveError::Internal,
        }
    }
}

impl ToResolveError for pkg::cache::OpenBlobError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        match self {
            pkg::cache::OpenBlobError::OutOfSpace => pkg::ResolveError::NoSpace,
            pkg::cache::OpenBlobError::ConcurrentWrite => pkg::ResolveError::Internal,
            pkg::cache::OpenBlobError::UnspecifiedIo => pkg::ResolveError::Io,
            pkg::cache::OpenBlobError::Internal => pkg::ResolveError::Internal,
            pkg::cache::OpenBlobError::Fidl(_) => pkg::ResolveError::Internal,
        }
    }
}

impl ToResolveError for pkg::cache::ListMissingBlobsError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        pkg::ResolveError::Internal
    }
}

impl ToResolveError for pkg::cache::TruncateBlobError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        use pkg::cache::TruncateBlobError::*;
        match self {
            NoSpace => pkg::ResolveError::NoSpace,
            UnexpectedResponse(_) => pkg::ResolveError::Io,
            Fidl(_) => pkg::ResolveError::Io,
            AlreadyTruncated(_) | Other(_) | BadState => pkg::ResolveError::Internal,
        }
    }
}

impl ToResolveError for pkg::cache::WriteBlobError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        use pkg::cache::WriteBlobError::*;
        match self {
            Overwrite | Corrupt | UnexpectedResponse(_) | Fidl(_) => pkg::ResolveError::Io,
            NoSpace => pkg::ResolveError::NoSpace,
            BytesNotNeeded(_) | Other(_) => pkg::ResolveError::Internal,
        }
    }
}

impl ToResolveError for pkg::cache::GetAlreadyCachedError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        if self.was_not_cached() {
            pkg::ResolveError::PackageNotFound
        } else {
            pkg::ResolveError::Internal
        }
    }
}

impl ToResolveError for FetchError {
    fn to_resolve_error(&self) -> pkg::ResolveError {
        use FetchError::*;
        match self {
            CreateBlob(e) => e.to_resolve_error(),
            BadHttpStatus { code: hyper::StatusCode::UNAUTHORIZED, .. } => {
                pkg::ResolveError::AccessDenied
            }
            BadHttpStatus { code: hyper::StatusCode::FORBIDDEN, .. } => {
                pkg::ResolveError::AccessDenied
            }
            BadHttpStatus { .. } => pkg::ResolveError::UnavailableBlob,
            ContentLengthMismatch { .. } => pkg::ResolveError::UnavailableBlob,
            UnknownLength { .. } => pkg::ResolveError::UnavailableBlob,
            BlobTooSmall { .. } => pkg::ResolveError::UnavailableBlob,
            BlobTooLarge { .. } => pkg::ResolveError::UnavailableBlob,
            Hyper { .. } => pkg::ResolveError::UnavailableBlob,
            Http { .. } => pkg::ResolveError::UnavailableBlob,
            Truncate(e) => e.to_resolve_error(),
            Write { e, .. } => e.to_resolve_error(),
            BlobWritten(_) => pkg::ResolveError::Internal,
            NoMirrors => pkg::ResolveError::Internal,
            BlobUrl(_) => pkg::ResolveError::Internal,
            FidlError(_) => pkg::ResolveError::Internal,
            IoError(_) => pkg::ResolveError::Io,
            BlobHeaderTimeout { .. } => pkg::ResolveError::UnavailableBlob,
            BlobBodyTimeout { .. } => pkg::ResolveError::UnavailableBlob,
            ExpectedHttpStatus206 { .. } => pkg::ResolveError::UnavailableBlob,
            MissingContentRangeHeader { .. } => pkg::ResolveError::UnavailableBlob,
            MalformedContentRangeHeader { .. } => pkg::ResolveError::UnavailableBlob,
            InvalidContentRangeHeader { .. } => pkg::ResolveError::UnavailableBlob,
            ExceededResumptionAttemptLimit { .. } => pkg::ResolveError::UnavailableBlob,
            ContentLengthContentRangeMismatch { .. } => pkg::ResolveError::UnavailableBlob,
        }
    }
}

impl From<&MerkleForError> for metrics::MerkleForUrlMigratedMetricDimensionResult {
    fn from(e: &MerkleForError) -> metrics::MerkleForUrlMigratedMetricDimensionResult {
        use metrics::MerkleForUrlMigratedMetricDimensionResult as EventCodes;
        match e {
            MerkleForError::MetadataNotFound { .. } => EventCodes::TufError,
            MerkleForError::TargetNotFound(_) => EventCodes::NotFound,
            MerkleForError::FetchTargetDescription(..) => EventCodes::TufError,
            MerkleForError::InvalidTargetPath(_) => EventCodes::InvalidTargetPath,
            MerkleForError::NoCustomMetadata => EventCodes::NoCustomMetadata,
            MerkleForError::SerdeError(_) => EventCodes::SerdeError,
        }
    }
}

pub async fn merkle_for_url(
    repo: Arc<AsyncMutex<Repository>>,
    url: &AbsolutePackageUrl,
    mut cobalt_sender: ProtocolSender<MetricEvent>,
) -> Result<BlobId, MerkleForError> {
    let target_path = TargetPath::new(format!(
        "{}/{}",
        url.name(),
        url.variant().unwrap_or(&fuchsia_url::PackageVariant::zero())
    ))
    .map_err(MerkleForError::InvalidTargetPath)?;
    let mut repo = repo.lock().await;
    let res = repo.get_merkle_at_path(&target_path).await;
    cobalt_sender.send(
        MetricEvent::builder(metrics::MERKLE_FOR_URL_MIGRATED_METRIC_ID)
            .with_event_codes(match &res {
                Ok(_) => metrics::MerkleForUrlMigratedMetricDimensionResult::Success,
                Err(res) => res.into(),
            })
            .as_occurrence(1),
    );
    res.map(|custom| custom.merkle())
}

#[derive(Debug, thiserror::Error)]
pub enum MerkleForError {
    #[error("the repository metadata {path} at version {version} was not found in the repository")]
    MetadataNotFound { path: MetadataPath, version: MetadataVersion },

    #[error("the package {0} was not found in the repository")]
    TargetNotFound(TargetPath),

    #[error("unexpected tuf error when fetching target description for {0:?}")]
    FetchTargetDescription(String, #[source] tuf::error::Error),

    #[error("the target path is not safe")]
    InvalidTargetPath(#[source] tuf::error::Error),

    #[error("the target description does not have custom metadata")]
    NoCustomMetadata,

    #[error("serde value could not be converted")]
    SerdeError(#[source] serde_json::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub struct FetchBlobContext {
    opener: pkg::cache::DeferredOpenBlob,
    mirrors: Arc<[MirrorConfig]>,
    parent_trace_id: ftrace::Id,
}

impl work_queue::TryMerge for FetchBlobContext {
    fn try_merge(&mut self, other: Self) -> Result<(), Self> {
        // The NeededBlobs protocol requires pkg-resolver to attempt to open each blob associated
        // with a Get() request, and attempting to open a blob being written by another Get()
        // operation would fail. So, this queue is needed to enforce a concurrency limit and ensure
        // a blob is not written by more than one fetch at a time.

        // Only requests that are the same request on the same channel can be merged, and the
        // packageresolver should never enqueue such duplicate requests, so, realistically,
        // try_merge always returns Err(other).
        if self.opener != other.opener {
            return Err(other);
        }

        // Don't attempt to merge mirrors, but do merge these contexts if the mirrors are
        // equivalent.
        if self.mirrors != other.mirrors {
            return Err(other);
        }

        // Contexts are mergeable.
        Ok(())
    }
}

/// A clonable handle to the blob fetch queue.  When all clones of
/// [`BlobFetcher`] are dropped, the queue will fetch all remaining blobs in
/// the queue and terminate its output stream.
#[derive(Clone)]
pub struct BlobFetcher {
    sender: work_queue::WorkSender<BlobId, FetchBlobContext, Result<(), Arc<FetchError>>>,
}

impl BlobFetcher {
    /// Creates an unbounded queue that will fetch up to `max_concurrency` blobs at once.
    /// Returns:
    ///   1. a Future to be awaited that processes the queue
    ///   2. a Self that enables pushing work onto the queue
    pub fn new(
        node: fuchsia_inspect::Node,
        max_concurrency: usize,
        stats: Arc<Mutex<Stats>>,
        cobalt_sender: ProtocolSender<MetricEvent>,
        blob_fetch_params: BlobFetchParams,
    ) -> (impl Future<Output = ()>, Self) {
        let http_client = Arc::new(fuchsia_hyper::new_https_client_from_tcp_options(
            fuchsia_hyper::TcpOptions::keepalive_timeout(TCP_KEEPALIVE_TIMEOUT),
        ));
        let weak_node = node.clone_weak();
        let inspect = inspect::BlobFetcher::from_node_and_params(node, &blob_fetch_params);

        let (queue, sender) = work_queue::work_queue(
            max_concurrency,
            move |merkle: BlobId, context: FetchBlobContext| {
                let inspect = inspect.fetch(&merkle);
                let http_client = Arc::clone(&http_client);
                let stats = Arc::clone(&stats);
                let cobalt_sender = cobalt_sender.clone();

                async move {
                    fetch_blob(
                        inspect,
                        &http_client,
                        stats,
                        cobalt_sender,
                        merkle,
                        context,
                        blob_fetch_params,
                    )
                    .map_err(Arc::new)
                    .await
                }
            },
        );
        weak_node.record_lazy_child("raw_queue", queue.record_lazy_inspect());

        (queue.into_future(), BlobFetcher { sender })
    }

    /// Enqueue the given blob to be fetched, or attach to an existing request to
    /// fetch the blob.
    pub fn push(
        &self,
        blob_id: BlobId,
        context: FetchBlobContext,
    ) -> impl Future<Output = Result<Result<(), Arc<FetchError>>, work_queue::Closed>> {
        self.sender.push(blob_id, context)
    }

    /// Enqueue all the given blobs to be fetched, merging them with existing
    /// known tasks if possible, returning an iterator of the futures that will
    /// resolve to the results.
    ///
    /// This method is similar to, but more efficient than, mapping an iterator
    /// to `BlobFetcher::push`.
    pub fn push_all(
        &self,
        entries: impl Iterator<Item = (BlobId, FetchBlobContext)>,
    ) -> impl Iterator<
        Item = impl Future<Output = Result<Result<(), Arc<FetchError>>, work_queue::Closed>>,
    > {
        self.sender.push_all(entries)
    }
}

async fn fetch_blob(
    inspect: inspect::NeedsRemoteType,
    http_client: &fuchsia_hyper::HttpsClient,
    stats: Arc<Mutex<Stats>>,
    cobalt_sender: ProtocolSender<MetricEvent>,
    merkle: BlobId,
    context: FetchBlobContext,
    blob_fetch_params: BlobFetchParams,
) -> Result<(), FetchError> {
    // TODO try the other mirrors depending on the errors encountered trying this one.
    let mirror = context.mirrors.get(0).ok_or(FetchError::NoMirrors)?;
    let trace_id = ftrace::Id::random();
    let guard = ftrace::async_enter!(
        trace_id,
        c"app",
        c"fetch_blob_http",
        // Async tracing does not support multiple concurrent child durations, so we create
        // a new top-level duration and attach the parent duration as metadata.
        "parent_trace_id" => u64::from(context.parent_trace_id),
        "hash" => merkle.to_string().as_str()
    );
    let inspect = inspect.http();
    let res = fetch_blob_http(
        &inspect,
        http_client,
        &mirror,
        merkle,
        &context.opener,
        blob_fetch_params,
        &stats,
        cobalt_sender,
        trace_id,
    )
    .await;
    guard.map(|o| o.end(&[ftrace::ArgValue::of("result", format!("{res:?}").as_str())]));
    res
}

#[derive(Default)]
struct FetchStats {
    // How many times the blob download was resumed, e.g. with Http Range requests
    resumptions: AtomicU64,
}

impl FetchStats {
    fn resume(&self) {
        self.resumptions.fetch_add(1, Ordering::SeqCst);
    }

    fn resumptions(&self) -> u64 {
        self.resumptions.load(Ordering::SeqCst)
    }
}

async fn fetch_blob_http(
    inspect: &inspect::TriggerAttempt<inspect::Http>,
    client: &fuchsia_hyper::HttpsClient,
    mirror: &MirrorConfig,
    merkle: BlobId,
    opener: &pkg::cache::DeferredOpenBlob,
    blob_fetch_params: BlobFetchParams,
    stats: &Mutex<Stats>,
    cobalt_sender: ProtocolSender<MetricEvent>,
    trace_id: ftrace::Id,
) -> Result<(), FetchError> {
    let blob_mirror_url = mirror.blob_mirror_url().to_owned();
    let mirror_stats = &stats.lock().for_mirror(blob_mirror_url.to_string());
    let blob_url = &make_blob_url(blob_mirror_url, &merkle, blob_fetch_params.blob_type)
        .map_err(FetchError::BlobUrl)?;
    inspect.set_mirror(&blob_url.to_string());
    let flaked = &AtomicBool::new(false);

    fuchsia_backoff::retry_or_first_error(retry::blob_fetch(), || {
        let mut cobalt_sender = cobalt_sender.clone();

        async move {
            let fetch_stats = FetchStats::default();
            let res = async {
                let inspect = inspect.attempt();
                inspect.state(inspect::Http::CreateBlob);
                if let Some(pkg::cache::NeededBlob { blob, closer: blob_closer }) =
                    opener.open().await.map_err(FetchError::CreateBlob)?
                {
                    inspect.state(inspect::Http::DownloadBlob);
                    let guard = ftrace::async_enter!(
                        trace_id,
                        c"app",
                        c"download_blob",
                        "hash" => merkle.to_string().as_str()
                    );
                    let res = download_blob(
                        &inspect,
                        client,
                        blob_url,
                        blob,
                        blob_fetch_params,
                        &fetch_stats,
                        trace_id,
                    )
                    .await;
                    let (size_str, status_str) = match &res {
                        Ok(size) => (size.to_string(), "success".to_string()),
                        Err(e) => ("no size because download failed".to_string(), e.to_string()),
                    };
                    guard.map(|o| {
                        o.end(&[
                            ftrace::ArgValue::of("size", size_str.as_str()),
                            ftrace::ArgValue::of("status", status_str.as_str()),
                        ])
                    });
                    inspect.state(inspect::Http::CloseBlob);
                    blob_closer.close().await;
                    res?;
                }
                Ok(())
            }
            .await;

            match res.as_ref().map_err(FetchError::kind) {
                Err(FetchErrorKind::NetworkRateLimit) => {
                    mirror_stats.network_rate_limits().increment();
                }
                Err(FetchErrorKind::Network) => {
                    flaked.store(true, Ordering::SeqCst);
                }
                Err(FetchErrorKind::NotFound | FetchErrorKind::Other) => {}
                Ok(()) => {
                    if flaked.load(Ordering::SeqCst) {
                        mirror_stats.network_blips().increment();
                    }
                }
            }

            let result_event_code = match &res {
                Ok(()) => metrics::FetchBlobMigratedMetricDimensionResult::Success,
                Err(e) => e.into(),
            };
            let resumed_event_code = if fetch_stats.resumptions() != 0 {
                metrics::FetchBlobMigratedMetricDimensionResumed::True
            } else {
                metrics::FetchBlobMigratedMetricDimensionResumed::False
            };
            cobalt_sender.send(
                MetricEvent::builder(metrics::FETCH_BLOB_MIGRATED_METRIC_ID)
                    .with_event_codes((result_event_code, resumed_event_code))
                    .as_occurrence(1),
            );

            res
        }
    })
    .await
}

fn make_blob_url(
    blob_mirror_url: http::Uri,
    merkle: &BlobId,
    blob_type: DeliveryBlobType,
) -> Result<hyper::Uri, http_uri_ext::Error> {
    blob_mirror_url.extend_dir_with_path(&format!("{}/{merkle}", u32::from(blob_type)))
}

// On success, returns the size of the downloaded blob in bytes (useful for tracing).
async fn download_blob(
    inspect: &inspect::Attempt<inspect::Http>,
    client: &fuchsia_hyper::HttpsClient,
    uri: &http::Uri,
    dest: pkg::cache::Blob<pkg::cache::NeedsTruncate>,
    blob_fetch_params: BlobFetchParams,
    fetch_stats: &FetchStats,
    trace_id: ftrace::Id,
) -> Result<u64, FetchError> {
    inspect.state(inspect::Http::HttpGet);
    let (expected_len, content) =
        resume::resuming_get(client, uri, blob_fetch_params, fetch_stats).await?;
    ftrace::async_instant!(trace_id, c"app", c"header_received");

    inspect.expected_size_bytes(expected_len);

    inspect.state(inspect::Http::TruncateBlob);
    let mut written = 0u64;
    let dest = match dest.truncate(expected_len).await.map_err(FetchError::Truncate)? {
        pkg::cache::TruncateBlobSuccess::AllWritten(dest) => dest,
        pkg::cache::TruncateBlobSuccess::NeedsData(mut dest) => {
            futures::pin_mut!(content);
            loop {
                inspect.state(inspect::Http::ReadHttpBody);
                let Some(chunk) = content.try_next().await? else {
                    return Err(FetchError::BlobTooSmall { uri: uri.to_string() });
                };

                if written + chunk.len() as u64 > expected_len {
                    return Err(FetchError::BlobTooLarge { uri: uri.to_string() });
                }
                ftrace::async_instant!(
                    trace_id,
                    c"app",
                    c"read_chunk_from_hyper",
                    "size" => chunk.len() as u64
                );

                inspect.state(inspect::Http::WriteBlob);
                dest = match dest
                    .write_with_trace_callbacks(
                        &chunk,
                        &|size: u64| {
                            ftrace::async_begin(
                                trace_id,
                                c"app",
                                c"waiting_for_pkg_cache_to_ack_write",
                                &[ftrace::ArgValue::of("size", size)],
                            )
                        },
                        &|| {
                            ftrace::async_end(
                                trace_id,
                                c"app",
                                c"waiting_for_pkg_cache_to_ack_write",
                                &[],
                            )
                        },
                    )
                    .await
                    .map_err(|e| FetchError::Write { e, uri: uri.to_string() })?
                {
                    pkg::cache::BlobWriteSuccess::NeedsData(blob) => {
                        written += chunk.len() as u64;
                        blob
                    }
                    pkg::cache::BlobWriteSuccess::AllWritten(blob) => {
                        written += chunk.len() as u64;
                        break blob;
                    }
                };
                inspect.write_bytes(chunk.len());
            }
        }
    };
    inspect.state(inspect::Http::WriteComplete);
    if expected_len != written {
        return Err(FetchError::BlobTooSmall { uri: uri.to_string() });
    }

    let () = dest.blob_written().await.map_err(FetchError::BlobWritten)?;

    Ok(expected_len)
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("could not create blob")]
    CreateBlob(#[source] pkg::cache::OpenBlobError),

    #[error("Fetch of type {blob_type:?} delivery blob at {uri} failed: http request expected 200, got {code}")]
    BadHttpStatus { code: hyper::StatusCode, uri: String, blob_type: DeliveryBlobType },

    #[error("repository has no configured mirrors")]
    NoMirrors,

    #[error("Blob fetch of {uri}: expected blob length of {expected}, got {actual}")]
    ContentLengthMismatch { expected: u64, actual: u64, uri: String },

    #[error("Blob fetch of {uri}: blob length not known or provided by server")]
    UnknownLength { uri: String },

    #[error("Blob fetch of {uri}: downloaded blob was too small")]
    BlobTooSmall { uri: String },

    #[error("Blob fetch of {uri}: downloaded blob was too large")]
    BlobTooLarge { uri: String },

    #[error("failed to truncate blob")]
    Truncate(#[source] pkg::cache::TruncateBlobError),

    #[error("Blob fetch of {uri}: failed to write blob data")]
    Write {
        #[source]
        e: pkg::cache::WriteBlobError,
        uri: String,
    },

    #[error("error when telling pkg-cache the blob has been written")]
    BlobWritten(#[source] pkg::cache::BlobWrittenError),

    #[error("hyper error while fetching {uri}")]
    Hyper {
        #[source]
        e: hyper::Error,
        uri: String,
    },

    #[error("http error while fetching {uri}")]
    Http {
        #[source]
        e: hyper::http::Error,
        uri: String,
    },

    #[error("blob url error")]
    BlobUrl(#[source] http_uri_ext::Error),

    #[error("FIDL error while fetching blob")]
    FidlError(#[source] fidl::Error),

    #[error("IO error while reading blob")]
    IoError(#[source] Status),

    #[error(
        // LINT.IfChange(blob_header_timeout)
        "timed out waiting for http response header while downloading blob: {uri}"
        // LINT.ThenChange(/tools/testing/tefmocheck/string_in_log_check.go:blob_header_timeout)
    )]
    BlobHeaderTimeout { uri: String },

    #[error(
        "timed out waiting for bytes from the http response body while downloading blob: {uri}"
    )]
    BlobBodyTimeout { uri: String },

    #[error(
        "Blob fetch of {uri}: http request for range {first_byte_pos}-{last_byte_pos} expected 206, got {code}, \
        headers {response_headers:?}"
    )]
    ExpectedHttpStatus206 {
        code: hyper::StatusCode,
        uri: String,
        first_byte_pos: u64,
        last_byte_pos: u64,
        response_headers: http::header::HeaderMap,
    },

    #[error("Blob fetch of {uri}: http request expected Content-Range header")]
    MissingContentRangeHeader { uri: String },

    #[error("Blob fetch of {uri}: http request for range {first_byte_pos}-{last_byte_pos} returned malformed Content-Range header: {header:?}")]
    MalformedContentRangeHeader {
        #[source]
        e: resume::ContentRangeParseError,
        uri: String,
        first_byte_pos: u64,
        last_byte_pos: u64,
        header: http::header::HeaderValue,
    },

    #[error("Blob fetch of {uri}: http request returned Content-Range: {content_range:?} but expected: {expected:?}")]
    InvalidContentRangeHeader {
        uri: String,
        content_range: resume::HttpContentRange,
        expected: resume::HttpContentRange,
    },

    #[error("Blob fetch of {uri}: exceeded resumption attempt limit of: {limit}")]
    ExceededResumptionAttemptLimit { uri: String, limit: u64 },

    #[error("Blob fetch of {uri}: Content-Length: {content_length} and Content-Range: {content_range:?} are inconsistent")]
    ContentLengthContentRangeMismatch {
        uri: String,
        content_length: u64,
        content_range: resume::HttpContentRange,
    },
}

impl From<&FetchError> for metrics::FetchBlobMigratedMetricDimensionResult {
    fn from(error: &FetchError) -> Self {
        use {metrics::FetchBlobMigratedMetricDimensionResult as EventCodes, FetchError::*};
        match error {
            CreateBlob { .. } => EventCodes::CreateBlob,
            BadHttpStatus { code, .. } => match *code {
                StatusCode::BAD_REQUEST => EventCodes::HttpBadRequest,
                StatusCode::UNAUTHORIZED => EventCodes::HttpUnauthorized,
                StatusCode::FORBIDDEN => EventCodes::HttpForbidden,
                StatusCode::NOT_FOUND => EventCodes::HttpNotFound,
                StatusCode::METHOD_NOT_ALLOWED => EventCodes::HttpMethodNotAllowed,
                StatusCode::REQUEST_TIMEOUT => EventCodes::HttpRequestTimeout,
                StatusCode::PRECONDITION_FAILED => EventCodes::HttpPreconditionFailed,
                StatusCode::RANGE_NOT_SATISFIABLE => EventCodes::HttpRangeNotSatisfiable,
                StatusCode::TOO_MANY_REQUESTS => EventCodes::HttpTooManyRequests,
                StatusCode::INTERNAL_SERVER_ERROR => EventCodes::HttpInternalServerError,
                StatusCode::BAD_GATEWAY => EventCodes::HttpBadGateway,
                StatusCode::SERVICE_UNAVAILABLE => EventCodes::HttpServiceUnavailable,
                StatusCode::GATEWAY_TIMEOUT => EventCodes::HttpGatewayTimeout,
                _ => match code.as_u16() {
                    100..=199 => EventCodes::Http1xx,
                    200..=299 => EventCodes::Http2xx,
                    300..=399 => EventCodes::Http3xx,
                    400..=499 => EventCodes::Http4xx,
                    500..=599 => EventCodes::Http5xx,
                    _ => EventCodes::BadHttpStatus,
                },
            },
            NoMirrors => EventCodes::NoMirrors,
            ContentLengthMismatch { .. } => EventCodes::ContentLengthMismatch,
            UnknownLength { .. } => EventCodes::UnknownLength,
            BlobTooSmall { .. } => EventCodes::BlobTooSmall,
            BlobTooLarge { .. } => EventCodes::BlobTooLarge,
            Truncate { .. } => EventCodes::Truncate,
            Write { .. } => EventCodes::Write,
            BlobWritten { .. } => EventCodes::BlobWritten,
            Hyper { .. } => EventCodes::Hyper,
            Http { .. } => EventCodes::Http,
            BlobUrl { .. } => EventCodes::BlobUrl,
            FidlError { .. } => EventCodes::FidlError,
            IoError { .. } => EventCodes::IoError,
            BlobHeaderTimeout { .. } => EventCodes::BlobHeaderDeadlineExceeded,
            BlobBodyTimeout { .. } => EventCodes::BlobBodyDeadlineExceeded,
            ExpectedHttpStatus206 { .. } => EventCodes::ExpectedHttpStatus206,
            MissingContentRangeHeader { .. } => EventCodes::MissingContentRangeHeader,
            MalformedContentRangeHeader { .. } => EventCodes::MalformedContentRangeHeader,
            InvalidContentRangeHeader { .. } => EventCodes::InvalidContentRangeHeader,
            ExceededResumptionAttemptLimit { .. } => EventCodes::ExceededResumptionAttemptLimit,
            ContentLengthContentRangeMismatch { .. } => {
                EventCodes::ContentLengthContentRangeMismatch
            }
        }
    }
}

impl FetchError {
    fn kind(&self) -> FetchErrorKind {
        use FetchError::*;
        match self {
            BadHttpStatus { code: StatusCode::TOO_MANY_REQUESTS, uri: _, blob_type: _ } => {
                FetchErrorKind::NetworkRateLimit
            }
            BadHttpStatus { code: StatusCode::NOT_FOUND, uri: _, blob_type: _ } => {
                FetchErrorKind::NotFound
            }
            Hyper { .. }
            | Http { .. }
            | BadHttpStatus { .. }
            | BlobHeaderTimeout { .. }
            | BlobBodyTimeout { .. }
            | ExpectedHttpStatus206 { .. }
            | MissingContentRangeHeader { .. }
            | MalformedContentRangeHeader { .. }
            | InvalidContentRangeHeader { .. }
            | ExceededResumptionAttemptLimit { .. }
            | ContentLengthContentRangeMismatch { .. } => FetchErrorKind::Network,
            CreateBlob { .. }
            | NoMirrors
            | ContentLengthMismatch { .. }
            | UnknownLength { .. }
            | BlobTooSmall { .. }
            | BlobTooLarge { .. }
            | Truncate { .. }
            | Write { .. }
            | BlobWritten { .. }
            | BlobUrl { .. }
            | FidlError { .. }
            | IoError { .. } => FetchErrorKind::Other,
        }
    }

    fn to_pkg_cache_fidl_error(&self) -> Option<&fidl::Error> {
        use FetchError::*;
        match self {
            CreateBlob(pkg::cache::OpenBlobError::Fidl(e))
            | Truncate(pkg::cache::TruncateBlobError::Fidl(e))
            | Write { e: pkg::cache::WriteBlobError::Fidl(e), .. }
            | BlobWritten(pkg::cache::BlobWrittenError::Fidl(e)) => Some(e),
            CreateBlob(_)
            | Truncate(_)
            | Write { .. }
            | BlobWritten(_)
            | FidlError { .. }
            | Hyper { .. }
            | Http { .. }
            | BadHttpStatus { .. }
            | BlobHeaderTimeout { .. }
            | BlobBodyTimeout { .. }
            | ExpectedHttpStatus206 { .. }
            | MissingContentRangeHeader { .. }
            | MalformedContentRangeHeader { .. }
            | InvalidContentRangeHeader { .. }
            | ExceededResumptionAttemptLimit { .. }
            | ContentLengthContentRangeMismatch { .. }
            | NoMirrors
            | ContentLengthMismatch { .. }
            | UnknownLength { .. }
            | BlobTooSmall { .. }
            | BlobTooLarge { .. }
            | BlobUrl { .. }
            | IoError { .. } => None,
        }
    }

    fn is_unexpected_pkg_cache_closed(&self) -> bool {
        matches!(self.to_pkg_cache_fidl_error(), Some(fidl::Error::ClientChannelClosed { .. }))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FetchErrorKind {
    NetworkRateLimit,
    Network,
    NotFound,
    Other,
}

#[cfg(test)]
mod tests {
    use {super::*, http::Uri};

    #[test]
    fn test_make_blob_url() {
        let merkle = "00112233445566778899aabbccddeeffffeeddccbbaa99887766554433221100"
            .parse::<BlobId>()
            .unwrap();

        assert_eq!(
            make_blob_url(
                "http://example.com".parse::<Uri>().unwrap(),
                &merkle,
                DeliveryBlobType::Type1
            )
            .unwrap(),
            format!("http://example.com/1/{merkle}").parse::<Uri>().unwrap()
        );

        assert_eq!(
            make_blob_url(
                "http://example.com/noslash".parse::<Uri>().unwrap(),
                &merkle,
                DeliveryBlobType::Type1
            )
            .unwrap(),
            format!("http://example.com/noslash/1/{merkle}").parse::<Uri>().unwrap()
        );

        assert_eq!(
            make_blob_url(
                "http://example.com/slash/".parse::<Uri>().unwrap(),
                &merkle,
                DeliveryBlobType::Type1
            )
            .unwrap(),
            format!("http://example.com/slash/1/{merkle}").parse::<Uri>().unwrap()
        );

        assert_eq!(
            make_blob_url(
                "http://example.com/twoslashes//".parse::<Uri>().unwrap(),
                &merkle,
                DeliveryBlobType::Type1
            )
            .unwrap(),
            format!("http://example.com/twoslashes//1/{merkle}").parse::<Uri>().unwrap()
        );

        // IPv6 zone id
        assert_eq!(
            make_blob_url(
                "http://[fe80::e022:d4ff:fe13:8ec3%252]:8083/blobs/".parse::<Uri>().unwrap(),
                &merkle,
                DeliveryBlobType::Type1
            )
            .unwrap(),
            format!("http://[fe80::e022:d4ff:fe13:8ec3%252]:8083/blobs/1/{merkle}")
                .parse::<Uri>()
                .unwrap()
        );
    }
}
