// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/paged_vmo.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/time.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/async/irq.h>
#include <lib/async/receiver.h>
#include <lib/async/task.h>
#include <lib/async/time.h>
#include <lib/async/wait.h>
#include <lib/fit/function.h>
#include <lib/zx/clock.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/pager.h>
#include <limits.h>
#include <threads.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>

#include <algorithm>
#include <atomic>
#include <random>
#include <thread>
#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

namespace {

class TestWait : public async_wait_t {
 public:
  TestWait(zx_handle_t object, zx_signals_t trigger, uint32_t options = 0)
      : async_wait_t{{ASYNC_STATE_INIT}, &TestWait::CallHandler, object, trigger, options} {}

  virtual ~TestWait() = default;

  uint32_t run_count = 0u;
  zx_status_t last_status = ZX_ERR_INTERNAL;
  const zx_packet_signal_t* last_signal = nullptr;

  virtual zx_status_t Begin(async_dispatcher_t* dispatcher) {
    return async_begin_wait(dispatcher, this);
  }

  zx_status_t Cancel(async_dispatcher_t* dispatcher) { return async_cancel_wait(dispatcher, this); }

 protected:
  virtual void Handle(async_dispatcher_t* dispatcher, zx_status_t status,
                      const zx_packet_signal_t* signal) {
    run_count++;
    last_status = status;
    if (signal) {
      last_signal_storage_ = *signal;
      last_signal = &last_signal_storage_;
    } else {
      last_signal = nullptr;
    }
  }

 private:
  static void CallHandler(async_dispatcher_t* dispatcher, async_wait_t* wait, zx_status_t status,
                          const zx_packet_signal_t* signal) {
    static_cast<TestWait*>(wait)->Handle(dispatcher, status, signal);
  }

  zx_packet_signal_t last_signal_storage_;
};

class TestWaitIrq : public async_irq_t {
 public:
  TestWaitIrq(zx_handle_t irq) : async_irq_t{{ASYNC_STATE_INIT}, &TestWaitIrq::CallHandler, irq} {}

  virtual ~TestWaitIrq() = default;

  uint32_t run_count = 0u;
  zx_status_t last_status = ZX_ERR_INTERNAL;
  const zx_packet_interrupt_t* last_signal = nullptr;

  virtual zx_status_t Begin(async_dispatcher_t* dispatcher) {
    return async_bind_irq(dispatcher, this);
  }

  zx_status_t Cancel(async_dispatcher_t* dispatcher) { return async_unbind_irq(dispatcher, this); }

 protected:
  virtual void Handle(async_dispatcher_t* dispatcher, zx_status_t status,
                      const zx_packet_interrupt_t* signal) {
    run_count++;
    last_status = status;
    if (signal) {
      last_signal_storage_ = *signal;
      last_signal = &last_signal_storage_;
    } else {
      last_signal = nullptr;
    }
  }

 private:
  static void CallHandler(async_dispatcher_t* dispatcher, async_irq_t* wait, zx_status_t status,
                          const zx_packet_interrupt_t* signal) {
    static_cast<TestWaitIrq*>(wait)->Handle(dispatcher, status, signal);
  }

  zx_packet_interrupt_t last_signal_storage_;
};

class CascadeWait : public TestWait {
 public:
  CascadeWait(zx_handle_t object, zx_signals_t trigger, zx_signals_t signals_to_clear,
              zx_signals_t signals_to_set, bool repeat)
      : TestWait(object, trigger),
        signals_to_clear_(signals_to_clear),
        signals_to_set_(signals_to_set),
        repeat_(repeat) {}

 protected:
  zx_signals_t signals_to_clear_;
  zx_signals_t signals_to_set_;
  bool repeat_;

  void Handle(async_dispatcher_t* dispatcher, zx_status_t status,
              const zx_packet_signal_t* signal) override {
    TestWait::Handle(dispatcher, status, signal);
    zx_object_signal(object, signals_to_clear_, signals_to_set_);
    if (repeat_ && status == ZX_OK) {
      Begin(dispatcher);
    }
  }
};

class SelfCancelingWait : public TestWait {
 public:
  SelfCancelingWait(zx_handle_t object, zx_signals_t trigger) : TestWait(object, trigger) {}

  zx_status_t cancel_result = ZX_ERR_INTERNAL;

 protected:
  void Handle(async_dispatcher_t* dispatcher, zx_status_t status,
              const zx_packet_signal_t* signal) override {
    TestWait::Handle(dispatcher, status, signal);
    cancel_result = Cancel(dispatcher);
  }
};

class TestTask : public async_task_t {
 public:
  TestTask() : async_task_t{{ASYNC_STATE_INIT}, &TestTask::CallHandler, ZX_TIME_INFINITE} {}

  virtual ~TestTask() = default;

  zx_status_t Post(async_dispatcher_t* dispatcher) {
    this->deadline = async_now(dispatcher);
    return async_post_task(dispatcher, this);
  }

  zx_status_t PostForTime(async_dispatcher_t* dispatcher, zx::time deadline) {
    this->deadline = deadline.get();
    return async_post_task(dispatcher, this);
  }

  zx_status_t Cancel(async_dispatcher_t* dispatcher) { return async_cancel_task(dispatcher, this); }

  uint32_t run_count = 0u;
  zx_status_t last_status = ZX_ERR_INTERNAL;

 protected:
  virtual void Handle(async_dispatcher_t* dispatcher, zx_status_t status) {
    run_count++;
    last_status = status;
  }

 private:
  static void CallHandler(async_dispatcher_t* dispatcher, async_task_t* task, zx_status_t status) {
    static_cast<TestTask*>(task)->Handle(dispatcher, status);
  }
};

class QuitTask : public TestTask {
 public:
  QuitTask() = default;

 protected:
  void Handle(async_dispatcher_t* dispatcher, zx_status_t status) override {
    TestTask::Handle(dispatcher, status);
    async_loop_quit(async_loop_from_dispatcher(dispatcher));
  }
};

class ResetQuitTask : public TestTask {
 public:
  ResetQuitTask() = default;

  zx_status_t result = ZX_ERR_INTERNAL;

 protected:
  void Handle(async_dispatcher_t* dispatcher, zx_status_t status) override {
    TestTask::Handle(dispatcher, status);
    result = async_loop_reset_quit(async_loop_from_dispatcher(dispatcher));
  }
};

class RepeatingTask : public TestTask {
 public:
  RepeatingTask(zx::duration interval, uint32_t repeat_count)
      : interval_(interval), repeat_count_(repeat_count) {}

  void set_finish_callback(fit::closure callback) { finish_callback_ = std::move(callback); }

 protected:
  zx::duration interval_;
  uint32_t repeat_count_;
  fit::closure finish_callback_;

  void Handle(async_dispatcher_t* dispatcher, zx_status_t status) override {
    TestTask::Handle(dispatcher, status);
    if (repeat_count_ == 0) {
      if (finish_callback_)
        finish_callback_();
    } else {
      repeat_count_ -= 1;
      if (status == ZX_OK) {
        deadline = zx_time_add_duration(deadline, interval_.get());
        Post(dispatcher);
      }
    }
  }
};

class SelfCancelingTask : public TestTask {
 public:
  SelfCancelingTask() = default;

  zx_status_t cancel_result = ZX_ERR_INTERNAL;

 protected:
  void Handle(async_dispatcher_t* dispatcher, zx_status_t status) override {
    TestTask::Handle(dispatcher, status);
    cancel_result = Cancel(dispatcher);
  }
};

class TestReceiver : async_receiver_t {
 public:
  TestReceiver() : async_receiver_t{{ASYNC_STATE_INIT}, &TestReceiver::CallHandler} {}

  virtual ~TestReceiver() = default;

  zx_status_t QueuePacket(async_dispatcher_t* dispatcher, const zx_packet_user_t* data) {
    return async_queue_packet(dispatcher, this, data);
  }

  uint32_t run_count = 0u;
  zx_status_t last_status = ZX_ERR_INTERNAL;
  const zx_packet_user_t* last_data;

 protected:
  virtual void Handle(async_dispatcher_t* dispatcher, zx_status_t status,
                      const zx_packet_user_t* data) {
    run_count++;
    last_status = status;
    if (data) {
      last_data_storage_ = *data;
      last_data = &last_data_storage_;
    } else {
      last_data = nullptr;
    }
  }

 private:
  static void CallHandler(async_dispatcher_t* dispatcher, async_receiver_t* receiver,
                          zx_status_t status, const zx_packet_user_t* data) {
    static_cast<TestReceiver*>(receiver)->Handle(dispatcher, status, data);
  }

  zx_packet_user_t last_data_storage_{};
};

class TestPagedVmo : public async_paged_vmo_t {
 public:
  TestPagedVmo()
      : async_paged_vmo_t{
            {ASYNC_STATE_INIT}, &TestPagedVmo::CallHandler, ZX_HANDLE_INVALID, ZX_HANDLE_INVALID} {}

  zx_status_t Create(async_dispatcher_t* dispatcher, const zx::pager& pager, zx::vmo* vmo_out) {
    zx_status_t status = async_create_paged_vmo(dispatcher, this, 0, pager.get(), PAGE_SIZE,
                                                vmo_out->reset_and_get_address());
    this->pager = pager.get();
    this->vmo = vmo_out->get();
    this->dispatcher_ = dispatcher;
    return status;
  }

  zx_status_t Detach() { return async_detach_paged_vmo(dispatcher_, this); }

  bool IsCanceled() { return canceled_; }

 private:
  static void CallHandler(async_dispatcher_t* dispatcher, async_paged_vmo_t* paged_vmo,
                          zx_status_t status, const zx_packet_page_request_t* page_request) {
    if (status == ZX_ERR_CANCELED) {
      static_cast<TestPagedVmo*>(paged_vmo)->canceled_ = true;
    }
  }

  async_dispatcher_t* dispatcher_;
  bool canceled_ = false;
};

// The C++ loop wrapper is one-to-one with the underlying C API so for the
// most part we will test through that interface but here we make sure that
// the C API actually exists but we don't comprehensively test what it does.
TEST(Loop, CApiBasic) {
  async_loop_t* loop;
  ASSERT_EQ(ZX_OK, async_loop_create(&kAsyncLoopConfigNoAttachToCurrentThread, &loop), "create");
  ASSERT_NE(loop, nullptr, "loop");

  EXPECT_EQ(ASYNC_LOOP_RUNNABLE, async_loop_get_state(loop), "runnable");

  async_loop_quit(loop);
  EXPECT_EQ(ASYNC_LOOP_QUIT, async_loop_get_state(loop), "quitting");
  async_loop_run(loop, ZX_TIME_INFINITE, false);
  EXPECT_EQ(ZX_OK, async_loop_reset_quit(loop));

  thrd_t thread{};
  EXPECT_EQ(ZX_OK, async_loop_start_thread(loop, "name", &thread), "thread start");
  EXPECT_NE(thrd_t{}, thread, "thread ws initialized");
  async_loop_quit(loop);
  async_loop_join_threads(loop);

  async_loop_shutdown(loop);
  EXPECT_EQ(ASYNC_LOOP_SHUTDOWN, async_loop_get_state(loop), "shutdown");

  async_loop_destroy(loop);
}

TEST(Loop, MakeDefaultFalse) {
  {
    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    EXPECT_NULL(async_get_default_dispatcher(), "not default");
  }
  EXPECT_NULL(async_get_default_dispatcher(), "still not default");
}

// Static data and methods for use in make_default_true_test()
async_dispatcher_t* test_default_dispatcher;

void set_test_default_dispatcher(async_dispatcher_t* dispatcher) {
  test_default_dispatcher = dispatcher;
}

async_dispatcher_t* get_test_default_dispatcher() { return test_default_dispatcher; }

TEST(Loop, MakeDefaultTrue) {
  async_loop_config_t config{};

  config.make_default_for_current_thread = true;
  config.default_accessors.getter = get_test_default_dispatcher;
  config.default_accessors.setter = set_test_default_dispatcher;

  {
    async::Loop loop(&config);
    EXPECT_EQ(loop.dispatcher(), get_test_default_dispatcher(), "became default");
  }
  EXPECT_NULL(get_test_default_dispatcher(), "no longer default");
}

TEST(Loop, CreateDefault) {
  {
    async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
    EXPECT_EQ(loop.dispatcher(), async_get_default_dispatcher(), "became default");
  }
  EXPECT_NULL(async_get_default_dispatcher(), "no longer default");
}

TEST(Loop, Quit) {
  for (int i = 0; i < 3; i++) {
    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    EXPECT_EQ(ASYNC_LOOP_RUNNABLE, loop.GetState(), "initially not quitting");

    loop.Quit();
    EXPECT_EQ(ASYNC_LOOP_QUIT, loop.GetState(), "quitting when quit");
    EXPECT_EQ(ZX_ERR_CANCELED, loop.Run(), "run returns immediately");
    EXPECT_EQ(ASYNC_LOOP_QUIT, loop.GetState(), "still quitting");

    ResetQuitTask reset_quit_task;
    EXPECT_EQ(ZX_OK, reset_quit_task.Post(loop.dispatcher()), "can post tasks even after quit");
    QuitTask quit_task;
    EXPECT_EQ(ZX_OK, quit_task.Post(loop.dispatcher()), "can post tasks even after quit");

    EXPECT_EQ(ZX_OK, loop.ResetQuit());
    EXPECT_EQ(ASYNC_LOOP_RUNNABLE, loop.GetState(), "not quitting after reset");

    EXPECT_EQ(ZX_OK, loop.Run(zx::time::infinite(), true /*once*/), "run tasks");

    EXPECT_EQ(1u, reset_quit_task.run_count, "reset quit task ran");
    EXPECT_EQ(ZX_ERR_BAD_STATE, reset_quit_task.result, "can't reset quit while loop is running");

    EXPECT_EQ(1u, quit_task.run_count, "quit task ran");
    EXPECT_EQ(ASYNC_LOOP_QUIT, loop.GetState(), "quitted");

    EXPECT_EQ(ZX_ERR_CANCELED, loop.Run(), "runs returns immediately when quitted");

    loop.Shutdown();
    EXPECT_EQ(ASYNC_LOOP_SHUTDOWN, loop.GetState(), "shut down");
    EXPECT_EQ(ZX_ERR_BAD_STATE, loop.Run(), "run returns immediately when shut down");
    EXPECT_EQ(ZX_ERR_BAD_STATE, loop.ResetQuit());
  }
}

TEST(Loop, Time) {
  // Verify that the dispatcher's time-telling is strictly monotonic,
  // which is constent with ZX_CLOCK_MONOTONIC.
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  zx::time t0 = zx::clock::get_monotonic();
  zx::time t1 = async::Now(loop.dispatcher());
  zx::time t2 = async::Now(loop.dispatcher());
  zx::time t3 = zx::clock::get_monotonic();

  EXPECT_LE(t0.get(), t1.get());
  EXPECT_LE(t1.get(), t2.get());
  EXPECT_LE(t2.get(), t3.get());
}

TEST(Loop, Wait) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  zx::event event;
  EXPECT_EQ(ZX_OK, zx::event::create(0u, &event), "create event");

  CascadeWait wait1(event.get(), ZX_USER_SIGNAL_1, 0u, ZX_USER_SIGNAL_2, false);
  CascadeWait wait2(event.get(), ZX_USER_SIGNAL_2, ZX_USER_SIGNAL_1 | ZX_USER_SIGNAL_2, 0u, true);
  CascadeWait wait3(event.get(), ZX_USER_SIGNAL_3, ZX_USER_SIGNAL_3, 0u, true);
  EXPECT_EQ(ZX_OK, wait1.Begin(loop.dispatcher()), "wait 1");
  EXPECT_EQ(ZX_OK, wait2.Begin(loop.dispatcher()), "wait 2");
  EXPECT_EQ(ZX_OK, wait3.Begin(loop.dispatcher()), "wait 3");

  // Initially nothing is signaled.
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(0u, wait1.run_count, "run count 1");
  EXPECT_EQ(0u, wait2.run_count, "run count 2");
  EXPECT_EQ(0u, wait3.run_count, "run count 3");

  // Set signal 1: notifies |wait1| which sets signal 2 and notifies |wait2|
  // which clears signal 1 and 2 again.
  EXPECT_EQ(ZX_OK, event.signal(0u, ZX_USER_SIGNAL_1), "signal 1");
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(1u, wait1.run_count, "run count 1");
  EXPECT_EQ(ZX_OK, wait1.last_status, "status 1");
  EXPECT_NE(wait1.last_signal, nullptr);
  EXPECT_EQ(ZX_USER_SIGNAL_1, wait1.last_signal->trigger & ZX_USER_SIGNAL_ALL, "trigger 1");
  EXPECT_EQ(ZX_USER_SIGNAL_1, wait1.last_signal->observed & ZX_USER_SIGNAL_ALL, "observed 1");
  EXPECT_EQ(1u, wait1.last_signal->count, "count 1");
  EXPECT_EQ(1u, wait2.run_count, "run count 2");
  EXPECT_EQ(ZX_OK, wait2.last_status, "status 2");
  EXPECT_NE(wait2.last_signal, nullptr);
  EXPECT_EQ(ZX_USER_SIGNAL_2, wait2.last_signal->trigger & ZX_USER_SIGNAL_ALL, "trigger 2");
  EXPECT_EQ(ZX_USER_SIGNAL_1 | ZX_USER_SIGNAL_2, wait2.last_signal->observed & ZX_USER_SIGNAL_ALL,
            "observed 2");
  EXPECT_EQ(1u, wait2.last_signal->count, "count 2");
  EXPECT_EQ(0u, wait3.run_count, "run count 3");

  // Set signal 1 again: does nothing because |wait1| was a one-shot.
  EXPECT_EQ(ZX_OK, event.signal(0u, ZX_USER_SIGNAL_1), "signal 1");
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(1u, wait1.run_count, "run count 1");
  EXPECT_EQ(1u, wait2.run_count, "run count 2");
  EXPECT_EQ(0u, wait3.run_count, "run count 3");

  // Set signal 2 again: notifies |wait2| which clears signal 1 and 2 again.
  EXPECT_EQ(ZX_OK, event.signal(0u, ZX_USER_SIGNAL_2), "signal 2");
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(1u, wait1.run_count, "run count 1");
  EXPECT_EQ(2u, wait2.run_count, "run count 2");
  EXPECT_EQ(ZX_OK, wait2.last_status, "status 2");
  EXPECT_NE(wait2.last_signal, nullptr);
  EXPECT_EQ(ZX_USER_SIGNAL_2, wait2.last_signal->trigger & ZX_USER_SIGNAL_ALL, "trigger 2");
  EXPECT_EQ(ZX_USER_SIGNAL_1 | ZX_USER_SIGNAL_2, wait2.last_signal->observed & ZX_USER_SIGNAL_ALL,
            "observed 2");
  EXPECT_EQ(1u, wait2.last_signal->count, "count 2");
  EXPECT_EQ(0u, wait3.run_count, "run count 3");

  // Set signal 3: notifies |wait3| which clears signal 3.
  // Do this a couple of times.
  for (uint32_t i = 0; i < 3; i++) {
    EXPECT_EQ(ZX_OK, event.signal(0u, ZX_USER_SIGNAL_3), "signal 3");
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
    EXPECT_EQ(1u, wait1.run_count, "run count 1");
    EXPECT_EQ(2u, wait2.run_count, "run count 2");
    EXPECT_EQ(i + 1u, wait3.run_count, "run count 3");
    EXPECT_EQ(ZX_OK, wait3.last_status, "status 3");
    EXPECT_NE(wait3.last_signal, nullptr);
    EXPECT_EQ(ZX_USER_SIGNAL_3, wait3.last_signal->trigger & ZX_USER_SIGNAL_ALL, "trigger 3");
    EXPECT_EQ(ZX_USER_SIGNAL_3, wait3.last_signal->observed & ZX_USER_SIGNAL_ALL, "observed 3");
    EXPECT_EQ(1u, wait3.last_signal->count, "count 3");
  }

  // Cancel wait 3 then set signal 3 again: nothing happens this time.
  EXPECT_EQ(ZX_OK, wait3.Cancel(loop.dispatcher()), "cancel");
  EXPECT_EQ(ZX_OK, event.signal(0u, ZX_USER_SIGNAL_3), "signal 3");
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(1u, wait1.run_count, "run count 1");
  EXPECT_EQ(2u, wait2.run_count, "run count 2");
  EXPECT_EQ(3u, wait3.run_count, "run count 3");

  // Redundant cancel returns an error.
  EXPECT_EQ(ZX_ERR_NOT_FOUND, wait3.Cancel(loop.dispatcher()), "cancel again");
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(1u, wait1.run_count, "run count 1");
  EXPECT_EQ(2u, wait2.run_count, "run count 2");
  EXPECT_EQ(3u, wait3.run_count, "run count 3");

  loop.Shutdown();
}

TEST(Loop, Irq) {
  async_loop_config_t config = kAsyncLoopConfigNoAttachToCurrentThread;
  config.irq_support = true;
  // Ensure that we get the IRQ
  {
    async::Loop loop(&config);
    zx::interrupt irq;
    EXPECT_EQ(ZX_OK, zx::interrupt::create({}, 0, ZX_INTERRUPT_VIRTUAL, &irq));
    TestWaitIrq wait(irq.get());
    EXPECT_EQ(ZX_OK, wait.Begin(loop.dispatcher()));
    irq.trigger(0, zx::time());
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle());
    EXPECT_EQ(1, wait.run_count);
    EXPECT_EQ(ZX_OK, irq.ack());
    wait.Cancel(loop.dispatcher());
  }
  // Ensure that we don't get the IRQ if it wasn't triggered
  {
    async::Loop loop(&config);
    zx::interrupt irq;
    EXPECT_EQ(ZX_OK, zx::interrupt::create({}, 0, ZX_INTERRUPT_VIRTUAL, &irq));
    TestWaitIrq wait(irq.get());
    EXPECT_EQ(ZX_OK, wait.Begin(loop.dispatcher()));
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle());
    EXPECT_EQ(0, wait.run_count);
    wait.Cancel(loop.dispatcher());
  }
  // Ensure that the packet is pulled out of the port on unbind
  {
    async::Loop loop(&config);
    zx::interrupt irq;
    EXPECT_EQ(ZX_OK, zx::interrupt::create({}, 0, ZX_INTERRUPT_VIRTUAL, &irq));
    TestWaitIrq wait(irq.get());
    EXPECT_EQ(ZX_OK, wait.Begin(loop.dispatcher()));
    irq.trigger(0, zx::time());
    EXPECT_EQ(ZX_OK, wait.Cancel(loop.dispatcher()));
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle());
    EXPECT_EQ(0, wait.run_count);
  }
  // Ensure that the interrupt gets unbound from the port on unbind
  {
    async::Loop loop(&config);
    zx::interrupt irq;
    EXPECT_EQ(ZX_OK, zx::interrupt::create({}, 0, ZX_INTERRUPT_VIRTUAL, &irq));
    TestWaitIrq wait(irq.get());
    EXPECT_EQ(ZX_OK, wait.Begin(loop.dispatcher()));
    EXPECT_EQ(ZX_OK, wait.Cancel(loop.dispatcher()));
    irq.trigger(0, zx::time());
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle());
    EXPECT_EQ(0, wait.run_count);
  }
  // Ensure that we get an error on unbind if the interrupt was still pending when the loop shuts
  // down
  {
    zx::interrupt irq;
    EXPECT_EQ(ZX_OK, zx::interrupt::create({}, 0, ZX_INTERRUPT_VIRTUAL, &irq));
    TestWaitIrq wait(irq.get());
    {
      async::Loop loop(&config);
      EXPECT_EQ(ZX_OK, wait.Begin(loop.dispatcher()));
      EXPECT_EQ(ZX_OK, loop.RunUntilIdle());
    }
    EXPECT_EQ(1, wait.run_count);
    EXPECT_EQ(ZX_ERR_CANCELED, wait.last_status);
    EXPECT_EQ(ZX_OK, irq.ack());
  }
}

TEST(Loop, WaitTimestamp) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  // Verify that the timestamp is zero when ZX_WAIT_ASYNC_TIMESTAMP isn't used.
  {
    zx::event event1;
    EXPECT_EQ(ZX_OK, zx::event::create(0u, &event1), "create event 1");

    TestWait wait1(event1.get(), ZX_USER_SIGNAL_1);
    EXPECT_EQ(nullptr, wait1.last_signal);
    EXPECT_EQ(ZX_OK, wait1.Begin(loop.dispatcher()), "wait without options");
    EXPECT_EQ(ZX_OK, event1.signal(0u, ZX_USER_SIGNAL_1), "signal event 1");
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
    EXPECT_NE(nullptr, wait1.last_signal);
    EXPECT_EQ(0u, wait1.last_signal->timestamp);
  }

  // Verify that the timestamp is NOT zero when ZX_WAIT_ASYNC_TIMESTAMP is used.
  {
    zx::event event2;
    EXPECT_EQ(ZX_OK, zx::event::create(0u, &event2), "create event 2");

    TestWait wait2(event2.get(), ZX_USER_SIGNAL_1, ZX_WAIT_ASYNC_TIMESTAMP);
    EXPECT_EQ(ZX_OK, wait2.Begin(loop.dispatcher()), "wait with capture timestamp option");

    EXPECT_EQ(nullptr, wait2.last_signal);
    zx::time before = zx::clock::get_monotonic();
    EXPECT_EQ(ZX_OK, event2.signal(0u, ZX_USER_SIGNAL_1), "signal event 2");
    zx::time after = zx::clock::get_monotonic();
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
    EXPECT_NE(nullptr, wait2.last_signal);
    EXPECT_NE(0u, wait2.last_signal->timestamp);
    EXPECT_TRUE(before <= zx::time(wait2.last_signal->timestamp));
    EXPECT_TRUE(after >= zx::time(wait2.last_signal->timestamp));
  }
}

TEST(Loop, WaitTimestampIntegration) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  // Verify that the timestamp is zero when ZX_WAIT_ASYNC_TIMESTAMP isn't used.
  {
    zx::event event1;
    EXPECT_EQ(ZX_OK, zx::event::create(0u, &event1), "create event 1");

    zx_packet_signal_t last_signal = {};
    EXPECT_EQ(0u, last_signal.timestamp);
    async::Wait wait1(
        event1.get(), ZX_USER_SIGNAL_1, 0,
        [&last_signal](async_dispatcher_t* dispatcher, async::Wait* wait, zx_status_t status,
                       const zx_packet_signal_t* signal) { last_signal = *signal; });
    EXPECT_EQ(ZX_OK, wait1.Begin(loop.dispatcher()), "wait without options");
    EXPECT_EQ(ZX_OK, event1.signal(0u, ZX_USER_SIGNAL_1), "signal event 1");
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
    EXPECT_EQ(0u, last_signal.timestamp);
  }

  // Verify that the timestamp is NOT zero when ZX_WAIT_ASYNC_TIMESTAMP is used.
  {
    zx::event event2;
    EXPECT_EQ(ZX_OK, zx::event::create(0u, &event2), "create event 1");

    zx_packet_signal_t last_signal = {};
    async::Wait wait2(
        event2.get(), ZX_USER_SIGNAL_1, ZX_WAIT_ASYNC_TIMESTAMP,
        [&last_signal](async_dispatcher_t* dispatcher, async::Wait* wait, zx_status_t status,
                       const zx_packet_signal_t* signal) { last_signal = *signal; });
    EXPECT_EQ(ZX_OK, wait2.Begin(loop.dispatcher()), "wait with capture timestamp option");

    EXPECT_EQ(0u, last_signal.timestamp);
    zx::time before = zx::clock::get_monotonic();
    EXPECT_EQ(ZX_OK, event2.signal(0u, ZX_USER_SIGNAL_1), "signal event 2");
    zx::time after = zx::clock::get_monotonic();
    EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
    EXPECT_NE(0u, last_signal.timestamp);
    EXPECT_TRUE(before <= zx::time(last_signal.timestamp));
    EXPECT_TRUE(after >= zx::time(last_signal.timestamp));
  }
}

TEST(Loop, WaitUnwaitableHandle) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  zx::event event;
  EXPECT_EQ(ZX_OK, zx::event::create(0u, &event), "create event");
  event.replace(ZX_RIGHT_NONE, &event);

  TestWait wait(event.get(), ZX_USER_SIGNAL_0);
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, wait.Begin(loop.dispatcher()), "begin");
  EXPECT_EQ(ZX_ERR_NOT_FOUND, wait.Cancel(loop.dispatcher()), "cancel");
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(0u, wait.run_count, "run count");
}

TEST(Loop, WaitShutdown) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  zx::event event;
  EXPECT_EQ(ZX_OK, zx::event::create(0u, &event), "create event");

  CascadeWait wait1(event.get(), ZX_USER_SIGNAL_0, 0u, 0u, false);
  CascadeWait wait2(event.get(), ZX_USER_SIGNAL_0, ZX_USER_SIGNAL_0, 0u, true);
  TestWait wait3(event.get(), ZX_USER_SIGNAL_1);
  SelfCancelingWait wait4(event.get(), ZX_USER_SIGNAL_0);
  SelfCancelingWait wait5(event.get(), ZX_USER_SIGNAL_1);

  EXPECT_EQ(ZX_OK, wait1.Begin(loop.dispatcher()), "begin 1");
  EXPECT_EQ(ZX_OK, wait2.Begin(loop.dispatcher()), "begin 2");
  EXPECT_EQ(ZX_OK, wait3.Begin(loop.dispatcher()), "begin 3");
  EXPECT_EQ(ZX_OK, wait4.Begin(loop.dispatcher()), "begin 4");
  EXPECT_EQ(ZX_OK, wait5.Begin(loop.dispatcher()), "begin 5");

  // Nothing signaled so nothing happens at first.
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(0u, wait1.run_count, "run count 1");
  EXPECT_EQ(0u, wait2.run_count, "run count 2");
  EXPECT_EQ(0u, wait3.run_count, "run count 3");
  EXPECT_EQ(0u, wait4.run_count, "run count 4");
  EXPECT_EQ(0u, wait5.run_count, "run count 5");

  // Set signal 1: notifies both waiters, |wait2| clears the signal and repeats
  EXPECT_EQ(ZX_OK, event.signal(0u, ZX_USER_SIGNAL_0), "signal 1");
  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(1u, wait1.run_count, "run count 1");
  EXPECT_EQ(ZX_OK, wait1.last_status, "status 1");
  EXPECT_NE(wait1.last_signal, nullptr);
  EXPECT_EQ(ZX_USER_SIGNAL_0, wait1.last_signal->trigger & ZX_USER_SIGNAL_ALL, "trigger 1");
  EXPECT_EQ(ZX_USER_SIGNAL_0, wait1.last_signal->observed & ZX_USER_SIGNAL_ALL, "observed 1");
  EXPECT_EQ(1u, wait1.last_signal->count, "count 1");
  EXPECT_EQ(1u, wait2.run_count, "run count 2");
  EXPECT_EQ(ZX_OK, wait2.last_status, "status 2");
  EXPECT_NE(wait2.last_signal, nullptr);
  EXPECT_EQ(ZX_USER_SIGNAL_0, wait2.last_signal->trigger & ZX_USER_SIGNAL_ALL, "trigger 2");
  EXPECT_EQ(ZX_USER_SIGNAL_0, wait2.last_signal->observed & ZX_USER_SIGNAL_ALL, "observed 2");
  EXPECT_EQ(1u, wait2.last_signal->count, "count 2");
  EXPECT_EQ(0u, wait3.run_count, "run count 3");
  EXPECT_EQ(1u, wait4.run_count, "run count 4");
  EXPECT_EQ(ZX_USER_SIGNAL_0, wait4.last_signal->trigger & ZX_USER_SIGNAL_ALL, "trigger 4");
  EXPECT_EQ(ZX_USER_SIGNAL_0, wait4.last_signal->observed & ZX_USER_SIGNAL_ALL, "observed 4");
  EXPECT_EQ(ZX_ERR_NOT_FOUND, wait4.cancel_result, "cancel result 4");
  EXPECT_EQ(0u, wait5.run_count, "run count 5");

  // When the loop shuts down:
  //   |wait1| not notified because it was serviced and didn't repeat
  //   |wait2| notified because it repeated
  //   |wait3| notified because it was not yet serviced
  //   |wait4| not notified because it was serviced
  //   |wait5| notified because it was not yet serviced
  loop.Shutdown();
  EXPECT_EQ(1u, wait1.run_count, "run count 1");
  EXPECT_EQ(2u, wait2.run_count, "run count 2");
  EXPECT_EQ(ZX_ERR_CANCELED, wait2.last_status, "status 2");
  EXPECT_NULL(wait2.last_signal, "signal 2");
  EXPECT_EQ(1u, wait3.run_count, "run count 3");
  EXPECT_EQ(ZX_ERR_CANCELED, wait3.last_status, "status 3");
  EXPECT_NULL(wait3.last_signal, "signal 3");
  EXPECT_EQ(1u, wait4.run_count, "run count 4");
  EXPECT_EQ(1u, wait5.run_count, "run count 5");
  EXPECT_EQ(ZX_ERR_CANCELED, wait5.last_status, "status 5");
  EXPECT_NULL(wait5.last_signal, "signal 5");
  EXPECT_EQ(ZX_ERR_NOT_FOUND, wait5.cancel_result, "cancel result 5");

  // Try to add or cancel work after shutdown.
  TestWait wait6(event.get(), ZX_USER_SIGNAL_0);
  EXPECT_EQ(ZX_ERR_BAD_STATE, wait6.Begin(loop.dispatcher()), "begin after shutdown");
  EXPECT_EQ(ZX_ERR_NOT_FOUND, wait6.Cancel(loop.dispatcher()), "cancel after shutdown");
  EXPECT_EQ(0u, wait6.run_count, "run count 6");
}

TEST(Loop, Task) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::time start_time = async::Now(loop.dispatcher());
  TestTask task1;
  RepeatingTask task2(zx::msec(1), 3u);
  TestTask task3;
  QuitTask task4;
  TestTask task5;  // posted after quit

  EXPECT_EQ(ZX_OK, task1.PostForTime(loop.dispatcher(), start_time + zx::msec(1)), "post 1");
  EXPECT_EQ(ZX_OK, task2.PostForTime(loop.dispatcher(), start_time + zx::msec(1)), "post 2");
  EXPECT_EQ(ZX_OK, task3.PostForTime(loop.dispatcher(), start_time), "post 3");
  task2.set_finish_callback([&loop, &task4, &task5, start_time] {
    task4.PostForTime(loop.dispatcher(), start_time + zx::msec(10));
    task5.PostForTime(loop.dispatcher(), start_time + zx::msec(10));
  });

  // Cancel task 3.
  EXPECT_EQ(ZX_OK, task3.Cancel(loop.dispatcher()), "cancel 3");

  // Run until quit.
  EXPECT_EQ(ZX_ERR_CANCELED, loop.Run(), "run loop");
  EXPECT_EQ(ASYNC_LOOP_QUIT, loop.GetState(), "quitting");
  EXPECT_EQ(1u, task1.run_count, "run count 1");
  EXPECT_EQ(ZX_OK, task1.last_status, "status 1");
  EXPECT_EQ(4u, task2.run_count, "run count 2");
  EXPECT_EQ(ZX_OK, task2.last_status, "status 2");
  EXPECT_EQ(0u, task3.run_count, "run count 3");
  EXPECT_EQ(1u, task4.run_count, "run count 4");
  EXPECT_EQ(ZX_OK, task4.last_status, "status 4");
  EXPECT_EQ(0u, task5.run_count, "run count 5");

  // Reset quit and keep running, now task5 should go ahead followed
  // by any subsequently posted tasks even if they have earlier deadlines.
  QuitTask task6;
  TestTask task7;
  EXPECT_EQ(ZX_OK, task6.PostForTime(loop.dispatcher(), start_time), "post 6");
  EXPECT_EQ(ZX_OK, task7.PostForTime(loop.dispatcher(), start_time), "post 7");
  EXPECT_EQ(ZX_OK, loop.ResetQuit());
  EXPECT_EQ(ZX_ERR_CANCELED, loop.Run(), "run loop");
  EXPECT_EQ(ASYNC_LOOP_QUIT, loop.GetState(), "quitting");

  EXPECT_EQ(1u, task5.run_count, "run count 5");
  EXPECT_EQ(ZX_OK, task5.last_status, "status 5");
  EXPECT_EQ(1u, task6.run_count, "run count 6");
  EXPECT_EQ(ZX_OK, task6.last_status, "status 6");
  EXPECT_EQ(0u, task7.run_count, "run count 7");

  loop.Shutdown();
}

TEST(Loop, TaskShutdown) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::time start_time = async::Now(loop.dispatcher());
  TestTask task1;
  RepeatingTask task2(zx::msec(1000), 1u);
  TestTask task3;
  TestTask task4;
  QuitTask task5;
  SelfCancelingTask task6;
  SelfCancelingTask task7;

  EXPECT_EQ(ZX_OK, task1.PostForTime(loop.dispatcher(), start_time + zx::msec(1)), "post 1");
  EXPECT_EQ(ZX_OK, task2.PostForTime(loop.dispatcher(), start_time + zx::msec(1)), "post 2");
  EXPECT_EQ(ZX_OK, task3.PostForTime(loop.dispatcher(), zx::time::infinite()), "post 3");
  EXPECT_EQ(ZX_OK, task4.PostForTime(loop.dispatcher(), zx::time::infinite()), "post 4");
  EXPECT_EQ(ZX_OK, task5.PostForTime(loop.dispatcher(), start_time + zx::msec(1)), "post 5");
  EXPECT_EQ(ZX_OK, task6.PostForTime(loop.dispatcher(), start_time), "post 6");
  EXPECT_EQ(ZX_OK, task7.PostForTime(loop.dispatcher(), zx::time::infinite()), "post 7");

  // Run tasks which are due up to the time when the quit task runs.
  EXPECT_EQ(ZX_ERR_CANCELED, loop.Run(), "run loop");
  EXPECT_EQ(1u, task1.run_count, "run count 1");
  EXPECT_EQ(ZX_OK, task1.last_status, "status 1");
  EXPECT_EQ(1u, task2.run_count, "run count 2");
  EXPECT_EQ(ZX_OK, task2.last_status, "status 2");
  EXPECT_EQ(0u, task3.run_count, "run count 3");
  EXPECT_EQ(0u, task4.run_count, "run count 4");
  EXPECT_EQ(1u, task5.run_count, "run count 5");
  EXPECT_EQ(ZX_OK, task5.last_status, "status 5");
  EXPECT_EQ(1u, task6.run_count, "run count 6");
  EXPECT_EQ(ZX_OK, task6.last_status, "status 6");
  EXPECT_EQ(ZX_ERR_NOT_FOUND, task6.cancel_result, "cancel result 6");
  EXPECT_EQ(0u, task7.run_count, "run count 7");

  // Cancel task 4.
  EXPECT_EQ(ZX_OK, task4.Cancel(loop.dispatcher()), "cancel 4");

  // When the loop shuts down:
  //   |task1| not notified because it was serviced
  //   |task2| notified because it requested a repeat
  //   |task3| notified because it was not yet serviced
  //   |task4| not notified because it was canceled
  //   |task5| not notified because it was serviced
  //   |task6| not notified because it was serviced
  //   |task7| notified because it was not yet serviced
  loop.Shutdown();
  EXPECT_EQ(1u, task1.run_count, "run count 1");
  EXPECT_EQ(2u, task2.run_count, "run count 2");
  EXPECT_EQ(ZX_ERR_CANCELED, task2.last_status, "status 2");
  EXPECT_EQ(1u, task3.run_count, "run count 3");
  EXPECT_EQ(ZX_ERR_CANCELED, task3.last_status, "status 3");
  EXPECT_EQ(0u, task4.run_count, "run count 4");
  EXPECT_EQ(1u, task5.run_count, "run count 5");
  EXPECT_EQ(1u, task6.run_count, "run count 6");
  EXPECT_EQ(1u, task7.run_count, "run count 7");
  EXPECT_EQ(ZX_ERR_CANCELED, task7.last_status, "status 7");
  EXPECT_EQ(ZX_ERR_NOT_FOUND, task7.cancel_result, "cancel result 7");

  // Try to add or cancel work after shutdown.
  TestTask task8;
  EXPECT_EQ(ZX_ERR_BAD_STATE, task8.PostForTime(loop.dispatcher(), zx::time::infinite()),
            "post after shutdown");
  EXPECT_EQ(ZX_ERR_NOT_FOUND, task8.Cancel(loop.dispatcher()), "cancel after shutdown");
  EXPECT_EQ(0u, task8.run_count, "run count 8");
}

TEST(Loop, Receiver) {
  const zx_packet_user_t data1{.u64 = {11, 12, 13, 14}};
  const zx_packet_user_t data2{.u64 = {21, 22, 23, 24}};
  const zx_packet_user_t data3{.u64 = {31, 32, 33, 34}};
  const zx_packet_user_t data_default{};

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  TestReceiver receiver1;
  TestReceiver receiver2;
  TestReceiver receiver3;

  EXPECT_EQ(ZX_OK, receiver1.QueuePacket(loop.dispatcher(), &data1), "queue 1");
  EXPECT_EQ(ZX_OK, receiver1.QueuePacket(loop.dispatcher(), &data3), "queue 1, again");
  EXPECT_EQ(ZX_OK, receiver2.QueuePacket(loop.dispatcher(), &data2), "queue 2");
  EXPECT_EQ(ZX_OK, receiver3.QueuePacket(loop.dispatcher(), nullptr), "queue 3");

  EXPECT_EQ(ZX_OK, loop.RunUntilIdle(), "run loop");
  EXPECT_EQ(2u, receiver1.run_count, "run count 1");
  EXPECT_EQ(ZX_OK, receiver1.last_status, "status 1");
  EXPECT_NE(receiver1.last_data, nullptr);
  EXPECT_EQ(0, memcmp(&data3, receiver1.last_data, sizeof(zx_packet_user_t)), "data 1");
  EXPECT_EQ(1u, receiver2.run_count, "run count 2");
  EXPECT_EQ(ZX_OK, receiver2.last_status, "status 2");
  EXPECT_NE(receiver2.last_data, nullptr);
  EXPECT_EQ(0, memcmp(&data2, receiver2.last_data, sizeof(zx_packet_user_t)), "data 2");
  EXPECT_EQ(1u, receiver3.run_count, "run count 3");
  EXPECT_EQ(ZX_OK, receiver3.last_status, "status 3");
  EXPECT_NE(receiver3.last_data, nullptr);
  EXPECT_EQ(0, memcmp(&data_default, receiver3.last_data, sizeof(zx_packet_user_t)), "data 3");
}

TEST(Loop, ReceiverShutdown) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.Shutdown();

  // Try to add work after shutdown.
  TestReceiver receiver;
  EXPECT_EQ(ZX_ERR_BAD_STATE, receiver.QueuePacket(loop.dispatcher(), nullptr),
            "queue after shutdown");
  EXPECT_EQ(0u, receiver.run_count, "run count 1");
}

TEST(Loop, PagedVmoShutdown) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  EXPECT_EQ(ASYNC_LOOP_RUNNABLE, loop.GetState(), "loop runnable");

  zx::pager pager;
  EXPECT_EQ(ZX_OK, zx::pager::create(0, &pager), "pager create");
  zx::vmo vmo;

  TestPagedVmo paged_vmo;
  EXPECT_EQ(ZX_OK, paged_vmo.Create(loop.dispatcher(), pager, &vmo), "paged vmo create");

  zx_info_vmo_t info;
  EXPECT_EQ(ZX_OK, vmo.get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr),
            "vmo get info");
  EXPECT_EQ(ZX_INFO_VMO_PAGER_BACKED, info.flags & ZX_INFO_VMO_PAGER_BACKED, "vmo pager backed");

  loop.Shutdown();

  // Verify that we sent a ZX_ERR_CANCELED to the handler on loop shutdown.
  // TODO(rashaeqbal): Ideally we want to verify that the VMO has been detached from the pager.
  // However, there is currently no straightforward way to verify this. Checking for ZX_ERR_CANCELED
  // serves as a proxy for this, since we detach before the ZX_ERR_CANCELED status is sent to the
  // handler.
  EXPECT_TRUE(paged_vmo.IsCanceled(), "paged vmo cancel after shutdown");
}

TEST(Loop, PagedVmoAlreadyDestroyed) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  zx::pager pager;
  EXPECT_EQ(ZX_OK, zx::pager::create(0, &pager));

  // Allocate the TestPagedVMO on the heap so asan can check that there are no dereferences of it
  // after destruction.
  zx::vmo vmo;
  auto paged_vmo = std::make_unique<TestPagedVmo>();
  EXPECT_EQ(ZX_OK, paged_vmo->Create(loop.dispatcher(), pager, &vmo));

  // Release the VMO before unregistering the paged_vmo. The kernel will report that detaching
  // failed but the loop's tracking information should still be deleted.
  vmo.reset();
  EXPECT_NE(paged_vmo->Detach(), ZX_OK);
  paged_vmo.reset();

  // This test really just tests that this doesn't crash with a bad pointer or with an asan
  // use-after-free error as a result of the paged_vmo watcher still being registered.
  loop.Shutdown();
}

TEST(Loop, PagedVmoMultithreaded) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  EXPECT_EQ(ASYNC_LOOP_RUNNABLE, loop.GetState(), "loop runnable");

  zx::pager pager;
  EXPECT_OK(zx::pager::create(0, &pager), "pager create");

  EXPECT_OK(loop.StartThread("test-pager-thread"));

  constexpr uint64_t kNumVmos = 20;
  constexpr uint64_t kNumThreads = 5;
  // Create a separate scratch space for each thread so that they don't require locked access to
  // shared state. Locking will synchronize the underlying paged_vmo_list modifications too, which
  // is what this test verifies. We need lockless multi-threaded access and also valid vmos for
  // each thread to work with.
  struct thread_state {
    std::array<TestPagedVmo, kNumVmos> paged_vmos;
    std::array<zx::vmo, kNumVmos> vmos;
  };
  std::array<struct thread_state, kNumThreads> state;

  // This test verifies multithreaded access to paged_vmo_list in three different phases:
  // 1. Create vmos from multiple threads in parallel.
  // 2. Randomly detach a few vmos created in phase 1, while creating some more vmos.
  // 3. Detach all remaining vmos while shutting down the async loop.

  // Phase 1. Create a few paged vmos concurrently.
  // Verifies concurrent additions to the paged_vmo_list.
  std::array<std::thread, kNumThreads> threads1;
  for (uint64_t i = 0; i < kNumThreads; i++) {
    threads1[i] = std::thread([&state, &loop, &pager, index = i]() {
      for (uint64_t x = 0; x < kNumVmos / 2; x++) {
        EXPECT_OK(
            state[index].paged_vmos[x].Create(loop.dispatcher(), pager, &state[index].vmos[x]),
            "paged vmo create");
        zx_info_vmo_t info;
        EXPECT_OK(state[index].vmos[x].get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr),
                  "vmo get info");
        EXPECT_EQ(ZX_INFO_VMO_PAGER_BACKED, info.flags & ZX_INFO_VMO_PAGER_BACKED,
                  "vmo pager backed");
      }
    });
  }

  for (auto& thread : threads1) {
    thread.join();
  }

  std::default_random_engine random(zxtest::Runner::GetInstance()->random_seed());

  // Phase 2. Detach a few of the vmos created above, while concurrently creating some more vmos.
  // Verifies concurrent additions and deletions from the paged_vmo_list.
  std::array<std::thread, kNumThreads> threads2;
  for (uint64_t i = 0; i < kNumThreads; i++) {
    threads2[i] = std::thread([&state, &loop, &pager, &random, index = i]() {
      // Generate some random vmo indices to operate on. Detach vmos with indices less than
      // kNumVmos/2 (these were already created above), and create vmos for other indices.
      std::array<uint64_t, kNumVmos / 2> indices;
      std::uniform_int_distribution<uint64_t> distribution(0, kNumVmos - 1);
      std::generate(indices.begin(), indices.end(), [&]() { return distribution(random); });

      for (auto x : indices) {
        if (x < kNumVmos / 2) {
          // This vmo was created above in phase 1. Detach it.
          if (state[index].vmos[x].is_valid()) {
            EXPECT_OK(state[index].paged_vmos[x].Detach());
            state[index].vmos[x].reset();
          }
        } else {
          // Otherwise create a new vmo (if not already created).
          if (!state[index].vmos[x].is_valid()) {
            EXPECT_OK(
                state[index].paged_vmos[x].Create(loop.dispatcher(), pager, &state[index].vmos[x]),
                "paged vmo create");
            zx_info_vmo_t info;
            EXPECT_OK(
                state[index].vmos[x].get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr),
                "vmo get info");
            EXPECT_EQ(ZX_INFO_VMO_PAGER_BACKED, info.flags & ZX_INFO_VMO_PAGER_BACKED,
                      "vmo pager backed");
          }
        }
      }
    });
  }

  for (auto& thread : threads2) {
    thread.join();
  }

  // Phase 3. Detach any remaining vmos, while concurrently shutting down the async loop. Verifies
  // the two deletion paths for elements in paged_vmo_list - loop shutdown and explicit detach.
  std::array<std::thread, kNumThreads> threads3;

  // Create a thread to shut down the loop in parallel, deleting vmos from the list.
  auto shutdown_thread = std::thread([&loop]() { loop.Shutdown(); });

  for (uint64_t i = 0; i < kNumThreads; i++) {
    threads3[i] = std::thread([&state, index = i]() {
      for (uint64_t x = 0; x < kNumVmos; x++) {
        if (state[index].vmos[x].is_valid()) {
          // Redundant detaches are fine - if a vmo is not found in the list, we'll simply return a
          // ZX_ERR_NOT_FOUND. What we care about here is that there are no crashes while accessing
          // list members, i.e. parallel detaches and shutdown do not cause the vmo from
          // disappearing while being accessed.
          zx_status_t status = state[index].paged_vmos[x].Detach();
          EXPECT_TRUE(status == ZX_OK || status == ZX_ERR_NOT_FOUND);
        }
      }
    });
  }

  for (auto& thread : threads3) {
    thread.join();
  }
  shutdown_thread.join();
}

class GetDefaultDispatcherTask : public QuitTask {
 public:
  async_dispatcher_t* last_default_dispatcher;

 protected:
  void Handle(async_dispatcher_t* dispatcher, zx_status_t status) override {
    QuitTask::Handle(dispatcher, status);
    last_default_dispatcher = async_get_default_dispatcher();
  }
};

class ConcurrencyMeasure {
 public:
  ConcurrencyMeasure(uint32_t end) : end_(end) {}

  uint32_t max_threads() const { return max_threads_.load(std::memory_order_acquire); }
  uint32_t count() const { return count_.load(std::memory_order_acquire); }

  void Tally(async_dispatcher_t* dispatcher) {
    // Increment count of concurrently active threads.  Update maximum if needed.
    uint32_t active =
        1u + std::atomic_fetch_add_explicit(&active_threads_, 1u, std::memory_order_acq_rel);
    uint32_t old_max;
    do {
      old_max = max_threads_.load(std::memory_order_acquire);
    } while (active > old_max &&
             !max_threads_.compare_exchange_weak(old_max, active, std::memory_order_acq_rel,
                                                 std::memory_order_acquire));

    // Pretend to do work.
    zx::nanosleep(zx::deadline_after(zx::msec(1)));

    // Decrement count of active threads.
    std::atomic_fetch_sub_explicit(&active_threads_, 1u, std::memory_order_acq_rel);

    // Quit when last item processed.
    if (1u + std::atomic_fetch_add_explicit(&count_, 1u, std::memory_order_acq_rel) == end_)
      async_loop_quit(async_loop_from_dispatcher(dispatcher));
  }

 private:
  const uint32_t end_;
  std::atomic_uint32_t count_{};
  std::atomic_uint32_t active_threads_{};
  std::atomic_uint32_t max_threads_{};
};

class ThreadAssertWait : public TestWait {
 public:
  ThreadAssertWait(zx_handle_t object, zx_signals_t trigger, ConcurrencyMeasure* measure)
      : TestWait(object, trigger), measure_(measure) {}

 protected:
  ConcurrencyMeasure* measure_;

  void Handle(async_dispatcher_t* dispatcher, zx_status_t status,
              const zx_packet_signal_t* signal) override {
    TestWait::Handle(dispatcher, status, signal);
    measure_->Tally(dispatcher);
  }
};

class ThreadAssertTask : public TestTask {
 public:
  ThreadAssertTask(ConcurrencyMeasure* measure) : measure_(measure) {}

 protected:
  ConcurrencyMeasure* measure_;

  void Handle(async_dispatcher_t* dispatcher, zx_status_t status) override {
    TestTask::Handle(dispatcher, status);
    measure_->Tally(dispatcher);
  }
};

class ThreadAssertReceiver : public TestReceiver {
 public:
  ThreadAssertReceiver(ConcurrencyMeasure* measure) : measure_(measure) {}

 protected:
  ConcurrencyMeasure* measure_;

  // This receiver's handler will run concurrently on multiple threads
  // (unlike the Waits and Tasks) so we must guard its state.
  fbl::Mutex mutex_;

  void Handle(async_dispatcher_t* dispatcher, zx_status_t status,
              const zx_packet_user_t* data) override {
    {
      fbl::AutoLock lock(&mutex_);
      TestReceiver::Handle(dispatcher, status, data);
    }
    measure_->Tally(dispatcher);
  }
};

TEST(Loop, ThreadsHaveDefaultDispatcher) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  EXPECT_EQ(ZX_OK, loop.StartThread(), "start thread");

  GetDefaultDispatcherTask task;
  EXPECT_EQ(ZX_OK, task.Post(loop.dispatcher()), "post task");
  loop.JoinThreads();

  EXPECT_EQ(1u, task.run_count, "run count");
  EXPECT_EQ(ZX_OK, task.last_status, "status");
  EXPECT_EQ(loop.dispatcher(), task.last_default_dispatcher, "default dispatcher");
}

TEST(Loop, ThreadsDontHaveDefaultDispatcher) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  EXPECT_EQ(ZX_OK, loop.StartThread(), "start thread");

  GetDefaultDispatcherTask task;
  EXPECT_EQ(ZX_OK, task.Post(loop.dispatcher()), "post task");
  loop.JoinThreads();

  EXPECT_EQ(1u, task.run_count, "run count");
  EXPECT_EQ(ZX_OK, task.last_status, "status");
  EXPECT_NULL(task.last_default_dispatcher, "default dispatcher");
}

// The goal here is to ensure that threads stop when Quit() is called.
TEST(Loop, ThreadsQuit) {
  const size_t num_threads = 4;

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  for (size_t i = 0; i < num_threads; i++) {
    EXPECT_EQ(ZX_OK, loop.StartThread());
  }
  loop.Quit();
  loop.JoinThreads();
  EXPECT_EQ(ASYNC_LOOP_QUIT, loop.GetState());
}

// The goal here is to ensure that threads stop when Shutdown() is called.
TEST(Loop, ThroadsShutdown) {
  for (int i = 0; i < 3; i++) {
    const size_t num_threads = 4;

    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    for (size_t i = 0; i < num_threads; i++) {
      EXPECT_EQ(ZX_OK, loop.StartThread());
    }
    loop.Shutdown();
    EXPECT_EQ(ASYNC_LOOP_SHUTDOWN, loop.GetState());

    loop.JoinThreads();  // should be a no-op

    EXPECT_EQ(ZX_ERR_BAD_STATE, loop.StartThread(), "can't start threads after shutdown");
  }
}

// The goal here is to schedule a lot of work and see whether it runs
// on as many threads as we expected it to.
TEST(Loop, ThroadsWitsRunConcurrently) {
  for (int i = 0; i < 3; i++) {
    const size_t num_threads = 4;
    const size_t num_items = 100;

    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    for (size_t i = 0; i < num_threads; i++) {
      EXPECT_EQ(ZX_OK, loop.StartThread(), "start thread");
    }

    ConcurrencyMeasure measure(num_items);
    zx::event event;
    EXPECT_EQ(ZX_OK, zx::event::create(0u, &event), "create event");
    EXPECT_EQ(ZX_OK, event.signal(0u, ZX_USER_SIGNAL_0), "signal");

    // Post a number of work items to run all at once.
    ThreadAssertWait* items[num_items];
    for (size_t i = 0; i < num_items; i++) {
      items[i] = new ThreadAssertWait(event.get(), ZX_USER_SIGNAL_0, &measure);
      EXPECT_EQ(ZX_OK, items[i]->Begin(loop.dispatcher()), "begin wait");
    }

    // Wait until quitted.
    loop.JoinThreads();

    // Ensure all work items completed.
    EXPECT_EQ(num_items, measure.count(), "item count");
    for (size_t i = 0; i < num_items; i++) {
      EXPECT_EQ(1u, items[i]->run_count, "run count");
      EXPECT_EQ(ZX_OK, items[i]->last_status, "status");
      EXPECT_NE(items[i]->last_signal, nullptr, "signal");
      EXPECT_EQ(ZX_USER_SIGNAL_0, items[i]->last_signal->observed & ZX_USER_SIGNAL_ALL, "observed");
      delete items[i];
    }

    // Ensure that we actually ran many waits concurrently on different threads.
    EXPECT_NE(1u, measure.max_threads(), "waits handled concurrently");
  }
}

// The goal here is to schedule a lot of work and see whether it runs
// on as many threads as we expected it to.
TEST(Loop, ThreadsTasksRunSequentially) {
  for (int i = 0; i < 3; i++) {
    const size_t num_threads = 4;
    const size_t num_items = 100;

    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    for (size_t i = 0; i < num_threads; i++) {
      EXPECT_EQ(ZX_OK, loop.StartThread(), "start thread");
    }

    ConcurrencyMeasure measure(num_items);

    // Post a number of work items to run all at once.
    ThreadAssertTask* items[num_items];
    zx::time start_time = async::Now(loop.dispatcher());
    for (size_t i = 0; i < num_items; i++) {
      items[i] = new ThreadAssertTask(&measure);
      EXPECT_EQ(ZX_OK, items[i]->PostForTime(loop.dispatcher(), start_time + zx::msec(i)),
                "post task");
    }

    // Wait until quitted.
    loop.JoinThreads();

    // Ensure all work items completed.
    EXPECT_EQ(num_items, measure.count(), "item count");
    for (size_t i = 0; i < num_items; i++) {
      EXPECT_EQ(1u, items[i]->run_count, "run count");
      EXPECT_EQ(ZX_OK, items[i]->last_status, "status");
      delete items[i];
    }

    // Ensure that we actually ran tasks sequentially despite having many
    // threads available.
    EXPECT_EQ(1u, measure.max_threads(), "tasks handled sequentially");
  }
}

// The goal here is to schedule a lot of work and see whether it runs
// on as many threads as we expected it to.
TEST(Loop, ThreadsReceiversRunConcurrently) {
  for (int i = 0; i < 3; i++) {
    const size_t num_threads = 4;
    const size_t num_items = 100;

    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    for (size_t i = 0; i < num_threads; i++) {
      EXPECT_EQ(ZX_OK, loop.StartThread(), "start thread");
    }

    ConcurrencyMeasure measure(num_items);

    // Post a number of packets all at once.
    ThreadAssertReceiver receiver(&measure);
    for (size_t i = 0; i < num_items; i++) {
      EXPECT_EQ(ZX_OK, receiver.QueuePacket(loop.dispatcher(), nullptr), "queue packet");
    }

    // Wait until quitted.
    loop.JoinThreads();

    // Ensure all work items completed.
    EXPECT_EQ(num_items, measure.count(), "item count");
    EXPECT_EQ(num_items, receiver.run_count, "run count");
    EXPECT_EQ(ZX_OK, receiver.last_status, "status");

    // Ensure that we actually processed many packets concurrently on different threads.
    EXPECT_NE(1u, measure.max_threads(), "packets handled concurrently");
  }
}

// Ensure that tasks are cancelled using a worker thread if one is available.
//
// This ensures that thread-hostile callbacks don't get run on the wrong thread.
TEST(Loop, ShutdownOnWorkerThreads) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  // Start a worker thread.
  thrd_t worker;
  loop.StartThread("loop_tests_thread", &worker);

  // Post a task that will never be executed.
  bool callback_called = false;
  async::Task task([&worker, &callback_called](async_dispatcher_t* dispatcher, async::Task* task,
                                               zx_status_t status) {
    EXPECT_EQ(status, ZX_ERR_CANCELED, "Unexpected status: %s", zx_status_get_string(status));
    EXPECT_TRUE(thrd_equal(thrd_current(), worker), "Callback called on incorrect thread");
    callback_called = true;
  });
  task.PostDelayed(loop.dispatcher(), zx::duration::infinite());

  // Shut down the loop, and ensure the callback was called on the other thread.
  loop.Shutdown();
  EXPECT_TRUE(callback_called);
}

// Ensure that tasks are cancelled correctly when no worker threads are available.
TEST(Loop, ShutdownNoWorkerThreads) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  std::optional<std::thread> shutdown_thread;

  // Post a task that will never be executed.
  bool callback_called = false;
  async::Task task([&shutdown_thread, &callback_called](async_dispatcher_t* dispatcher,
                                                        async::Task* task, zx_status_t status) {
    EXPECT_EQ(status, ZX_ERR_CANCELED, "Unexpected status: %s", zx_status_get_string(status));
    EXPECT_EQ(std::this_thread::get_id(), shutdown_thread->get_id(),
              "Callback called on incorrect thread");
    callback_called = true;
  });
  task.PostDelayed(loop.dispatcher(), zx::duration::infinite());

  // Shut down the loop, and ensure the callback was called on the thread calling `Shutdown`.
  shutdown_thread.emplace([&loop] { loop.Shutdown(); });
  shutdown_thread->join();

  EXPECT_TRUE(callback_called);
}

// Test that if PostTask races against Shutdown(), then either the Post will return an error or the
// shutdown will run the task.
TEST(Loop, PostTasksDestroyed) {
  std::default_random_engine random(zxtest::Runner::GetInstance()->random_seed());

  // Tuned to make PostTask() error around 50% of the time on a VIM3 in release mode.
  std::uniform_int_distribution<int64_t> time_ns_distribution(0.0, 5000);
  std::uniform_int_distribution<int64_t> main_thread_ns_distribution(0.0, 100000);
  constexpr auto kCombinedDelayUs = zx::usec(100);

  uint32_t post_success_count = 0;
  uint32_t trial_count = 0;
  auto end_time = zx::deadline_after(zx::sec(2));
  constexpr uint32_t kMinTrialCount = 10;
  while ((trial_count < kMinTrialCount) || (zx::clock::get_monotonic() < end_time)) {
    trial_count++;
    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

    int64_t secondary_thread_sleep_ns = time_ns_distribution(random);
    int64_t main_thread_sleep_ns = main_thread_ns_distribution(random);

    TestTask task1;
    auto start_time = zx::deadline_after(kCombinedDelayUs);
    auto thread = std::thread([&] {
      zx::nanosleep(start_time + zx::nsec(secondary_thread_sleep_ns));
      loop.Shutdown();
    });

    zx::nanosleep(start_time + zx::nsec(main_thread_sleep_ns));
    zx_status_t task_result = task1.Post(loop.dispatcher());

    thread.join();

    if (task_result == ZX_OK) {
      post_success_count++;
      EXPECT_EQ(1u, task1.run_count, "secondary sleep %ld main sleep %ld",
                secondary_thread_sleep_ns, main_thread_sleep_ns);
    }
  }
  // Print fraction of times PostTask is successful (as opposed to returning an error), to help get
  // an understanding of whether the delay distributions are tuned to make a race condition likely.
  printf("Post success rate: %d/%d\n", post_success_count, trial_count);
}

}  // namespace
