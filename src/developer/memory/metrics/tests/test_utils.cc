// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/tests/test_utils.h"

#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>
#include <zircon/syscalls/object.h>

#include <gtest/gtest.h>

#include "src/developer/memory/metrics/capture.h"
namespace memory {

const zx_handle_t TestUtils::kRootHandle = 1;
const zx_handle_t TestUtils::kSelfHandle = 2;
const zx_koid_t TestUtils::kSelfKoid = 3;

class MockOS : public OS {
 public:
  explicit MockOS(OsResponses responses)
      : responses_(std::move(responses)), i_get_property_(0), clock_(0) {}

 private:
  zx_status_t GetKernelStats(fidl::WireSyncClient<fuchsia_kernel::Stats>* stats) override {
    return ZX_OK;
  }

  zx_handle_t ProcessSelf() override { return TestUtils::kSelfHandle; }

  zx_time_t GetMonotonic() override { return clock_++; }

  zx_status_t GetProcesses(
      fit::function<zx_status_t(int, zx::handle, zx_koid_t, zx_koid_t)> cb) override {
    const auto& r = responses_.get_processes;
    for (const auto& c : r.callbacks) {
      auto ret = cb(c.depth, zx::handle(c.handle), c.koid, c.parent_koid);
      if (ret != ZX_OK) {
        return ret;
      }
    }
    return r.ret;
  }

  zx_status_t GetProperty(zx_handle_t handle, uint32_t property, void* value,
                          size_t name_len) override {
    const auto& r = responses_.get_property.at(i_get_property_++);
    EXPECT_EQ(r.handle, handle);
    EXPECT_EQ(r.property, property);
    auto len = std::min(name_len, r.value_len);
    memcpy(value, r.value, len);
    return r.ret;
  }

  const GetInfoResponse* GetGetInfoResponse(zx_handle_t handle, uint32_t topic) {
    for (const auto& resp : responses_.get_info) {
      if (resp.handle == handle && resp.topic == topic) {
        return &resp;
      }
    }
    EXPECT_TRUE(false) << "This should not be reached: handle " << handle << " topic " << topic;
    return nullptr;
  }

  zx_status_t GetInfo(zx_handle_t handle, uint32_t topic, void* buffer, size_t buffer_size,
                      size_t* actual, size_t* avail) override {
    const GetInfoResponse* r = GetGetInfoResponse(handle, topic);
    if (r == nullptr) {
      return ZX_ERR_INVALID_ARGS;
    }
    EXPECT_EQ(r->handle, handle);
    EXPECT_EQ(r->topic, topic);
    size_t num_copied = 0;
    if (buffer != nullptr) {
      num_copied = std::min(r->value_count, buffer_size / r->value_size);
      memcpy(buffer, r->values, num_copied * r->value_size);
    }
    if (actual != nullptr) {
      *actual = num_copied;
    }
    if (avail != nullptr) {
      if (num_copied < r->value_count) {
        *avail = r->value_count - num_copied;
      } else {
        *avail = 0;
      }
    }
    return r->ret;
  }

  zx_status_t GetKernelMemoryStats(const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
                                   zx_info_kmem_stats_t* kmem) override {
    const GetInfoResponse* r =
        GetGetInfoResponse(TestUtils::kRootHandle, ZX_INFO_KMEM_STATS_EXTENDED);
    if (r == nullptr)
      return ZX_ERR_INVALID_ARGS;
    memcpy(kmem, r->values, r->value_size);
    return r->ret;
  }

  zx_status_t GetKernelMemoryStatsExtended(
      const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
      zx_info_kmem_stats_extended_t* kmem_ext, zx_info_kmem_stats_t* kmem) override {
    const GetInfoResponse* r1 =
        GetGetInfoResponse(TestUtils::kRootHandle, ZX_INFO_KMEM_STATS_EXTENDED);
    if (r1 == nullptr)
      return ZX_ERR_INVALID_ARGS;
    memcpy(kmem, r1->values, r1->value_size);
    const GetInfoResponse* r2 =
        GetGetInfoResponse(TestUtils::kRootHandle, ZX_INFO_KMEM_STATS_EXTENDED);
    if (r2 == nullptr)
      return ZX_ERR_INVALID_ARGS;
    memcpy(kmem_ext, r2->values, r2->value_size);
    return r2->ret;
  }

  OsResponses responses_;
  uint32_t i_get_property_;
  zx_time_t clock_;
};

// static.
void TestUtils::CreateCapture(Capture* capture, const CaptureTemplate& t, CaptureLevel level) {
  capture->time_ = t.time;
  capture->kmem_ = t.kmem;
  if (level == CaptureLevel::KMEM) {
    return;
  }
  capture->kmem_extended_ = t.kmem_extended;
  if (level != CaptureLevel::VMO) {
    return;
  }
  for (const auto& vmo : t.vmos) {
    capture->koid_to_vmo_.emplace(vmo.koid, vmo);
  }
  for (const auto& process : t.processes) {
    capture->koid_to_process_.emplace(process.koid, process);
  }
  capture->ReallocateDescendents(t.rooted_vmo_names);
}

// static.
std::vector<ProcessSummary> TestUtils::GetProcessSummaries(const Summary& summary) {
  std::vector<ProcessSummary> summaries = summary.process_summaries();
  sort(summaries.begin(), summaries.end(),
       [](ProcessSummary a, ProcessSummary b) { return a.koid() < b.koid(); });
  return summaries;
}

zx_status_t TestUtils::GetCapture(Capture* capture, CaptureLevel level, const OsResponses& r,
                                  CaptureFilter* filter) {
  MockOS os(r);
  CaptureState state;
  zx_status_t ret = Capture::GetCaptureState(&state, &os);
  EXPECT_EQ(ZX_OK, ret);
  return Capture::GetCapture(capture, state, level, filter, &os, Capture::kDefaultRootedVmoNames);
}

zx_status_t CaptureSupplier::GetCapture(Capture* capture, CaptureLevel level,
                                        bool use_capture_supplier_time) {
  auto& t = templates_.at(index_);
  if (!use_capture_supplier_time) {
    t.time = index_;
  }
  index_++;
  TestUtils::CreateCapture(capture, t, level);
  return ZX_OK;
}

}  // namespace memory
