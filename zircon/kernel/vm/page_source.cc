// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/console.h>
#include <trace.h>

#include <fbl/auto_lock.h>
#include <kernel/lockdep.h>
#include <ktl/move.h>
#include <vm/page_source.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

PageSource::PageSource(fbl::RefPtr<PageProvider>&& page_provider)
    : page_provider_properties_(page_provider->properties()),
      page_provider_(ktl::move(page_provider)) {
  LTRACEF("%p\n", this);
}

PageSource::~PageSource() {
  LTRACEF("%p\n", this);
  DEBUG_ASSERT(detached_);
  DEBUG_ASSERT(closed_);
}

void PageSource::Detach() {
  canary_.Assert();
  LTRACEF("%p\n", this);
  Guard<Mutex> guard{&page_source_mtx_};
  if (detached_) {
    return;
  }

  detached_ = true;

  // Cancel all requests except writebacks, which can be completed after detach.
  for (uint8_t type = 0; type < page_request_type::COUNT; type++) {
    if (type == page_request_type::WRITEBACK ||
        !page_provider_->SupportsPageRequestType(page_request_type(type))) {
      continue;
    }
    while (!outstanding_requests_[type].is_empty()) {
      auto req = outstanding_requests_[type].pop_front();
      LTRACEF("dropping request with offset %lx len %lx\n", req->offset_, req->len_);

      // Tell the clients the request is complete - they'll fail when they
      // reattempt the page request for the same pages after failing this time.
      CompleteRequestLocked(req);
    }
  }

  // No writebacks supported yet.
  DEBUG_ASSERT(outstanding_requests_[page_request_type::WRITEBACK].is_empty());

  page_provider_->OnDetach();
}

void PageSource::Close() {
  canary_.Assert();
  LTRACEF("%p\n", this);
  // TODO: Close will have more meaning once writeback is implemented

  // This will be a no-op if the page source has already been detached.
  Detach();

  Guard<Mutex> guard{&page_source_mtx_};
  if (closed_) {
    return;
  }

  closed_ = true;
  page_provider_->OnClose();
}

void PageSource::OnPagesSupplied(uint64_t offset, uint64_t len) {
  ResolveRequests(page_request_type::READ, offset, len);
}

void PageSource::OnPagesDirtied(uint64_t offset, uint64_t len) {
  ResolveRequests(page_request_type::DIRTY, offset, len);
}

void PageSource::ResolveRequests(page_request_type type, uint64_t offset, uint64_t len) {
  canary_.Assert();
  LTRACEF_LEVEL(2, "%p offset %lx, len %lx\n", this, offset, len);
  uint64_t end;
  bool overflow = add_overflow(offset, len, &end);
  DEBUG_ASSERT(!overflow);  // vmobject should have already validated overflow
  DEBUG_ASSERT(type < page_request_type::COUNT);

  Guard<Mutex> guard{&page_source_mtx_};
  if (detached_) {
    return;
  }

  // The first possible request we could fulfill is the one with the smallest
  // end address that is greater than offset. Then keep looking as long as the
  // target request's start offset is less than the end.
  auto start = outstanding_requests_[type].upper_bound(offset);
  while (start.IsValid() && start->offset_ < end) {
    auto cur = start;
    ++start;

    // Calculate how many pages were resolved in this request by finding the start and
    // end offsets of the operation in this request.
    uint64_t req_offset, req_end;
    if (offset >= cur->offset_) {
      // The operation started partway into this request.
      req_offset = offset - cur->offset_;
    } else {
      // The operation started before this request.
      req_offset = 0;
    }
    if (end < cur->GetEnd()) {
      // The operation ended partway into this request.
      req_end = end - cur->offset_;

      uint64_t unused;
      DEBUG_ASSERT(!sub_overflow(end, cur->offset_, &unused));
    } else {
      // The operation ended past the end of this request.
      req_end = cur->len_;
    }

    DEBUG_ASSERT(req_end >= req_offset);
    uint64_t fulfill = req_end - req_offset;

    // If we're not done, continue to the next request.
    if (fulfill < cur->pending_size_) {
      cur->pending_size_ -= fulfill;
      continue;
    } else if (fulfill > cur->pending_size_) {
      // This just means that part of the request was decommitted. That's not
      // an error, but it's good to know when we're tracing.
      LTRACEF("%p, excessive page count\n", this);
    }

    LTRACEF_LEVEL(2, "%p, signaling %lx\n", this, cur->offset_);

    // Notify anything waiting on this range.
    CompleteRequestLocked(outstanding_requests_[type].erase(cur));
  }
}

void PageSource::OnPagesFailed(uint64_t offset, uint64_t len, zx_status_t error_status) {
  canary_.Assert();
  LTRACEF_LEVEL(2, "%p offset %lx, len %lx\n", this, offset, len);

  DEBUG_ASSERT(PageSource::IsValidInternalFailureCode(error_status));

  uint64_t end;
  bool overflow = add_overflow(offset, len, &end);
  DEBUG_ASSERT(!overflow);  // vmobject should have already validated overflow

  Guard<Mutex> guard{&page_source_mtx_};
  if (detached_) {
    return;
  }

  for (uint8_t type = 0; type < page_request_type::COUNT; type++) {
    if (!page_provider_->SupportsPageRequestType(page_request_type(type))) {
      continue;
    }
    // The first possible request we could fail is the one with the smallest
    // end address that is greater than offset. Then keep looking as long as the
    // target request's start offset is less than the supply end.
    auto start = outstanding_requests_[type].upper_bound(offset);
    while (start.IsValid() && start->offset_ < end) {
      auto cur = start;
      ++start;

      LTRACEF_LEVEL(2, "%p, signaling failure %d %lx\n", this, error_status, cur->offset_);

      // Notify anything waiting on this range.
      CompleteRequestLocked(outstanding_requests_[type].erase(cur), error_status);
    }
  }
}

// static
bool PageSource::IsValidExternalFailureCode(zx_status_t error_status) {
  switch (error_status) {
    case ZX_ERR_IO:
    case ZX_ERR_IO_DATA_INTEGRITY:
    case ZX_ERR_BAD_STATE:
    case ZX_ERR_NO_SPACE:
    case ZX_ERR_BUFFER_TOO_SMALL:
      return true;
    default:
      return false;
  }
}

// static
bool PageSource::IsValidInternalFailureCode(zx_status_t error_status) {
  switch (error_status) {
    case ZX_ERR_NO_MEMORY:
      return true;
    default:
      return IsValidExternalFailureCode(error_status);
  }
}

zx_status_t PageSource::GetPages(uint64_t offset, uint64_t len, PageRequest* request,
                                 VmoDebugInfo vmo_debug_info) {
  canary_.Assert();
  DEBUG_ASSERT(len > 0);
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len));

  if (!page_provider_->SupportsPageRequestType(page_request_type::READ)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ASSERT(request);
  offset = fbl::round_down(offset, static_cast<uint64_t>(PAGE_SIZE));

  LTRACEF_LEVEL(2, "%p offset %" PRIx64 " prefetch_len %" PRIx64, this, offset, len);

  Guard<Mutex> guard{&page_source_mtx_};
  if (detached_) {
    return ZX_ERR_BAD_STATE;
  }

  DEBUG_ASSERT(!request->IsInitialized());

  request->Init(fbl::RefPtr<PageRequestInterface>(this), offset, page_request_type::READ,
                vmo_debug_info);

  return PopulateRequestLocked(request, offset, len, page_request_type::READ);
}

void PageSource::FreePages(list_node* pages) { page_provider_->FreePages(pages); }

zx_status_t PageSource::PopulateRequestLocked(PageRequest* request, uint64_t offset, uint64_t len,
                                              page_request_type type) {
  ASSERT(request);
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(len > 0);
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len));
  DEBUG_ASSERT(type < page_request_type::COUNT);
  DEBUG_ASSERT(request->type_ == type);
  DEBUG_ASSERT(request->IsInitialized());

  DEBUG_ASSERT(!request->provider_owned_);
  DEBUG_ASSERT(request->offset_ == offset);
  DEBUG_ASSERT(request->len_ == 0);

  // Assert on overflow, since it means vmobject is trying to get out-of-bounds pages.
  [[maybe_unused]] bool overflowed = add_overflow(request->len_, len, &request->len_);
  DEBUG_ASSERT(!overflowed);
  DEBUG_ASSERT(request->len_ >= PAGE_SIZE);

  uint64_t cur_end;
  overflowed = add_overflow(request->offset_, request->len_, &cur_end);
  DEBUG_ASSERT(!overflowed);

  auto node = outstanding_requests_[request->type_].upper_bound(request->offset_);
  if (node.IsValid()) {
    if (request->offset_ >= node->offset_ && cur_end >= node->GetEnd()) {
      // If the beginning part of this request is covered by an existing request, end the request
      // at the existing request's end and wait for that request to be resolved first.
      request->len_ = node->GetEnd() - request->offset_;
    } else if (request->offset_ < node->offset_ && cur_end >= node->offset_) {
      // If offset is less than node->GetOffset(), then we end the request when we'd start
      // overlapping.
      request->len_ = node->offset_ - request->offset_;
    }
  }

  SendRequestToProviderLocked(request);

  return ZX_ERR_SHOULD_WAIT;
}

bool PageSource::DebugIsPageOk(vm_page_t* page, uint64_t offset) {
  return page_provider_->DebugIsPageOk(page, offset);
}

void PageSource::SendRequestToProviderLocked(PageRequest* request) {
  LTRACEF_LEVEL(2, "%p %p\n", this, request);
  DEBUG_ASSERT(request->type_ < page_request_type::COUNT);
  DEBUG_ASSERT(request->IsInitialized());
  DEBUG_ASSERT(page_provider_->SupportsPageRequestType(request->type_));
  // Find the node with the smallest endpoint greater than offset and then
  // check to see if offset falls within that node.
  auto overlap = outstanding_requests_[request->type_].upper_bound(request->offset_);
  if (overlap.IsValid() && overlap->offset_ <= request->offset_) {
    // GetPage guarantees that if offset lies in an existing node, then it is
    // completely contained in that node.
    overlap->overlap_.push_back(request);
  } else {
    DEBUG_ASSERT(!request->provider_owned_);
    request->pending_size_ = request->len_;

    DEBUG_ASSERT(!fbl::InContainer<PageProviderTag>(*request));
    request->provider_owned_ = true;
    page_provider_->SendAsyncRequest(request);
    outstanding_requests_[request->type_].insert(request);
  }
}

void PageSource::CompleteRequestLocked(PageRequest* request, zx_status_t status) {
  VM_KTRACE_DURATION(1, "page_request_complete", ("offset", request->offset_),
                     ("len", request->len_));
  DEBUG_ASSERT(request->type_ < page_request_type::COUNT);
  DEBUG_ASSERT(page_provider_->SupportsPageRequestType(request->type_));

  // Take the request back from the provider before waking up the corresponding thread. Once the
  // request has been taken back we are also free to modify offset_.
  page_provider_->ClearAsyncRequest(request);
  request->provider_owned_ = false;

  while (!request->overlap_.is_empty()) {
    auto waiter = request->overlap_.pop_front();
    VM_KTRACE_FLOW_BEGIN(1, "page_request_signal", reinterpret_cast<uintptr_t>(waiter));
    DEBUG_ASSERT(!waiter->provider_owned_);
    waiter->offset_ = UINT64_MAX;
    waiter->event_.Signal(status);
  }
  VM_KTRACE_FLOW_BEGIN(1, "page_request_signal", reinterpret_cast<uintptr_t>(request));
  request->offset_ = UINT64_MAX;
  request->event_.Signal(status);
}

void PageSource::CancelRequest(PageRequest* request) {
  canary_.Assert();
  Guard<Mutex> guard{&page_source_mtx_};
  LTRACEF("%p %lx\n", this, request->offset_);

  if (!request->IsInitialized()) {
    return;
  }
  DEBUG_ASSERT(request->type_ < page_request_type::COUNT);
  DEBUG_ASSERT(page_provider_->SupportsPageRequestType(request->type_));

  if (fbl::InContainer<PageSourceTag>(*request)) {
    LTRACEF("Overlap node\n");
    // This node is overlapping some other node, so just remove the request
    auto main_node = outstanding_requests_[request->type_].upper_bound(request->offset_);
    ASSERT(main_node.IsValid());
    main_node->overlap_.erase(*request);
  } else if (!request->overlap_.is_empty()) {
    LTRACEF("Outstanding with overlap\n");
    // This node is an outstanding request with overlap, so replace it with the
    // first overlap node.
    auto new_node = request->overlap_.pop_front();
    DEBUG_ASSERT(!new_node->provider_owned_);
    new_node->overlap_.swap(request->overlap_);
    new_node->offset_ = request->offset_;
    new_node->len_ = request->len_;
    new_node->pending_size_ = request->pending_size_;
    DEBUG_ASSERT(new_node->type_ == request->type_);

    DEBUG_ASSERT(!fbl::InContainer<PageProviderTag>(*new_node));
    outstanding_requests_[request->type_].erase(*request);
    outstanding_requests_[request->type_].insert(new_node);

    new_node->provider_owned_ = true;
    page_provider_->SwapAsyncRequest(request, new_node);
    request->provider_owned_ = false;
  } else if (static_cast<fbl::WAVLTreeContainable<PageRequest*>*>(request)->InContainer()) {
    LTRACEF("Outstanding no overlap\n");
    // This node is an outstanding request with no overlap
    outstanding_requests_[request->type_].erase(*request);
    page_provider_->ClearAsyncRequest(request);
    request->provider_owned_ = false;
  }

  // Request has been cleared from the PageProvider, so we're free to modify the offset_
  request->offset_ = UINT64_MAX;
}

zx_status_t PageSource::WaitOnRequest(PageRequest* request) {
  canary_.Assert();

  // If we have been detached the request will already have been completed in ::Detach and so the
  // provider should instantly wake from the event.
  return page_provider_->WaitOnEvent(&request->event_);
}

zx_status_t PageSource::RequestDirtyTransition(PageRequest* request, uint64_t offset, uint64_t len,
                                               VmoDebugInfo vmo_debug_info) {
  canary_.Assert();
  ASSERT(request);

  if (!page_provider_->SupportsPageRequestType(page_request_type::DIRTY)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  uint64_t end;
  bool overflow = add_overflow(offset, len, &end);
  DEBUG_ASSERT(!overflow);
  offset = fbl::round_down(offset, static_cast<uint64_t>(PAGE_SIZE));
  end = fbl::round_up(end, static_cast<uint64_t>(PAGE_SIZE));

  Guard<Mutex> guard{&page_source_mtx_};
  if (detached_) {
    return ZX_ERR_BAD_STATE;
  }

  // Request should not be previously initialized.
  DEBUG_ASSERT(!request->IsInitialized());
  request->Init(fbl::RefPtr<PageRequestInterface>(this), offset, page_request_type::DIRTY,
                vmo_debug_info);

  return PopulateRequestLocked(request, offset, end - offset, page_request_type::DIRTY);
}

void PageSource::Dump(uint depth) const {
  Guard<Mutex> guard{&page_source_mtx_};
  for (uint i = 0; i < depth; ++i) {
    printf("  ");
  }
  printf("page_source %p detached %d closed %d\n", this, detached_, closed_);
  for (uint8_t type = 0; type < page_request_type::COUNT; type++) {
    for (auto& req : outstanding_requests_[type]) {
      for (uint i = 0; i < depth; ++i) {
        printf("  ");
      }
      printf("  vmo 0x%lx/k%lu %s req [0x%lx, 0x%lx) pending 0x%lx overlap %lu %s\n",
             req.vmo_debug_info_.vmo_ptr, req.vmo_debug_info_.vmo_id,
             PageRequestTypeToString(page_request_type(type)), req.offset_, req.GetEnd(),
             req.pending_size_, req.overlap_.size_slow(), req.provider_owned_ ? "[sent]" : "");
    }
  }
  page_provider_->Dump(depth);
}

PageRequest::~PageRequest() { CancelRequest(); }

void PageRequest::Init(fbl::RefPtr<PageRequestInterface> src, uint64_t offset,
                       page_request_type type, VmoDebugInfo vmo_debug_info) {
  DEBUG_ASSERT(!IsInitialized());
  vmo_debug_info_ = vmo_debug_info;
  len_ = 0;
  offset_ = offset;
  DEBUG_ASSERT(type < page_request_type::COUNT);
  type_ = type;
  src_ = ktl::move(src);

  event_.Unsignal();
}

zx_status_t PageRequest::Wait() {
  lockdep::AssertNoLocksHeld();
  VM_KTRACE_DURATION(1, "page_request_wait", ("offset", offset_), ("len", len_));
  zx_status_t status = src_->WaitOnRequest(this);
  VM_KTRACE_FLOW_END(1, "page_request_signal", reinterpret_cast<uintptr_t>(this));
  if (status != ZX_OK && !PageSource::IsValidInternalFailureCode(status)) {
    src_->CancelRequest(this);
  }
  return status;
}

void PageRequest::CancelRequest() {
  // Nothing to cancel if the request isn't initialized yet.
  if (!IsInitialized()) {
    return;
  }
  DEBUG_ASSERT(src_);
  src_->CancelRequest(this);
  DEBUG_ASSERT(!IsInitialized());
}

PageRequest* LazyPageRequest::get() {
  if (!request_.has_value()) {
    request_.emplace();
  }
  return &*request_;
}

static int cmd_page_source(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments\n");
  usage:
    printf("usage:\n");
    printf("%s dump <address>\n", argv[0].str);
    return ZX_ERR_INTERNAL;
  }

  if (!strcmp(argv[1].str, "dump")) {
    if (argc < 3) {
      goto notenoughargs;
    }
    reinterpret_cast<PageSource*>(argv[2].u)->Dump(0);
  } else {
    printf("unknown command\n");
    goto usage;
  }

  return ZX_OK;
}

STATIC_COMMAND_START
STATIC_COMMAND("vm_page_source", "page source debug commands", &cmd_page_source)
STATIC_COMMAND_END(ps_object)
