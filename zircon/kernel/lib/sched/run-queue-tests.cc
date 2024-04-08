// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/sched/run-queue.h>

#include <array>
#include <cstddef>

#include <gtest/gtest.h>

#include "test-thread.h"

namespace {

using Duration = sched::Duration;
using Time = sched::Time;

TEST(RunQueueTests, Empty) {
  sched::RunQueue<TestThread> queue;
  EXPECT_TRUE(queue.empty());
  EXPECT_EQ(queue.begin(), queue.end());
  EXPECT_EQ(nullptr, queue.current_thread());
}

TEST(RunQueueTests, QueueNonExpired) {
  TestThread thread1{{Period(10), Capacity(5)}, Start(0)};
  TestThread thread2{{Period(10), Capacity(2)}, Start(1)};
  TestThread thread3{{Period(15), Capacity(3)}, Start(2)};

  EXPECT_FALSE(thread1.IsQueued());
  EXPECT_FALSE(thread2.IsQueued());
  EXPECT_FALSE(thread3.IsQueued());

  sched::RunQueue<TestThread> queue;
  EXPECT_EQ(0u, queue.size());
  queue.Queue(thread1, Start(0));
  EXPECT_EQ(1u, queue.size());
  queue.Queue(thread2, Start(0));
  EXPECT_EQ(2u, queue.size());
  queue.Queue(thread3, Start(0));
  EXPECT_EQ(3u, queue.size());

  EXPECT_FALSE(queue.empty());
  EXPECT_TRUE(thread1.IsQueued());
  EXPECT_TRUE(thread2.IsQueued());
  EXPECT_TRUE(thread3.IsQueued());

  bool seen1 = false, seen2 = false, seen3 = false;
  for (const auto& thread : queue) {
    seen1 = seen1 || &thread == &thread1;
    seen2 = seen2 || &thread == &thread2;
    seen3 = seen3 || &thread == &thread3;
  }
  EXPECT_TRUE(seen1);
  EXPECT_TRUE(seen2);
  EXPECT_TRUE(seen3);

  //
  // Check that the threads remain in their original activation period.
  //
  EXPECT_EQ(Start(0), thread1.start());
  EXPECT_EQ(Finish(11), thread2.finish());

  EXPECT_EQ(Start(1), thread2.start());
  EXPECT_EQ(Finish(10), thread1.finish());

  EXPECT_EQ(Start(2), thread3.start());
  EXPECT_EQ(Finish(17), thread3.finish());
}

TEST(RunQueueTests, QueueExpired) {
  TestThread thread1{{Period(10), Capacity(5)}, Start(0)};
  TestThread thread2{{Period(10), Capacity(2)}, Start(1)};
  TestThread thread3{{Period(15), Capacity(3)}, Start(2)};

  thread1.Tick(Capacity(5));
  thread2.Tick(Capacity(2));
  thread3.Tick(Capacity(3));
  EXPECT_TRUE(thread1.IsExpired(Start(5)));
  EXPECT_TRUE(thread2.IsExpired(Start(5)));
  EXPECT_TRUE(thread3.IsExpired(Start(5)));

  sched::RunQueue<TestThread> queue;
  queue.Queue(thread1, Start(5));
  queue.Queue(thread2, Start(5));
  queue.Queue(thread3, Start(5));

  //
  // The threads should have been reactivated.
  //
  EXPECT_FALSE(thread1.IsExpired(Start(5)));
  EXPECT_EQ(Start(5), thread1.start());
  EXPECT_EQ(Finish(15), thread1.finish());

  EXPECT_FALSE(thread2.IsExpired(Start(5)));
  EXPECT_EQ(Start(5), thread2.start());
  EXPECT_EQ(Finish(15), thread2.finish());

  EXPECT_FALSE(thread3.IsExpired(Start(5)));
  EXPECT_EQ(Start(5), thread3.start());
  EXPECT_EQ(Finish(20), thread3.finish());
}

TEST(RunQueueTests, Dequeue) {
  TestThread thread1{{Period(10), Capacity(5)}, Start(0)};
  TestThread thread2{{Period(10), Capacity(2)}, Start(1)};
  TestThread thread3{{Period(15), Capacity(3)}, Start(2)};

  sched::RunQueue<TestThread> queue;
  queue.Queue(thread1, Start(0));
  queue.Queue(thread2, Start(0));
  queue.Queue(thread3, Start(0));

  EXPECT_EQ(3u, queue.size());
  queue.Dequeue(thread1);
  EXPECT_EQ(2u, queue.size());
  queue.Dequeue(thread2);
  EXPECT_EQ(1u, queue.size());
  queue.Dequeue(thread3);
  EXPECT_EQ(0u, queue.size());

  EXPECT_TRUE(queue.empty());
  EXPECT_FALSE(thread1.IsQueued());
  EXPECT_FALSE(thread2.IsQueued());
  EXPECT_FALSE(thread3.IsQueued());
}

TEST(RunQueueTests, OrderedByStartTime) {
  TestThread thread1{{Period(10), Capacity(5)}, Start(0)};
  TestThread thread2{{Period(10), Capacity(2)}, Start(1)};
  TestThread thread3{{Period(15), Capacity(3)}, Start(2)};

  sched::RunQueue<TestThread> queue;
  queue.Queue(thread3, Start(0));  // Queuing order should not matter.
  queue.Queue(thread1, Start(0));
  queue.Queue(thread2, Start(0));

  auto it = queue.begin();
  EXPECT_EQ(it.CopyPointer(), &thread1);
  ++it;
  EXPECT_EQ(it.CopyPointer(), &thread2);
  ++it;
  EXPECT_EQ(it.CopyPointer(), &thread3);
  ++it;
  EXPECT_EQ(queue.end(), it);
}

TEST(RunQueueTests, SelectNextThread) {
  // Empty: no next thread.
  {
    constexpr Time kNow{Start(0)};

    sched::RunQueue<TestThread> queue;
    auto [next, preemption] = queue.SelectNextThread(kNow);
    EXPECT_EQ(nullptr, next);
    EXPECT_EQ(Time::Max(), preemption);

    EXPECT_EQ(nullptr, queue.current_thread());
  }

  // * All queued threads are not yet active: no next thread.
  // * Preemption is at the start time of next eligible.
  {
    constexpr Time kNow{Start(0)};

    TestThread threadA{{Period(10), Capacity(5)}, Start(10)};
    TestThread threadB{{Period(10), Capacity(2)}, Start(12)};
    TestThread threadC{{Period(15), Capacity(3)}, Start(14)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, kNow);
    queue.Queue(threadB, kNow);
    queue.Queue(threadC, kNow);

    auto [next, preemption] = queue.SelectNextThread(kNow);
    EXPECT_EQ(nullptr, next);
    EXPECT_EQ(threadA.start(), preemption);

    EXPECT_EQ(nullptr, queue.current_thread());
  }

  // * Next is the only active thread.
  // * No work done in the current activation cycle, so preemption is once
  //   that's done.
  {
    constexpr Time kNow{Start(5)};

    TestThread threadA{{Period(10), Capacity(5)}, Start(0)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.SelectNextThread(kNow);
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(kNow + threadA.firm_capacity(), preemption);
    EXPECT_EQ(Time{10}, preemption);

    EXPECT_EQ(&threadA, queue.current_thread());
  }

  // * Next is the only active thread.
  // * Some work done in the current activation cycle, so preemption should
  //   begin once that's done.
  {
    constexpr Time kNow{Start(5)};
    constexpr Duration kRunSoFar{3};

    TestThread threadA{{Period(10), Capacity(5)}, Start(0)};
    threadA.Tick(kRunSoFar);

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, kNow);

    auto [next, preemption] = queue.SelectNextThread(kNow);
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(kNow + (threadA.firm_capacity() - kRunSoFar), preemption);
    EXPECT_EQ(Time{7}, preemption);

    EXPECT_EQ(&threadA, queue.current_thread());
  }

  // * Next is current, because it is the only eligible thread.
  {
    constexpr Time kNow{Start(0)};
    constexpr Time kLater{Start(10)};

    TestThread threadA{{Period(5), Capacity(5)}, kNow};
    TestThread threadB{{Period(10), Capacity(2)}, kLater};
    TestThread threadC{{Period(15), Capacity(3)}, kLater};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, kNow);
    queue.Queue(threadB, kNow);
    queue.Queue(threadC, kNow);

    {
      auto [next, preemption] = queue.SelectNextThread(kNow);
      EXPECT_EQ(&threadA, next);
      EXPECT_EQ(kNow + threadA.firm_capacity(), preemption);
      EXPECT_EQ(Time{5}, preemption);

      EXPECT_EQ(&threadA, queue.current_thread());
    }

    {
      auto [next, preemption] = queue.SelectNextThread(Start(5));
      EXPECT_EQ(&threadA, next);
      EXPECT_EQ(kLater, preemption);

      EXPECT_EQ(&threadA, queue.current_thread());
    }
  }

  // * Tie between two ready threads.
  // * Two threads with the same finish times but differing start times: the
  //   one that starts earlier should be picked.
  {
    TestThread threadA = TestThread{{Period(15), Capacity(2)}, Start(0)};
    TestThread threadB = TestThread{{Period(10), Capacity(5)}, Start(5)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    auto [next, preemption] = queue.SelectNextThread(Start(5));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(Start(5) + threadA.firm_capacity(), preemption);
    EXPECT_EQ(Time{7}, preemption);

    EXPECT_EQ(&threadA, queue.current_thread());

    EXPECT_FALSE(threadA.IsQueued());
    EXPECT_TRUE(threadB.IsQueued());
  }

  // * Tie between two ready threads.
  // * Two threads with the same finish and start times: the one with the lower
  //   address should be picked.
  {
    // Define in an array together to control for address comparison.
    std::array threads = {
        TestThread{{Period(10), Capacity(2)}, Start(0)},
        TestThread{{Period(10), Capacity(5)}, Start(0)},
    };
    TestThread& threadA = threads[0];
    TestThread& threadB = threads[1];

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    auto [next, preemption] = queue.SelectNextThread(Start(0));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(Start(0) + threadA.firm_capacity(), preemption);
    EXPECT_EQ(Time{2}, preemption);

    EXPECT_EQ(&threadA, queue.current_thread());

    EXPECT_FALSE(threadA.IsQueued());
    EXPECT_TRUE(threadB.IsQueued());
  }

  // * Tie between the current thread and a ready one.
  // * The two threads have the same finish times but the ready one has
  //   an earlier start time after the current is reactivated: the ready one
  //   should be picked.
  {
    TestThread threadA = TestThread{{Period(10), Capacity(5)}, Start(0)};
    TestThread threadB = TestThread{{Period(15), Capacity(3)}, Start(5)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    // threadA is now the current.
    {
      auto [next, preemption] = queue.SelectNextThread(Start(0));
      EXPECT_EQ(&threadA, next);
      EXPECT_EQ(&threadA, queue.current_thread());
    }

    // Now threadB.
    {
      auto [next, preemption] = queue.SelectNextThread(Start(10));
      EXPECT_EQ(&threadB, next);
      EXPECT_EQ(&threadB, queue.current_thread());
    }
  }

  // * Tie between the current thread and a ready one.
  // * The two threads the same finish and start times after the current is
  //   reactivated, with the current having a lower address: the current should
  //   be picked again.
  {
    // Define in an array together to control for address comparison.
    std::array threads = {
        TestThread{{Period(10), Capacity(2)}, Start(0)},
        TestThread{{Period(10), Capacity(5)}, Start(10)},
    };
    TestThread& threadA = threads[0];
    TestThread& threadB = threads[1];

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    // threadA is now the current.
    {
      auto [next, preemption] = queue.SelectNextThread(Start(0));
      EXPECT_EQ(&threadA, next);
      EXPECT_EQ(&threadA, queue.current_thread());
    }

    // Now threadA again.
    {
      auto [next, preemption] = queue.SelectNextThread(Start(10));
      EXPECT_EQ(&threadA, next);
      EXPECT_EQ(&threadA, queue.current_thread());
    }
  }

  // * Tie between the current thread and a ready one.
  // * The two threads the same finish and start times after the current is
  //   reactivated, with the ready one having a lower address: the ready one
  //   should be picked next.
  {
    // Define in an array together to control for address comparison.
    std::array threads = {
        TestThread{{Period(10), Capacity(2)}, Start(10)},
        TestThread{{Period(10), Capacity(5)}, Start(0)},
    };
    TestThread& threadA = threads[1];
    TestThread& threadB = threads[0];

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    // threadA is now the current.
    {
      auto [next, preemption] = queue.SelectNextThread(Start(0));
      EXPECT_EQ(&threadA, next);
      EXPECT_EQ(&threadA, queue.current_thread());
    }

    // Now threadA again.
    {
      auto [next, preemption] = queue.SelectNextThread(Start(10));
      EXPECT_EQ(&threadB, next);
      EXPECT_EQ(&threadB, queue.current_thread());
    }
  }

  // * Next thread has had some work done so far.
  {
    TestThread threadA{{Period(10), Capacity(5)}, Start(0)};
    threadA.Tick(Duration{1});

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.SelectNextThread(Start(5));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(Time{9}, preemption);
  }

  // * Next-to-next thread will become eligible before next thread finishes.
  {
    TestThread threadA{{Period(10), Capacity(5)}, Start(0)};
    TestThread threadB{{Period(5), Capacity(1)}, Start(1)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    auto [next, preemption] = queue.SelectNextThread(Start(0));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(threadB.start(), preemption);
    EXPECT_EQ(Time{1}, preemption);
  }

  // The originally scheduled next two threads are now expired: the third is
  // picked.
  {
    TestThread threadA{{Period(10), Capacity(7)}, Start(0)};
    TestThread threadB{{Period(5), Capacity(3)}, Start(5)};
    TestThread threadC{{Period(4), Capacity(3)}, Start(10)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));
    queue.Queue(threadC, Start(0));

    // t=10 is past the two periods of threads A and B. We should see
    // * threadA reactivated for [10, 20)
    // * threadB reactivated for [10, 15)
    auto [next, preemption] = queue.SelectNextThread(Start(10));
    EXPECT_EQ(&threadC, next);
    EXPECT_EQ(Time{13}, preemption);
  }
}

}  // namespace
