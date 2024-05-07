// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "kernel/semaphore.h"

#include <lib/fit/defer.h>
#include <lib/kconcurrent/chainlock.h>
#include <lib/kconcurrent/chainlock_transaction.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <kernel/auto_preempt_disabler.h>

// Every call to this method must wake one waiter unless the queue is empty.
//
// When leaving this method, we must have either
//   - incremented a non-negative count or
//   - unblocked one thread and
//     - left an empty queue with a count of zero or
//     - left a non-empty queue with negative count
void Semaphore::Post() {
  // When calling post, we know we cannot be holding any chain locks (since we
  // demand that there be no transaction in process). In addition, if they are
  // holding any spinlocks, they need to have disabled local preemption outside
  // of the outer-most scope of the spin-lock(s) they hold.  Otherwise, posting
  // to the semaphore might result in a local preemption while a spin-lock is
  // held, something which cannot be permitted.
  DEBUG_ASSERT_MSG(!Thread::Current::preemption_state().PreemptIsEnabled() ||
                       (READ_PERCPU_FIELD(num_spinlocks) == 0),
                   "Preemption %d Num Spinlocks %u\n",
                   Thread::Current::preemption_state().PreemptIsEnabled(),
                   READ_PERCPU_FIELD(num_spinlocks));

  for (bool finished = false; !finished;) {
    // Is the count greater than or equal to zero?  If so, increment and return.
    // Take care to not increment a negative count.
    int64_t old_count = count_.load(ktl::memory_order_relaxed);
    while (old_count >= 0) {
      if (count_.compare_exchange_weak(old_count, old_count + 1, ktl::memory_order_release,
                                       ktl::memory_order_relaxed)) {
        return;
      }
    }

    // The fast-path has failed, looks like we are going to need to obtain some
    // locks in order to proceed.  Create the CLT we will need to do so now.
    ChainLockTransactionPreemptDisableAndIrqSave clt{CLT_TAG("Semaphore::Wait")};
    auto finalize_clt = fit::defer([&clt]() TA_NO_THREAD_SAFETY_ANALYSIS { clt.Finalize(); });
    for (;; clt.Relax()) {
      // We observed a negative count and have not yet incremented it.  There might
      // be waiters waiting.  We'll need to acquire the lock and check.
      UnconditionalChainLockGuard guard{waitq_.get_lock()};

      // Because we hold the lock we know that no other thread can transition the
      // count from non-negative to negative or vice versa.  If the count is
      // non-negative, then there must be no waiters so just increment and return.
      old_count = count_.load(ktl::memory_order_relaxed);
      if (old_count >= 0) {
        [[maybe_unused]] uint32_t q;
        DEBUG_ASSERT_MSG((q = waitq_.Count()) == 0, "q=%u old_count=%ld\n", q, old_count);
        count_.fetch_add(1, ktl::memory_order_release);
        return;
      }

      // Because we hold the lock we know the number of waiters cannot change out
      // from under us.  At this point we know the count is negative, and it cannot become
      // non-negative without holding our queue's lock.  Check for waiters to wake.
      const uint32_t num_in_queue = waitq_.Count();
      if (num_in_queue == 0) {
        // There are no waiters to wake.  They must have timed out or been
        // interrupted.  Reset the count and perform our increment.
        count_.store(1, ktl::memory_order_release);
        return;
      }

      // At this point we know there's at least one waiter waiting.  We're committed
      // to waking.  However, we need to determine what the count should be when
      // drop the lock and return.  After we wake, if there are no waiters left, the
      // count should be reset to zero, otherwise it should remain negative.
      //
      // Try to wake one of the threads in the queue, but be ready for it to fail
      // because of a backoff error.
      ktl::optional<bool> wake_result = waitq_.WakeOneLocked(ZX_OK);
      if (!wake_result.has_value()) {
        continue;  // Drop all of the locks and start again.
      }
      DEBUG_ASSERT(wake_result.value());

      // Cancel our deferred finalize action.  WakeOneLocked has "finalize upon
      // success" semantics.  Basically, if it succeeds in obtaining all of the
      // required locks, it will finalize the existing transaction-in-process
      // using the CLT's static method.  If it fails, it will keep the
      // transaction un-finalized so we continue to consider the time spent
      // acquiring locks so far as part of the total transaction
      // lock-acquisition time.
      //
      // TODO(johngro): Another option here would be to skip the fit::defer and
      // just use a manual clt.Finalize() before each of the successful
      // early-return paths above.  I'm not certain which one is the proper
      // balance of safe, easy to maintain, and easy to understand, right now.
      finalize_clt.cancel();

      // If we just woke the last thread from the queue, set the negative count
      // back to zero.  We know that this is safe because we are still holding the
      // queue's lock.  Signaling threads cannot increment the count because it is
      // currently negative, and they cannot transition from negative to
      // non-negative without holding the queue's lock.  Likewise, Waiting threads
      // cannot decrement the count (again, because it is negative) without first
      // obtaining the queue's lock.  This holds true even if the thread we just
      // woke starts running on another CPU and attempts to alter the count
      // with either a post or a wait operation.
      if (num_in_queue == 1) {
        count_.store(0, ktl::memory_order_release);
      }
      return;
    }
  }
}

// Handling failed waits -- Waits can fail due to timeout or thread signal.
// When a Wait fails, the caller cannot proceed to the resource guarded by the
// semaphore.  It's as if the waiter gave up and left.  We want to ensure failed
// waits do not impact other waiters.  While it seems like a failed wait should
// simply "undo" its decrement, it is not safe to do so in Wait.  Instead, we
// "fix" the count in subsequent call to Post.
//
// To understand why we can't simply fix the count in Wait after returning from
// Block, let's look at an alternative (buggy) implementation of Post and Wait.
// In this hypothetical implementation, Post increments and Wait decrements
// before Block, but also increments if Block returns an error.  With this
// hypothetical implementation in mind, consider the following sequence of
// operations:
//
//    Q  C
//    0  0
// 1W 1 -1  B
// 1T 0 -1
// 2P 0  0
// 3W 1 -1  B
// 1R 1  0
//
// The way to read the sequence above is that the wait queue (Q) starts empty
// and the count (C) starts at zero.  Thread1 calls Wait (1W) and blocks (B).
// Thread1's times out (1T) and is removed from the queue, but has not yet
// resumed execution.  Thread2 calls Post (2P), but does not unblock any threads
// because it finds an empty queue. Thread3 calls Wait (3W) and blocks (B).
// Thread1 returns from Block, resumes execution (1R), and increments the count.
// At this point Thread3 is blocked as it should be (two Posts and one failed
// Wait), however, a subsequent call to Post will not unblock it.  We have a
// "lost wakeup".
zx_status_t Semaphore::Wait(const Deadline& deadline) {
  // Is the count greater than zero?  If so, decrement and return.  Take care to
  // not decrement zero or a negative value.
  int64_t old_count = count_.load(ktl::memory_order_relaxed);
  while (old_count > 0) {
    if (count_.compare_exchange_weak(old_count, old_count - 1, ktl::memory_order_acquire,
                                     ktl::memory_order_relaxed)) {
      return ZX_OK;
    }
  }

  // We either observed that count is zero or negative.  We have not
  // decremented yet.  We may or may not need to block, but we need to hold
  // our queue's lock before proceeding.
  ChainLockTransactionIrqSave clt{CLT_TAG("Semaphore::Wait")};
  auto finalize_clt = fit::defer([&clt]() TA_NO_THREAD_SAFETY_ANALYSIS { clt.Finalize(); });
  waitq_.get_lock().AcquireUnconditionally();

  // Because we hold the lock we know that no other thread can transition the
  // count from non-negative to negative or vice versa.  If we can decrement a
  // positive count, then we're done and don't need to block.
  old_count = count_.fetch_sub(1, ktl::memory_order_acquire);
  if (old_count > 0) {
    waitq_.get_lock().Release();
    return ZX_OK;
  }

  // We either decremented zero or a negative value.  We must block.  Grab our
  // thread's lock and do so.
  //
  // TODO(johngro): Is it safe to simply grab the thread's lock
  // unconditionally here?  I cannot (currently) think of a way that we might
  // need to encounter a scenario where we might need to legitimately back off
  // (even if we encounter a Backoff error), but if one _could_ exist, we
  // should be prepared to do so.
  //
  // Currently, this is the best argument I have for why this is safe.
  // 1) Our queue is a normal WaitQueue, not an OwnedWaitQueue.  Because of
  //    this, it must always be the target node of any PI graph it is involved
  //    in.
  // 2) The current thread is running (by definition), therefore it must also be
  //    the target node of any PI graph it is involved in.
  // 3) No one else can be involved in any operation which would add an edge
  //    from the current thread to another wait queue (only threads can block
  //    themselves in wait queues).
  //
  // We already have our queue's lock.  If there is another operation in flight
  // which currently held the thread's lock, it would also need to involve
  // holding our queue's lock in order to produce a scenario where the backoff
  // protocol was needed.  By the above arguments, the only such operation would
  // be blocking the current thread in this queue, which cannot happen because
  // of #3 (we are the current thread).
  //
  // Will this reasoning always hold, however?  For example, what if (some day)
  // there was an operation which wanted to globally hold all thread locks at
  // the same time as all wait queue locks?  While this does not seem likely,
  // I'm not sure how to effectively stop someone from accidentally making a
  // mistake like this.
  //
  Thread* const current_thread = Thread::Current::Get();
  UnconditionalChainLockGuard guard{current_thread->get_lock()};
  finalize_clt.call();
  return waitq_.Block(current_thread, deadline, Interruptible::Yes);
}
