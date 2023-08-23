// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_LOADINFO_MUTABLE_MEMORY_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_LOADINFO_MUTABLE_MEMORY_H_

#include <lib/fit/result.h>

#include <optional>
#include <vector>

#include "container.h"
#include "diagnostics.h"

namespace elfldltl {

// elfldltl::LoadInfoMutableMemory is an adapter providing mutation methods of
// the Memory API (see Store and StoreAdd <lib/elfldltl/memory.h>) representing
// a module image described by elfldltl::LoadInfo (see <lib/elfldltl/load.h>).
// This Memory API object can be used to apply relocations via the interfaces
// in <lib/elfldltl/link.h>.
//
// The constructor requires Diagnostics and LoadInfo objects by reference, and
// some callable object by value.  The callable object is used as
// `fit::result<bool, T>(Diagnostics&,LoadInfo::Segment&)`.  In the error case,
// the `bool` value is as would be returned by `Diagnostics::FormatError`.  In
// the success case, the T is any movable object that provides the mutation
// methods of the Memory API.  This Memory object represents just the segment
// and should accept addresses from `segment.vaddr()` for `segment.filesz()`
// bytes, but need not accept any other addresses.  Each segment's object is
// only requested once and then is used for the lifetime of the adapter.
//
// The callback takes the `LoadInfo::Segment` object by mutable reference so it
// can update it to store mutation state when using a segment wrapper subclass.
// That state stored in the LoadInfo segments can be used to map the mutated
// data into a process later.
//
// The final optional template parameter provides a container template to be
// used for the internal storage.  To specify that, the Diagnostics, LoadInfo,
// and GetMutableMemory callable types must be specified explicitly too.  With
// the default container (std::vector) a deduction guide allows construction
// with no template parameters.

template <class Diagnostics, class LoadInfo, typename GetMutableMemory,
          template <class> class Container = StdContainer<std::vector>::Container>
class LoadInfoMutableMemory {
 public:
  using size_type = typename LoadInfo::size_type;

  // Cannot be default-constructed, copied, or moved.
  LoadInfoMutableMemory() = delete;
  LoadInfoMutableMemory(const LoadInfoMutableMemory&) = delete;
  LoadInfoMutableMemory(LoadInfoMutableMemory&&) = default;

  // Construction just stores the references and the callable object.
  LoadInfoMutableMemory(Diagnostics& diag, LoadInfo& load_info, GetMutableMemory get_mutable_memory)
      : diag_(diag), load_info_{load_info}, get_mutable_memory_{std::move(get_mutable_memory)} {}

  // The Init() call prepares the mappings from the LoadInfo segments; it must
  // be called before using the Memory API methods.  It's separate from the
  // constructor only so that it can fail if Container operations fail when
  // building up the internal table.
  bool Init() {
    using namespace std::literals::string_view_literals;
    constexpr auto is_mutable = [](const auto& segment) -> bool {
      return segment.writable() && segment.filesz() > 0;
    };
    for (auto& segment : load_info_.segments()) {
      if (LoadInfo::VisitSegment(is_mutable, segment) &&
          !segments_.emplace_back(diag_, "too many mutable segments"sv, segment)) {
        return false;
      }
    }
    // This must be initilaized after loading up the container, since a
    // previous end() iterator may have been invalidated.
    hint_ = segments_.end();
    return true;
  }

  // These are the Memory API methods for mutable memory.
  // See <lib/elfldltl/memory.h> for the API specification.

  template <typename T, typename U>
  bool Store(size_type ptr, U value) {
    auto store = [ptr, value](auto& memory) -> bool {
      return memory.template Store<T>(ptr, value);
    };
    return OnMemory<T>(ptr, store);
  }

  template <typename T, typename U>
  bool StoreAdd(size_type ptr, U value) {
    auto store = [ptr, value](auto& memory) -> bool {
      return memory.template StoreAdd<T>(ptr, value);
    };
    return OnMemory<T>(ptr, store);
  }

 private:
  using LoadSegment = typename LoadInfo::Segment;
  using MutableMemory = std::decay_t<  //
      decltype(std::declval<GetMutableMemory>()(std::declval<Diagnostics&>(),
                                                std::declval<LoadSegment&>())
                   .value())>;

  class MutableSegment {
   public:
    MutableSegment(LoadSegment& load_segment) : segment_(load_segment) {
      LoadInfo::VisitSegment(
          [this](const auto& segment) -> std::true_type {
            vaddr_ = segment.vaddr();
            filesz_ = segment.filesz();
            return {};
          },
          segment_);
    }

    bool contains(size_type ptr, size_type size) const {
      return vaddr_ <= ptr && filesz_ >= size && ptr - vaddr_ < filesz_ - size;
    }

    bool operator<(size_type ptr) const { return vaddr_ + filesz_ <= ptr; }

    bool has_memory() const { return memory_.has_value(); }

    MutableMemory& memory() { return *memory_; }

    void set_memory(MutableMemory result) { memory_.emplace(std::move(result)); }

    LoadSegment& segment() { return segment_; }

   private:
    LoadSegment & segment_;
    size_type vaddr_;
    size_type filesz_;
    std::optional<MutableMemory> memory_;
  };

  using SegmentList = Container<MutableSegment>;
  using SegmentListIterator = typename SegmentList::iterator;

  fit::result<bool, SegmentListIterator> GetMutableSegment(size_type ptr, size_type size) {
    // There will usually be many successive calls for addresses in the same
    // segment, and often only one segment actually touched.  So cache the last
    // segment used as a hint before doing binary search.
    if (hint_ == segments_.end() || !hint_->contains(ptr, size)) {
      auto it = std::lower_bound(segments_.begin(), segments_.end(), ptr);
      if (it == segments_.end() || !it->contains(ptr, size)) {
        return fit::error{
            diag_.FormatError("invalid relocation for ", size, " bytes", FileAddress{ptr})};
      }
      if (!it->has_memory()) {
        // This segment hasn't been mutated yet.  Get a MutableMemory for it.
        auto result = get_mutable_memory_(diag_, it->segment());
        if (result.is_error()) {
          return result.take_error();
        }
        it->set_memory(std::move(result).value());
      }
      hint_ = it;
    }
    return fit::ok(hint_);
  }

  template <typename T, typename Op>
  bool OnMemory(size_type ptr, Op&& op) {
    auto result = GetMutableSegment(ptr, sizeof(T));
    if (result.is_error()) {
      return result.error_value();
    }
    return std::forward<Op>(op)(result.value()->memory());
  }

  Diagnostics& diag_;
  LoadInfo& load_info_;
  [[no_unique_address]] GetMutableMemory get_mutable_memory_;
  SegmentList segments_;
  SegmentListIterator hint_;
};

// Deduction guide.  Explicit template parameters must be used for everything
// to use a different Container template, since deduction guides can't be
// partially specialized.
template <class Diagnostics, class LoadInfo, typename GetMutableMemory>
LoadInfoMutableMemory(Diagnostics&, LoadInfo&, GetMutableMemory&&)
    -> LoadInfoMutableMemory<Diagnostics, LoadInfo, std::decay_t<GetMutableMemory>>;

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_LOADINFO_MUTABLE_MEMORY_H_
