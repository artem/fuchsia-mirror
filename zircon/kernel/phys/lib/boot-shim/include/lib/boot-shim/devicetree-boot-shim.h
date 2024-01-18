// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_BOOT_SHIM_H_
#define ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_BOOT_SHIM_H_

#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/fit/function.h>
#include <lib/fit/result.h>
#include <lib/uart/all.h>
#include <lib/zbitl/storage-traits.h>

#include <type_traits>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/macros.h>

#include "boot-shim.h"

namespace boot_shim {

// Proxy for allocator. This allocator represents the following interface:
//
//   void* Allocator(size_t byte_count, size_t alignment);
//
// On success returns a non NULL pointer to an aligned memory block of at least |byte_count| bytes.
// On failure |nullptr| is returned.
using DevicetreeBootShimAllocator =
    fit::inline_function<void*(size_t, size_t, fbl::AllocChecker&), 32>;

struct DevicetreeMmioRange {
  static DevicetreeMmioRange From(const devicetree::RegPropertyElement& reg) {
    return {.address = reg.address().value_or(0),
            .size = static_cast<size_t>(reg.size().value_or(0))};
  }

  constexpr bool empty() const { return size == 0; }
  constexpr uint64_t end() const { return address + size; }

  uint64_t address = 0;
  size_t size = 0;
};

// Provides an observer for MMIO Ranges. Devicetree Items that will provide configuration for
// kernel drivers, that will be interacted through MMIO must notify through this observer.
//
using DevicetreeBootShimMmioObserver = fit::inline_function<void(const DevicetreeMmioRange&)>;

// A DevicetreeBootShim represents a collection of items, which look into the devicetree itself
// to gather information to produce ZBI items.
//
// A devicetree item requires inspecting the devicetree. In addition to the API requirements from a
// BootShim's item it must fulfill a devicetree Matcher API. |DevicetreeItemBase| provides the
// expected API for the |DevicetreeBootShim|'s items.
//
// Prefer inheriting from |DevicetreeItemBase| when possible.
template <typename... Items>
class DevicetreeBootShim : public BootShim<Items...> {
 private:
  using Base = BootShim<Items...>;

 public:
  explicit DevicetreeBootShim(const char* name, devicetree::Devicetree dt, FILE* log = stdout)
      : Base(name, log), dt_(dt) {}

  // Initializes all devicetree boot shim items.
  // As part of the initialization each matcher's |Init(shim_name, log)| is called, followed by a
  // single invocation of |devicetree::Match| allowing each provided matcher to collect information
  // from the devicetree.
  bool Init() {
    auto match_with = [this](auto&... items) {
      (items.Init(*this), ...);
      return devicetree::Match(dt_, items...);
    };
    return this->template OnSelectItems<IsDevicetreeItem>(match_with);
  }

  const devicetree::Devicetree& devicetree() const { return dt_; }

  const DevicetreeBootShimAllocator& allocator() const {
    ZX_ASSERT(allocator_);
    return allocator_;
  }
  const DevicetreeBootShimMmioObserver& mmio_observer() const {
    ZX_ASSERT(mmio_observer_);
    return mmio_observer_;
  }

  void set_allocator(DevicetreeBootShimAllocator&& allocator) { allocator_ = std::move(allocator); }

  // Optional: Set a callback for MMIO Ranges of interest for each |Item|
  // of the shim.
  void set_mmio_observer(DevicetreeBootShimMmioObserver&& observer) {
    mmio_observer_ = std::move(observer);
  }

 private:
  DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(HasInit, Init, void (C::*)(const DevicetreeBootShim& shim));

  template <typename T>
  using IsDevicetreeItem = std::conditional_t<HasInit<T>::value && devicetree::kIsMatcher<T>,
                                              std::true_type, std::false_type>;
  devicetree::Devicetree dt_;
  DevicetreeBootShimAllocator allocator_ = nullptr;
  DevicetreeBootShimMmioObserver mmio_observer_ = [](const DevicetreeMmioRange&) {};
};

}  // namespace boot_shim

#endif  // ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_BOOT_SHIM_H_
