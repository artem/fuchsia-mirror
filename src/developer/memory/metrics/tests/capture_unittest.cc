// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/capture.h"

#include <zircon/types.h>

#include <gtest/gtest.h>

#include "src/developer/memory/metrics/filters.h"
#include "src/developer/memory/metrics/tests/test_utils.h"

namespace memory {
namespace test {

using CaptureUnitTest = testing::Test;

const static zx_info_kmem_stats_extended_t _kmem = {.total_bytes = 300,
                                                    .free_bytes = 100,
                                                    .wired_bytes = 10,
                                                    .total_heap_bytes = 20,
                                                    .free_heap_bytes = 30,
                                                    .vmo_bytes = 40,
                                                    .vmo_pager_total_bytes = 15,
                                                    .vmo_pager_newest_bytes = 4,
                                                    .vmo_pager_oldest_bytes = 8,
                                                    .vmo_discardable_locked_bytes = 3,
                                                    .vmo_discardable_unlocked_bytes = 7,
                                                    .mmu_overhead_bytes = 50,
                                                    .ipc_bytes = 60,
                                                    .other_bytes = 70};
const static GetInfoResponse kmem_info = {
    TestUtils::kRootHandle, ZX_INFO_KMEM_STATS_EXTENDED, &_kmem, sizeof(_kmem), 1, ZX_OK};

const static zx_info_handle_basic_t _self = {.koid = TestUtils::kSelfKoid};
const static GetInfoResponse self_info = {
    TestUtils::kSelfHandle, ZX_INFO_HANDLE_BASIC, &_self, sizeof(_self), 1, ZX_OK};

const zx_koid_t proc_koid = 10;
const zx_handle_t proc_handle = 100;
const char proc_name[] = "P1";
const static GetPropertyResponse proc_prop = {proc_handle, ZX_PROP_NAME, proc_name,
                                              sizeof(proc_name), ZX_OK};
const static GetProcessesCallback proc_cb = {1, proc_handle, proc_koid, 0};

const zx_koid_t proc2_koid = 20;
const zx_handle_t proc2_handle = 200;
const zx_handle_t proc2_job = 1000;
const char proc2_name[] = "P2";
const static GetPropertyResponse proc2_prop = {proc2_handle, ZX_PROP_NAME, proc2_name,
                                               sizeof(proc2_name), ZX_OK};
const static GetProcessesCallback proc2_cb = {1, proc2_handle, proc2_koid, proc2_job};

// A 3rd process within the same job as P2. This process represents a Starnix kernel. If this is
// included in the list of processes, P2 will be assumed to run under Starnix.
const zx_koid_t proc3_koid = 30;
const zx_handle_t proc3_handle = 300;
const char proc3_name[] = "starnix_kernel.cm";
const static GetPropertyResponse proc3_prop = {proc3_handle, ZX_PROP_NAME, proc3_name,
                                               sizeof(proc3_name), ZX_OK};
const static GetProcessesCallback proc3_cb = {1, proc3_handle, proc3_koid, proc2_job};

const zx_koid_t vmo_koid = 1000;
const uint64_t vmo_size = 10000;
const char vmo_name[] = "V1";
const static zx_info_vmo_t _vmo = {
    .koid = vmo_koid,
    .name = "V1",
    .size_bytes = vmo_size,
};
const static zx_info_vmo_t _vmo_dup[] = {{
                                             .koid = vmo_koid,
                                             .name = "V1",
                                             .size_bytes = vmo_size,
                                         },
                                         {
                                             .koid = vmo_koid,
                                             .name = "V1",
                                             .size_bytes = vmo_size,
                                         }};
const static GetInfoResponse vmos_info = {proc_handle, ZX_INFO_PROCESS_VMOS, &_vmo, sizeof(_vmo), 1,
                                          ZX_OK};
const static GetInfoResponse vmos_dup_info = {
    proc_handle, ZX_INFO_PROCESS_VMOS, _vmo_dup, sizeof(_vmo), 1, ZX_OK};

const zx_koid_t vmo2_koid = 2000;
const uint64_t vmo2_size = 20000;
const char vmo2_name[] = "V2";
const static zx_info_vmo_t _vmo2 = {
    .koid = vmo2_koid,
    .name = "V2",
    .size_bytes = vmo2_size,
};
const static GetInfoResponse vmos2_info = {
    proc2_handle, ZX_INFO_PROCESS_VMOS, &_vmo2, sizeof(_vmo2), 1, ZX_OK};

TEST_F(CaptureUnitTest, KMEM) {
  Capture c;
  auto ret = TestUtils::GetCapture(&c, CaptureLevel::KMEM,
                                   {
                                       .get_info = {self_info, kmem_info},
                                   });
  EXPECT_EQ(ZX_OK, ret);
  const auto& got_kmem = c.kmem();
  EXPECT_EQ(_kmem.total_bytes, got_kmem.total_bytes);
}

TEST_F(CaptureUnitTest, Process) {
  // Process and VMO need to capture the same info.
  Capture c;
  auto ret = TestUtils::GetCapture(&c, CaptureLevel::VMO,
                                   {.get_processes = {ZX_OK, {proc_cb}},
                                    .get_property = {proc_prop},
                                    .get_info = {self_info, kmem_info, vmos_info, vmos_info}});
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(1U, c.koid_to_process().size());
  const auto& process = c.process_for_koid(proc_koid);
  EXPECT_EQ(proc_koid, process.koid);
  EXPECT_STREQ(proc_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(1U, c.koid_to_vmo().size());
  EXPECT_EQ(vmo_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo_koid);
  EXPECT_EQ(vmo_koid, vmo.koid);
  EXPECT_STREQ(vmo_name, vmo.name);
}

TEST_F(CaptureUnitTest, VMO) {
  Capture c;
  auto ret = TestUtils::GetCapture(&c, CaptureLevel::VMO,
                                   {.get_processes = {ZX_OK, {proc_cb}},
                                    .get_property = {proc_prop},
                                    .get_info = {self_info, kmem_info, vmos_info, vmos_info}});
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(1U, c.koid_to_process().size());
  const auto& process = c.process_for_koid(proc_koid);
  EXPECT_EQ(proc_koid, process.koid);
  EXPECT_STREQ(proc_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(1U, c.koid_to_vmo().size());
  EXPECT_EQ(vmo_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo_koid);
  EXPECT_EQ(vmo_koid, vmo.koid);
  EXPECT_STREQ(vmo_name, vmo.name);
}

TEST_F(CaptureUnitTest, VMODouble) {
  Capture c;
  auto ret = TestUtils::GetCapture(&c, CaptureLevel::VMO,
                                   {
                                       .get_processes = {ZX_OK, {proc_cb, proc2_cb}},
                                       .get_property = {proc_prop, proc2_prop},
                                       .get_info =
                                           {
                                               self_info,
                                               kmem_info,
                                               vmos_info,
                                               vmos2_info,
                                           },
                                   });
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(2U, c.koid_to_process().size());
  EXPECT_EQ(2U, c.koid_to_vmo().size());

  const auto& process = c.process_for_koid(proc_koid);
  EXPECT_EQ(proc_koid, process.koid);
  EXPECT_STREQ(proc_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(vmo_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo_koid);
  EXPECT_EQ(vmo_koid, vmo.koid);
  EXPECT_STREQ(vmo_name, vmo.name);

  const auto& process2 = c.process_for_koid(proc2_koid);
  EXPECT_EQ(proc2_koid, process2.koid);
  EXPECT_STREQ(proc2_name, process2.name);
  EXPECT_EQ(1U, process2.vmos.size());
  EXPECT_EQ(vmo2_koid, process2.vmos[0]);
  const auto& vmo2 = c.vmo_for_koid(vmo2_koid);
  EXPECT_EQ(vmo2_koid, vmo2.koid);
  EXPECT_STREQ(vmo2_name, vmo2.name);
}

TEST_F(CaptureUnitTest, VMOProcessDuplicate) {
  Capture c;
  auto ret =
      TestUtils::GetCapture(&c, CaptureLevel::VMO,
                            {.get_processes = {ZX_OK, {proc_cb}},
                             .get_property = {proc_prop},
                             .get_info = {self_info, kmem_info, vmos_dup_info, vmos_dup_info}});
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(1U, c.koid_to_process().size());
  const auto& process = c.process_for_koid(proc_koid);
  EXPECT_EQ(proc_koid, process.koid);
  EXPECT_STREQ(proc_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(1U, c.koid_to_vmo().size());
  EXPECT_EQ(vmo_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo_koid);
  EXPECT_EQ(vmo_koid, vmo.koid);
  EXPECT_STREQ(vmo_name, vmo.name);
}

TEST_F(CaptureUnitTest, ProcessPropBadState) {
  // If the process disappears we should ignore it and continue.
  Capture c;
  auto ret = TestUtils::GetCapture(
      &c, CaptureLevel::PROCESS,
      {.get_processes = {ZX_OK, {proc_cb, proc2_cb}},
       .get_property = {{proc_handle, ZX_PROP_NAME, nullptr, 0, ZX_ERR_BAD_STATE}, proc2_prop},
       .get_info = {self_info, kmem_info, vmos2_info, vmos2_info}});
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(1U, c.koid_to_process().size());
  const auto& process = c.process_for_koid(proc2_koid);
  EXPECT_EQ(proc2_koid, process.koid);
  EXPECT_STREQ(proc2_name, process.name);
}

TEST_F(CaptureUnitTest, VMOCountBadState) {
  // If the process disappears we should ignore it and continue.
  Capture c;
  auto ret = TestUtils::GetCapture(
      &c, CaptureLevel::VMO,
      {.get_processes = {ZX_OK, {proc_cb, proc2_cb}},
       .get_property = {proc_prop, proc2_prop},
       .get_info = {self_info,
                    kmem_info,
                    {proc_handle, ZX_INFO_PROCESS_VMOS, &_vmo, sizeof(_vmo), 1, ZX_ERR_BAD_STATE},
                    vmos2_info}});
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(1U, c.koid_to_process().size());
  const auto& process = c.process_for_koid(proc2_koid);
  EXPECT_EQ(proc2_koid, process.koid);
  EXPECT_STREQ(proc2_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(1U, c.koid_to_vmo().size());
  EXPECT_EQ(vmo2_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo2_koid);
  EXPECT_EQ(vmo2_koid, vmo.koid);
  EXPECT_STREQ(vmo2_name, vmo.name);
}

TEST_F(CaptureUnitTest, VMOGetBadState) {
  // If the process disappears we should ignore it and continue.
  Capture c;
  auto ret = TestUtils::GetCapture(
      &c, CaptureLevel::VMO,
      {.get_processes = {ZX_OK, {proc_cb, proc2_cb}},
       .get_property = {proc_prop, proc2_prop},
       .get_info = {self_info,
                    kmem_info,
                    {proc_handle, ZX_INFO_PROCESS_VMOS, &_vmo, sizeof(_vmo), 1, ZX_ERR_BAD_STATE},
                    vmos2_info}});
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(1U, c.koid_to_process().size());
  const auto& process = c.process_for_koid(proc2_koid);
  EXPECT_EQ(proc2_koid, process.koid);
  EXPECT_STREQ(proc2_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(1U, c.koid_to_vmo().size());
  EXPECT_EQ(vmo2_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo2_koid);
  EXPECT_EQ(vmo2_koid, vmo.koid);
  EXPECT_STREQ(vmo2_name, vmo.name);
}

TEST_F(CaptureUnitTest, VMORooted) {
  Capture c;
  TestUtils::CreateCapture(&c,
                           {.vmos =
                                {
                                    {.koid = 1, .name = "R1", .committed_bytes = 100},
                                    {.koid = 2, .name = "C1", .size_bytes = 50, .parent_koid = 1},
                                    {.koid = 3, .name = "C2", .size_bytes = 25, .parent_koid = 2},
                                },
                            .processes =
                                {
                                    {.koid = 10, .name = "p1", .vmos = {1, 2, 3}},
                                },
                            .rooted_vmo_names = {"R1"}});
  // Carve up the rooted vmo into child and grandchild.
  EXPECT_EQ(50U, c.vmo_for_koid(1).committed_bytes);
  EXPECT_EQ(25U, c.vmo_for_koid(2).committed_bytes);
  EXPECT_EQ(25U, c.vmo_for_koid(3).committed_bytes);
}

TEST_F(CaptureUnitTest, VMORootedPartialCommit) {
  Capture c;
  TestUtils::CreateCapture(&c,
                           {.vmos =
                                {
                                    {.koid = 1, .name = "R1", .committed_bytes = 75},
                                    {.koid = 2, .name = "C1", .size_bytes = 77, .parent_koid = 1},
                                    {.koid = 3, .name = "C2", .size_bytes = 100, .parent_koid = 2},
                                },
                            .processes =
                                {
                                    {.koid = 10, .name = "p1", .vmos = {1, 2, 3}},
                                },
                            .rooted_vmo_names = {"R1"}});
  // The grandchild should take all available committed bytes from the root.
  EXPECT_EQ(0U, c.vmo_for_koid(1).committed_bytes);
  EXPECT_EQ(0U, c.vmo_for_koid(2).committed_bytes);
  EXPECT_EQ(75U, c.vmo_for_koid(3).committed_bytes);
}

TEST_F(CaptureUnitTest, StarnixVMO) {
  Capture c;
  FilterJobWithProcess filter(proc3_name);
  auto ret = TestUtils::GetCapture(
      &c, CaptureLevel::VMO,
      {.get_processes = {ZX_OK, {proc_cb, proc2_cb, proc3_cb}},
       .get_property = {proc_prop, proc2_prop, proc3_prop},
       .get_info = {self_info,
                    kmem_info,
                    vmos_info,
                    vmos2_info,
                    {proc3_handle, ZX_INFO_PROCESS_VMOS, &_vmo2, sizeof(_vmo2), 1, ZX_OK}}},
      &filter);
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(2U, c.koid_to_process().size());
  EXPECT_EQ(2U, c.koid_to_vmo().size());
  const auto& process = c.process_for_koid(proc_koid);
  EXPECT_EQ(proc_koid, process.koid);
  EXPECT_STREQ(proc_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(vmo_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo_koid);
  EXPECT_EQ(vmo_koid, vmo.koid);
  EXPECT_STREQ(vmo_name, vmo.name);

  const auto& process3 = c.process_for_koid(proc3_koid);
  EXPECT_EQ(proc3_koid, process3.koid);
  EXPECT_STREQ(proc3_name, process3.name);
  EXPECT_EQ(1U, process3.vmos.size());
  EXPECT_EQ(vmo2_koid, process3.vmos[0]);
  const auto& vmo2 = c.vmo_for_koid(vmo2_koid);
  EXPECT_EQ(vmo2_koid, vmo2.koid);
  EXPECT_STREQ(vmo2_name, vmo2.name);
}

TEST_F(CaptureUnitTest, StarnixVMOIncludeStarnix) {
  Capture c;
  auto ret = TestUtils::GetCapture(
      &c, CaptureLevel::VMO,
      {.get_processes = {ZX_OK, {proc_cb, proc2_cb, proc3_cb}},
       .get_property = {proc_prop, proc2_prop, proc3_prop},
       .get_info = {self_info,
                    kmem_info,
                    vmos_info,
                    vmos2_info,
                    {proc3_handle, ZX_INFO_PROCESS_VMOS, &_vmo, sizeof(_vmo), 1, ZX_OK}}});
  EXPECT_EQ(ZX_OK, ret);
  EXPECT_EQ(3U, c.koid_to_process().size());
  EXPECT_EQ(2U, c.koid_to_vmo().size());
  const auto& process = c.process_for_koid(proc_koid);
  EXPECT_EQ(proc_koid, process.koid);
  EXPECT_STREQ(proc_name, process.name);
  EXPECT_EQ(1U, process.vmos.size());
  EXPECT_EQ(vmo_koid, process.vmos[0]);
  const auto& vmo = c.vmo_for_koid(vmo_koid);
  EXPECT_EQ(vmo_koid, vmo.koid);
  EXPECT_STREQ(vmo_name, vmo.name);

  const auto& process2 = c.process_for_koid(proc2_koid);
  EXPECT_EQ(proc2_koid, process2.koid);
  EXPECT_STREQ(proc2_name, process2.name);
  EXPECT_EQ(1U, process2.vmos.size());
  EXPECT_EQ(vmo2_koid, process2.vmos[0]);
  const auto& vmo2 = c.vmo_for_koid(vmo2_koid);
  EXPECT_EQ(vmo2_koid, vmo2.koid);
  EXPECT_STREQ(vmo2_name, vmo2.name);

  const auto& process3 = c.process_for_koid(proc3_koid);
  EXPECT_EQ(proc3_koid, process3.koid);
  EXPECT_STREQ(proc3_name, process3.name);
  EXPECT_EQ(1U, process3.vmos.size());
  EXPECT_EQ(vmo_koid, process3.vmos[0]);
}

}  // namespace test
}  // namespace memory
