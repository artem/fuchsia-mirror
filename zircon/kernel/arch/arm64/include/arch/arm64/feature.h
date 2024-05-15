// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ARM64_FEATURE_H_
#define ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ARM64_FEATURE_H_

#include <assert.h>
#include <stdint.h>
#include <zircon/compiler.h>
#include <zircon/features.h>

#include <arch/arm64.h>
#include <kernel/cpu.h>

enum arm64_microarch {
  UNKNOWN,

  ARM_CORTEX_A32,
  ARM_CORTEX_A35,
  ARM_CORTEX_A53,
  ARM_CORTEX_A55,
  ARM_CORTEX_A57,
  ARM_CORTEX_A65,
  ARM_CORTEX_A65AE,
  ARM_CORTEX_A72,
  ARM_CORTEX_A73,
  ARM_CORTEX_A75,
  ARM_CORTEX_A76,
  ARM_CORTEX_A76AE,
  ARM_CORTEX_A77,
  ARM_CORTEX_A78,
  ARM_CORTEX_A78AE,
  ARM_CORTEX_A78C,
  ARM_CORTEX_A510,
  ARM_CORTEX_A520,
  ARM_CORTEX_A710,
  ARM_CORTEX_A715,
  ARM_CORTEX_A720,
  ARM_CORTEX_X1,
  ARM_CORTEX_X1C,
  ARM_CORTEX_X2,
  ARM_CORTEX_X3,
  ARM_NEOVERSE_E1,
  ARM_NEOVERSE_N1,
  ARM_NEOVERSE_N2,
  ARM_NEOVERSE_V1,

  CAVIUM_CN88XX,
  CAVIUM_CN99XX,

  APPLE_UNKNOWN,

  QEMU_TCG,
};

enum arm64_microarch midr_to_microarch(uint32_t midr);

extern uint32_t arm64_isa_features;
extern bool feat_pmuv3_enabled;

inline bool arm64_feature_test(uint32_t feature) { return arm64_isa_features & feature; }

// block size of the dc zva instruction, dcache cache line and icache cache line
extern uint32_t arm64_zva_size;
extern uint32_t arm64_icache_size;
extern uint32_t arm64_dcache_size;

// size of the asid
enum class arm64_asid_width {
  UNKNOWN,  // invalid, should be set prior to anything actually using it
  ASID_8,
  ASID_16,
};

struct arm64_mmu_features {
  arm64_asid_width asid_width;

  // supported stage1 page granules
  bool s1_page_4k;
  bool s1_page_16k;
  bool s1_page_64k;

  // privileged access never and related user access override
  bool pan;
  bool uao;

  // accessed and dirty bits
  bool accessed_bit;
  bool dirty_bit;

  // extended CCSIDR register format
  bool ccsidx;
};

// the global feature structure for mmu features
extern arm64_mmu_features arm64_mmu_features;

inline enum arm64_asid_width arm64_asid_width() {
  DEBUG_ASSERT(arm64_mmu_features.asid_width != arm64_asid_width::UNKNOWN);
  return arm64_mmu_features.asid_width;
}

// call on every cpu to initialize the feature set
void arm64_feature_init();

// dump the feature set
void arm64_feature_debug(bool full);

// Returns true if the current CPU is the first member of a cluster according to
// MPIDR.
bool arm64_feature_current_is_first_in_cluster();

void arm64_get_cache_info(arm64_cache_info_t* info);
void arm64_dump_cache_info(cpu_num_t cpu);

#endif  // ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ARM64_FEATURE_H_
