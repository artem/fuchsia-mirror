// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_INCLUDE_MAGMA_ARM_MALI_TYPES_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_INCLUDE_MAGMA_ARM_MALI_TYPES_H_

#include <lib/magma/magma_common_defs.h>

#include <cstdint>

#define MAGMA_VENDOR_VERSION_ARM 4

// These flags can be specified to magma_map_buffer.
enum MagmaArmMaliGpuMapFlags {
  // Accesses to this data should be GPU-L2 coherent.
  kMagmaArmMaliGpuMapFlagInnerShareable = (1 << MAGMA_MAP_FLAG_VENDOR_SHIFT),

  // Accesses to this data should be coherent with the CPU
  kMagmaArmMaliGpuMapFlagBothShareable = (1 << (MAGMA_MAP_FLAG_VENDOR_SHIFT + 1)),

  // Only protected atoms can access this data.
  kMagmaArmMaliGpuMapFlagProtected = (1 << (MAGMA_MAP_FLAG_VENDOR_SHIFT + 2)),
};

enum AtomFlags {
  kAtomFlagRequireFragmentShader = (1 << 0),

  // Compute shaders also include vertex and geometry shaders.
  kAtomFlagRequireComputeShader = (1 << 1),
  kAtomFlagRequireTiler = (1 << 2),
  kAtomFlagRequireCycleCounter = (1 << 3),
  kAtomFlagProtected = (1 << 4),  // Executes in protected mode.
  kAtomFlagForceSlot0 = (1 << 5),
  kAtomFlagForceSlot1 = (1 << 6),
  kAtomFlagForceSlot2 = (1 << 7),

  // Coalesce completion notifications with later notifications.
  kAtomFlagCoalesce = (1 << 8),

  // Atoms with this flag set are processed completely in the MSD and aren't
  // sent to hardware.
  kAtomFlagSoftware = (1 << 30),

  kAtomFlagSemaphoreSet = kAtomFlagSoftware | 1,
  kAtomFlagSemaphoreReset = kAtomFlagSoftware | 2,
  kAtomFlagSemaphoreWait = kAtomFlagSoftware | 3,
  kAtomFlagSemaphoreWaitAndReset = kAtomFlagSoftware | 4,

  // Allocate new JIT memory regions. Must be followed by magma_arm_jit_atom_trailer and one or more
  // magma_arm_jit_memory_allocate_info. kAtomFlagJitAddressSpaceAllocate must have been sent before
  // this atom.
  kAtomFlagJitMemoryAllocate = kAtomFlagSoftware | 5,

  // Release in-use jit memory regions. Must be followed by magma_arm_jit_atom_trailer and one or
  // more magma_arm_jit_memory_free_info.
  kAtomFlagJitMemoryFree = kAtomFlagSoftware | 6,

  // Signals that a region of address space should be used to allocate JIT atoms. Followed by a
  // magma_arm_jit_address_space_allocate_info specifying details of the address region. A client
  // may send this only once on a connection.
  kAtomFlagJitAddressSpaceAllocate = kAtomFlagSoftware | 7,
};

enum ArmMaliResultCodeFlags {
  // The result code is for a software event, not one created by hardware.
  kArmMaliSoftwareEvent = (1u << 14),

  // A software event succeeded.
  kArmMaliSoftwareEventSuccess = (1u << 13),

  // This event is not related to a specific GPU job.
  kArmMaliSoftwareEventMisc = (1u << 12),
};

enum ArmMaliResultCode {
  kArmMaliResultRunning = 0,

  // These codes match the result codes from hardware.
  kArmMaliResultSuccess = 1,
  kArmMaliResultSoftStopped = 3,
  // The atom was terminated with a hard stop.
  kArmMaliResultAtomTerminated = 4,

  // These faults happen while attempting to read jobs in a job chain.
  kArmMaliResultFault = 0x40,
  kArmMaliResultConfigFault = kArmMaliResultFault + 0,
  kArmMaliResultPowerFault = kArmMaliResultFault + 1,
  kArmMaliResultReadFault = kArmMaliResultFault + 2,
  kArmMaliResultWriteFault = kArmMaliResultFault + 3,
  kArmMaliResultAffinityFault = kArmMaliResultFault + 4,
  kArmMaliResultBusFault = kArmMaliResultFault + 8,

  // These faults happpen when executing jobs.
  kArmMaliResultInstructionFault = 0x50,
  kArmMaliResultProgramCounterInvalidFault = kArmMaliResultInstructionFault,
  kArmMaliResultEncodingInvalidFault = kArmMaliResultInstructionFault + 0x1,
  kArmMaliResultTypeMismatchFault = kArmMaliResultInstructionFault + 0x2,
  kArmMaliResultOperandFault = kArmMaliResultInstructionFault + 0x3,
  kArmMaliResultTlsFault = kArmMaliResultInstructionFault + 0x4,
  kArmMaliResultBarrierFault = kArmMaliResultInstructionFault + 0x5,
  kArmMaliResultAlignmentFault = kArmMaliResultInstructionFault + 0x6,
  kArmMaliResultDataInvalidFault = kArmMaliResultInstructionFault + 0x8,
  kArmMaliResultTileRangeFault = kArmMaliResultInstructionFault + 0xa,
  kArmMaliResultOutOfMemoryFault = 0x60,

  kArmMaliResultUnknownFault = 0x7f,

  // Faults for software atoms. Must match ICD-internal values.
  kArmMaliResultTimedOut = kArmMaliSoftwareEvent | 1,
  kArmMaliResultMemoryGrowthFailed = kArmMaliSoftwareEvent | 0,
  kArmMaliResultJobInvalid = kArmMaliSoftwareEvent | 3,

  // This result code is only generated by the driver itself.
  kArmMaliResultTerminated =
      kArmMaliSoftwareEvent | kArmMaliSoftwareEventSuccess | kArmMaliSoftwareEventMisc,
};

// These values match the hardware.
enum ArmMaliCacheCoherencyStatus {
  kArmMaliCacheCoherencyAceLite = 0,
  kArmMaliCacheCoherencyAce = 1,

  kArmMaliCacheCoherencyNone = 31,
};

enum ArmMaliDependencyType {
  // Data dependencies cause dependent atoms to fail.
  kArmMaliDependencyData = 0,

  // Order dependencies allow dependent atoms to execute, even on failure.
  kArmMaliDependencyOrder = 1,
};

struct magma_arm_mali_dependency {
  uint8_t type;         // ArmMaliDependencyType
  uint8_t atom_number;  // 0 means no dependency.
} __attribute__((packed));

// This is arbitrary user data that's used to identify an atom.
struct magma_arm_mali_user_data {
  uint64_t data[2];
} __attribute__((packed));

struct magma_arm_mali_atom {
  uint64_t size;  // Size of this structure. Used for backwards compatibility.
  uint64_t job_chain_addr;
  struct magma_arm_mali_user_data data;
  uint32_t flags;  // a set of AtomFlags.
  uint8_t atom_number;
  int8_t priority;  // 0 is default. < 0 is less important, > 0 is more important.

  magma_arm_mali_dependency dependencies[2];
} __attribute__((packed));

struct magma_arm_mali_status {
  magma_arm_mali_user_data data;
  uint32_t result_code;
  uint8_t atom_number;
} __attribute__((packed));

struct magma_arm_jit_memory_allocate_info {
  // Version of this struct - must be 0 for now.
  uint8_t version_number;
  // Align to 2 bytes.
  uint8_t padding;
  // This is a hint to the MSD to help it determine which allocation to reuse. If non-zero, the MSD
  // will prefer reusing allocations with the same usage_id.
  uint16_t usage_id;
  // This ID is used to link allocation and freeing. An in-use ID must not be reused until it's
  // freed.
  uint8_t id;
  // |bin_id| must match when reusing memory.
  uint8_t bin_id;
  // Flags for the allocation. Currently always 0.
  uint8_t flags;
  // This is the maximum number of allocations allowed from this bin_id.
  uint8_t max_allocations;
  // The GPU VA to store the JIT region VA to.
  uint64_t address;
  // The GPU VA that can contain additional information about the heap (unused for now).
  uint64_t heap_info_address;
  // The number of pages to allocate.
  uint64_t va_page_count;
  // The minimum number of pages to have committed initially.
  uint64_t committed_page_count;
  // The granularity of how many pages to add during a page fault.
  uint64_t extend_page_count;
} __attribute__((packed));

static_assert(sizeof(magma_arm_jit_memory_allocate_info) % 8 == 0,
              "jit memory info size must be multiple of 8 bytes");

struct magma_arm_jit_memory_free_info {
  // Version of this struct - must be 0 for now.
  uint8_t version_number;
  // ID that must match |id| in the |magma_arm_jit_memory_allocate_info|.
  uint8_t id;

  uint8_t padding[6];
} __attribute__((packed));

static_assert(sizeof(magma_arm_jit_memory_free_info) % 8 == 0,
              "jit memory free info size not aligned");

struct magma_arm_jit_address_space_allocate_info {
  // Version of this struct - must be 0 for now.
  uint8_t version_number;
  // |trim_level| is the percentage of an allocation to free after a jit free.
  // An allocation will never shrink below |committed_page_count| in size.
  uint8_t trim_level;
  // Max number of allocations overall for this connection.
  uint8_t max_allocations;
  uint8_t padding[5];
  // Starting GPU address of the region. Non-JIT allocations must never be mapped into this region.
  uint64_t address;
  // Number of pages to dedicate to the JIT address space.
  uint64_t va_page_count;
} __attribute__((packed));

static_assert(sizeof(magma_arm_jit_address_space_allocate_info) % 8 == 0,
              "jit address space size must be a multiple of 8 bytes");

// This struct is used to specify how many other structs are to follow.
struct magma_arm_jit_atom_trailer {
  uint32_t jit_memory_info_count;
  uint8_t padding[4];
} __attribute__((packed));
static_assert(sizeof(magma_arm_jit_atom_trailer) % 8 == 0,
              "jit trailer must be a multiple of 8 bytes");

// Returned by kMsdArmVendorQueryDeviceTimestamp.
struct magma_arm_mali_device_timestamp_return {
  uint64_t monotonic_raw_timestamp_before;
  uint64_t monotonic_timestamp;
  uint64_t device_timestamp;
  uint64_t device_cycle_count;
  uint64_t monotonic_raw_timestamp_after;
} __attribute__((packed));

// Returned by kMsdArmVendorQueryDeviceProperties.
struct magma_arm_mali_device_properties_return_header {
  // Size of the header struct. Entries will immediately follow the header.
  uint64_t header_size;
  // Number of entries following the header. Entries are sorted low to high by id.
  uint64_t entry_count;
} __attribute__((packed));

struct magma_arm_mali_device_properties_return_entry {
  uint64_t id;
  uint64_t value;
} __attribute__((packed));

#endif
