// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fxt/fields.h>
#include <lib/standalone-test/standalone.h>
#include <lib/zx/event.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>
#include <zircon/limits.h>
#include <zircon/rights.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
#include <zircon/system/public/zircon/process.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <thread>

#include <runtime/thread.h>
#include <zxtest/zxtest.h>

#include "zircon/system/public/zircon/syscalls/iob.h"
#include "zircon/third_party/ulib/musl/include/zircon/threads.h"

#ifdef EXPERIMENTAL_THREAD_SAMPLER_ENABLED
constexpr bool sampler_enabled = EXPERIMENTAL_THREAD_SAMPLER_ENABLED;
#else
constexpr bool sampler_enabled = false;
#endif

namespace {

void TestFn(zx::unowned_event event) {
  event->signal(0u, ZX_USER_SIGNAL_0);
  for (;;) {
    zx_status_t wait_result = event->wait_one(ZX_USER_SIGNAL_1, zx::time::infinite_past(), nullptr);
    if (wait_result != ZX_ERR_TIMED_OUT) {
      break;
    }
  }
}

TEST(ThreadSampler, StartStop) {
  // Start the thread sampler on a thread, wait for some time while taking samples, check to see
  // that samples were written.
  size_t buffer_size = ZX_PAGE_SIZE;
  zx_sampler_config_t config{
      .period = zx::msec(1).get(),
      .buffer_size = buffer_size,
  };
  zx::iob sampler;
  zx_status_t create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                             sampler.reset_and_get_address());
  if constexpr (!sampler_enabled) {
    ASSERT_EQ(create_res, ZX_ERR_NOT_SUPPORTED);
    return;
  }

  ASSERT_OK(create_res);

  zx::event event;
  ASSERT_EQ(zx::event::create(0, &event), ZX_OK);

  // Create a thread
  std::thread sample_thread{TestFn, event.borrow()};
  zx_handle_t native_handle = native_thread_get_zx_handle(sample_thread.native_handle());

  ASSERT_OK(event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), nullptr));
  ASSERT_OK(zx_sampler_attach(sampler.get(), native_handle));
  ASSERT_OK(zx_sampler_start(sampler.get()));
  zx::nanosleep(zx::deadline_after(zx::sec(1)));
  ASSERT_OK(zx_sampler_stop(sampler.get()));
  ASSERT_OK(event.signal(0, ZX_USER_SIGNAL_1));
  sample_thread.join();

  zx_info_iob_t info;
  ASSERT_OK(sampler.get_info(ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr));
  size_t record_count{0};
  for (uint32_t i = 0; i < info.region_count; i++) {
    zx_vaddr_t addr;
    size_t offset = 0;
    ASSERT_OK(
        zx::vmar::root_self()->map_iob(ZX_VM_PERM_READ, 0, sampler, i, 0, buffer_size, &addr));
    while (offset < buffer_size) {
      uint64_t header = *reinterpret_cast<uint64_t*>(addr + offset);
      if (header == 0) {
        break;
      }
      record_count += 1;
      size_t record_words = fxt::RecordFields::RecordSize::Get<size_t>(header);
      ASSERT_TRUE(record_words > 0);
      offset += record_words * 8;
    }
    zx::vmar::root_self()->unmap(addr, buffer_size);
  }
  ASSERT_GE(record_count, 10);
}

TEST(ThreadSampler, SamplerLifetime) {
  // Once a sampler is created, another sampler should not be able to be created until the returned
  // buffer is release
  size_t buffer_size = ZX_PAGE_SIZE;
  zx_sampler_config_t config{
      .period = zx::msec(1).get(),
      .buffer_size = buffer_size,
  };
  {
    zx::iob buffers;
    zx_status_t create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                               buffers.reset_and_get_address());
    if constexpr (!sampler_enabled) {
      ASSERT_EQ(create_res, ZX_ERR_NOT_SUPPORTED);
      return;
    }
    ASSERT_OK(create_res);

    zx::iob new_buffers;
    zx_status_t create_res_bad = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                                   new_buffers.reset_and_get_address());

    EXPECT_EQ(create_res_bad, ZX_ERR_ALREADY_EXISTS);
  }

  // Once the buffer is released, a new sampler can now be created
  zx::iob buffers;
  ASSERT_OK(zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                              buffers.reset_and_get_address()));
}

TEST(ThreadSampler, DroppedSampler) {
  // Ensure we clean up and can create a new sampler if we drop the old one mid session
  size_t buffer_size = ZX_PAGE_SIZE;
  zx_sampler_config_t config{
      .period = zx::msec(1).get(),
      .buffer_size = buffer_size,
  };
  zx::iob sampler;
  zx_status_t create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                             sampler.reset_and_get_address());
  if constexpr (!sampler_enabled) {
    ASSERT_EQ(create_res, ZX_ERR_NOT_SUPPORTED);
    return;
  }

  ASSERT_OK(create_res);

  zx::event event;
  ASSERT_EQ(zx::event::create(0, &event), ZX_OK);

  // Create a thread
  std::thread sample_thread{TestFn, event.borrow()};
  zx_handle_t native_handle = native_thread_get_zx_handle(sample_thread.native_handle());

  ASSERT_OK(event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), nullptr));
  ASSERT_OK(zx_sampler_attach(sampler.get(), native_handle));
  ASSERT_OK(zx_sampler_start(sampler.get()));

  // Drop the sampler mid session
  sampler.reset();

  // And create a new one
  create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                 sampler.reset_and_get_address());
  ASSERT_OK(create_res);
  ASSERT_OK(zx_sampler_attach(sampler.get(), native_handle));
  ASSERT_OK(zx_sampler_start(sampler.get()));
  zx::nanosleep(zx::deadline_after(zx::sec(1)));
  ASSERT_OK(zx_sampler_stop(sampler.get()));

  ASSERT_OK(event.signal(0, ZX_USER_SIGNAL_1));
  sample_thread.join();

  zx_info_iob_t info;
  ASSERT_OK(sampler.get_info(ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr));
  size_t record_count{0};
  for (uint32_t i = 0; i < info.region_count; i++) {
    zx_vaddr_t addr;
    size_t offset = 0;
    ASSERT_OK(
        zx::vmar::root_self()->map_iob(ZX_VM_PERM_READ, 0, sampler, i, 0, buffer_size, &addr));
    while (offset < buffer_size) {
      uint64_t header = *reinterpret_cast<uint64_t*>(addr + offset);
      if (header == 0) {
        break;
      }
      record_count += 1;
      size_t record_words = fxt::RecordFields::RecordSize::Get<size_t>(header);
      ASSERT_TRUE(record_words > 0);
      offset += record_words * 8;
    }
    zx::vmar::root_self()->unmap(addr, buffer_size);
  }
  ASSERT_GE(record_count, 10);
}

TEST(ThreadSampler, BadIob) {
  // We should not be able to pass in any arbitrary iob
  size_t buffer_size = ZX_PAGE_SIZE;
  zx_sampler_config_t config{
      .period = zx::msec(1).get(),
      .buffer_size = buffer_size,
  };
  zx::iob sampler;
  zx_status_t create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                             sampler.reset_and_get_address());
  if constexpr (!sampler_enabled) {
    ASSERT_EQ(create_res, ZX_ERR_NOT_SUPPORTED);
    return;
  }
  ASSERT_OK(create_res);

  const uint64_t kIoBufferEpRwMap = ZX_IOB_EP0_CAN_MAP_READ | ZX_IOB_EP0_CAN_MAP_WRITE |
                                    ZX_IOB_EP1_CAN_MAP_READ | ZX_IOB_EP1_CAN_MAP_WRITE;
  zx_iob_region_t iob_config{
      .type = ZX_IOB_REGION_TYPE_PRIVATE,
      .access = kIoBufferEpRwMap,
      .size = ZX_PAGE_SIZE,
      .discipline = zx_iob_discipline_t{.type = ZX_IOB_DISCIPLINE_TYPE_NONE},
      .private_region =
          {
              .options = 0,
          },
  };
  zx_handle_t ep0, ep1;
  EXPECT_OK(zx_iob_create(0, &iob_config, 1, &ep0, &ep1));

  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_start(ep0));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_start(ep1));
  EXPECT_OK(zx_sampler_start(sampler.get()));

  zx::event event;
  ASSERT_EQ(zx::event::create(0, &event), ZX_OK);
  std::thread sample_thread{TestFn, event.borrow()};
  zx_handle_t native_handle = native_thread_get_zx_handle(sample_thread.native_handle());

  ASSERT_OK(event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), nullptr));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_attach(ep0, native_handle));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_attach(ep1, native_handle));

  ASSERT_OK(zx_sampler_attach(sampler.get(), native_handle));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_stop(ep0));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_stop(ep1));
  EXPECT_OK(zx_sampler_stop(sampler.get()));
  ASSERT_OK(event.signal(0, ZX_USER_SIGNAL_1));
  sample_thread.join();
}

TEST(ThreadSampler, NoRights) {
  // We require ZX_RIGHT_APPLY_PROFILE on the returned iob in order to control sampling.
  // If a handle lacks the rights, it should be denied access.
  size_t buffer_size = ZX_PAGE_SIZE;
  zx_sampler_config_t config{
      .period = zx::msec(1).get(),
      .buffer_size = buffer_size,
  };
  zx::iob sampler;
  zx_status_t create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                             sampler.reset_and_get_address());
  if constexpr (!sampler_enabled) {
    ASSERT_EQ(create_res, ZX_ERR_NOT_SUPPORTED);
    return;
  }
  ASSERT_OK(create_res);

  zx::iob no_right_sampler;
  ASSERT_OK(sampler.duplicate(ZX_RIGHT_NONE, &no_right_sampler));

  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_start(no_right_sampler.get()));
  EXPECT_OK(zx_sampler_start(sampler.get()));

  zx::event event;
  ASSERT_EQ(zx::event::create(0, &event), ZX_OK);
  std::thread sample_thread{TestFn, event.borrow()};
  zx_handle_t native_handle = native_thread_get_zx_handle(sample_thread.native_handle());

  ASSERT_OK(event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), nullptr));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_attach(no_right_sampler.get(), native_handle));

  ASSERT_OK(zx_sampler_attach(sampler.get(), native_handle));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, zx_sampler_stop(no_right_sampler.get()));
  EXPECT_OK(zx_sampler_stop(sampler.get()));
  ASSERT_OK(event.signal(0, ZX_USER_SIGNAL_1));
  sample_thread.join();
}

TEST(ThreadSampler, ClosedHandleReadBuffers) {
  // Even after we close the handle, buffers we mapped from the iob should still be readable
  size_t buffer_size = ZX_PAGE_SIZE;
  zx_sampler_config_t config{
      .period = zx::msec(1).get(),
      .buffer_size = buffer_size,
  };
  zx::iob sampler;
  zx_status_t create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                             sampler.reset_and_get_address());
  if constexpr (!sampler_enabled) {
    ASSERT_EQ(create_res, ZX_ERR_NOT_SUPPORTED);
    return;
  }

  ASSERT_OK(create_res);

  zx::event event;
  ASSERT_EQ(zx::event::create(0, &event), ZX_OK);

  // Create a thread
  std::thread sample_thread{TestFn, event.borrow()};
  zx_handle_t native_handle = native_thread_get_zx_handle(sample_thread.native_handle());

  ASSERT_OK(event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), nullptr));
  ASSERT_OK(zx_sampler_attach(sampler.get(), native_handle));
  ASSERT_OK(zx_sampler_start(sampler.get()));
  zx::nanosleep(zx::deadline_after(zx::sec(1)));
  ASSERT_OK(zx_sampler_stop(sampler.get()));
  ASSERT_OK(event.signal(0, ZX_USER_SIGNAL_1));
  sample_thread.join();

  zx_info_iob_t info;
  ASSERT_OK(sampler.get_info(ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr));
  size_t record_count{0};
  zx_vaddr_t addrs[info.region_count];
  // Map the iob regions to hold onto our references
  for (uint32_t i = 0; i < info.region_count; i++) {
    ASSERT_OK(
        zx::vmar::root_self()->map_iob(ZX_VM_PERM_READ, 0, sampler, i, 0, buffer_size, &addrs[i]));
  }

  // Reset our sampler
  sampler.reset();
  create_res = zx_sampler_create(standalone::GetRootResource()->get(), 0, &config,
                                 sampler.reset_and_get_address());
  ASSERT_OK(create_res);
  ASSERT_OK(zx_sampler_start(sampler.get()));
  ASSERT_OK(zx_sampler_stop(sampler.get()));

  // The buffers we mapped should still be readable even after creating and using a new sampler
  for (uint32_t i = 0; i < info.region_count; i++) {
    size_t offset = 0;
    while (offset < buffer_size) {
      uint64_t header = *reinterpret_cast<uint64_t*>(addrs[i] + offset);
      if (header == 0) {
        break;
      }
      record_count += 1;
      size_t record_words = fxt::RecordFields::RecordSize::Get<size_t>(header);
      ASSERT_TRUE(record_words > 0);
      offset += record_words * 8;
    }
    zx::vmar::root_self()->unmap(addrs[i], buffer_size);
  }
  ASSERT_GE(record_count, 10);
}

}  // namespace
