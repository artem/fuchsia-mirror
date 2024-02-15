// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_RISCV64_FEATURE_H_
#define ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_RISCV64_FEATURE_H_

#include <bitset>
#include <string_view>

namespace arch {

// An enumeration of RISC-V features.
//
// This is not intended to be exhaustive, but rather to include the features
// that the kernel currently depends on.
//
// Values should not be prescribed manually and are intended to automatically
// increment from 0.
enum class RiscvFeature {
  // SuperVisor extension for Page-Based Memory Types
  kSvpbmt,

  // The standard V extension for vector support
  kVector,

  // Cache-Block Management Operations
  kZicbom,

  // Cache-Block Zero Operations
  kZicboz,

  // Counter CSRs are accessible
  kZicntr,

  // Not a real feature, but a bookend that allows us to calculate the largest
  // underlying enum value.
  kMax,

  // No values should appear after kMax.
};

// A simple container abstraction around the set of RISC-V features.
class RiscvFeatures {
 public:
  // Whether a given feature is supported.
  bool operator[](RiscvFeature Feature) const { return bits_[static_cast<size_t>(Feature)]; }

  RiscvFeatures& operator&=(const RiscvFeatures& other) {
    bits_ &= other.bits_;
    return *this;
  }

  // Sets support for a given feature.
  RiscvFeatures& Set(RiscvFeature Feature, bool supported = true) {
    bits_.set(static_cast<size_t>(Feature), supported);
    return *this;
  }

  // Sets all features referenced in an ISA string.
  RiscvFeatures& SetMany(std::string_view isa_string);

 private:
  std::bitset<static_cast<size_t>(RiscvFeature::kMax)> bits_;
};

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_RISCV64_FEATURE_H_
