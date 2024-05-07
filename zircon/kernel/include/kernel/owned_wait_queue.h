// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_OWNED_WAIT_QUEUE_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_OWNED_WAIT_QUEUE_H_

#include <lib/kconcurrent/chainlock.h>

#include <fbl/canary.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/macros.h>
#include <kernel/scheduler_state.h>
#include <kernel/thread.h>
#include <kernel/wait.h>
#include <ktl/optional.h>
#include <ktl/variant.h>

// Helpers used to identify which locks need to be held for various templated PI
// interactions which could be operating on a Thread->Thread, Thread->OWQ,
// OWQ->Thread or OWQ->OWQ.
namespace internal {
inline const ChainLock& GetPiNodeLock(const Thread& node);
inline const ChainLock& GetPiNodeLock(const WaitQueue& node);
inline const ChainLock& GetPiNodeLock(const OwnedWaitQueue& node);
}  // namespace internal

// fwd decl so we can be friends with our tests.
struct OwnedWaitQueueTopologyTests;

// Owned wait queues are an extension of wait queues which adds the concept of
// ownership for use when profile inheritance semantics are needed.
//
// An owned wait queue maintains an unmanaged pointer to a Thread in order to
// track who owns it at any point in time.  In addition, it contains node state
// which can be used by the owning thread in order to track the wait queues that
// the thread is currently an owner of.  This also makes use of unmanaged
// pointer.
//
// It should be an error for any thread to destruct while it owns an
// OwnedWaitQueue.  Likewise, it should be an error for any wait queue to
// destruct while it has an owner.  These invariants are enforced in the
// destructor for OwnedWaitQueue and Thread.  This enforcement is considered
// to be the reasoning why holding unmanaged pointers is considered to be safe.
//
class OwnedWaitQueue : protected WaitQueue, public fbl::DoublyLinkedListable<OwnedWaitQueue*> {
 public:
  // We want to limit access to our base WaitQueue's methods, but not all of
  // them.  Make public the methods which should be safe for folks to use from
  // the OwnedWaitQueue level of things.
  //
  // This list is pretty short right now, and there are probably other methods
  // which could be added safely (WakeOne, WakeAll, Peek, etc...) we'd rather
  // keep the list as short as possible for now, and only expand it when there
  // is a tangible need, and a thorough review for safety.
  //
  // The general rule of thumb here is that code which knows that it using an
  // OwnedWaitQueue should go through the OWQ specific APIs instead of
  // attempting to use the base WaitQueue APIs.
  using WaitQueue::Count;
  using WaitQueue::get_lock;
  using WaitQueue::IsEmpty;

  struct IWakeRequeueHook {
   public:
    IWakeRequeueHook() = default;

    // Note; do not force a virtual destructor here.  This interface exists
    // only for the purpose of injecting into various wake operations to be
    // used during thread selection and to update external bookkeeping.
    // Instances of it are meant to be stack allocated and should always be
    // destroyed in the scope where they were created.  They are never meant to
    // be heap allocated or have their lifecycle managed using an IWakeRequeueHook
    // pointer.

    IWakeRequeueHook(const IWakeRequeueHook&) = delete;
    IWakeRequeueHook(IWakeRequeueHook&&) = delete;
    IWakeRequeueHook& operator=(const IWakeRequeueHook&) = delete;
    IWakeRequeueHook& operator=(IWakeRequeueHook&&) = delete;

    // The Allow hook is called during the locking phase of a wake or a requeue
    // operation.  The locking code is holding the queue's lock at this point,
    // but not the thread's lock yet.  The thread is still a member of the
    // queue, meaning we have read-only access to the thread's variables.
    //
    // The implementation may use this hook to decide if this thread should be
    // woken or not, returning true if it should be and false otherwise.  If
    // true is returned, the locking operation will attempt to lock the thread
    // in order to commit it to waking, backing of if needed.  If false is
    // returned, the thread will be left in the queue and the enumeration of
    // threads in the queue ceases.
    //
    // Allow may be called multiple times for the same thread during a locking
    // operation if the code needs to back off and attempt to obtain lock the
    // set of threads to be woken multiple times.
    virtual bool Allow(const Thread& t) TA_REQ_SHARED(t.get_lock()) { return true; }

    // OnWakeOrRequeue is called in the second phase of a wake/requeue
    // operation.  At this point, all of the threads to wake have been
    // identified and successfully locked.  This hook is called _just_ before
    // the thread is removed from its queue, and either unblocked (wake
    // operation) or moved to a different queue (requeue).
    virtual void OnWakeOrRequeue(Thread& t) TA_REQ(t.get_lock()) {}

   protected:
    ~IWakeRequeueHook() = default;
  };

  static IWakeRequeueHook& default_wake_hooks() { return default_wake_hooks_; }

  // A small struct which contains details about the locking used in order to
  // prepare for a BlockAndAssignOwner operation.  See the comments in
  // LockForBAAOOperation for details.
  struct BAAOLockingDetails {
    explicit BAAOLockingDetails(Thread* initial_new_owner) : new_owner{initial_new_owner} {}

    Thread* new_owner;
    const void* stop_point{nullptr};
  };

  using ReplaceOwnerLockingDetails = struct BAAOLockingDetails;

  struct WakeRequeueThreadDetails {
    Thread::UnblockList wake_threads{};
    Thread::UnblockList requeue_threads{};
  };

  struct RequeueLockingDetails {
    // No copy, yes move (because of the unblock lists)
    RequeueLockingDetails() = default;
    RequeueLockingDetails(const RequeueLockingDetails&) = delete;
    RequeueLockingDetails& operator=(const RequeueLockingDetails&) = delete;
    RequeueLockingDetails(RequeueLockingDetails&&) = default;
    RequeueLockingDetails& operator=(RequeueLockingDetails&&) = default;

    Thread* new_requeue_target_owner{nullptr};
    const void* new_requeue_target_owner_stop_point{nullptr};
    const void* old_wake_queue_owner_stop_point{nullptr};
    const void* old_requeue_target_owner_stop_point{nullptr};
    WakeRequeueThreadDetails threads{};
  };

  // A structure which holds the state of the queue as observed just before the
  // queue lock was dropped.  Used by futex and kernel mutex implementations to
  // update their bookkeeping before unwinding after a wake operation.
  struct WakeThreadsResult {
    uint32_t woken{0};
    uint32_t still_waiting{0};
    Thread* owner{nullptr};
  };

  // A enum which determines the specific behavior of the Propagate method.
  enum class PropagateOp {
    // Add a single edge from the upstream node to the downstream node, and
    // propagate the effects. This happens either when a thread blocks in an
    // OWQ, or when an owner is assigned to an OWQ.
    AddSingleEdge,

    // Remove a single edge from the upstream node to the downstream node. This
    // happens either when a thread unblocks from an OWQ, or when an OWQ becomes
    // unowned.
    RemoveSingleEdge,

    // The base profile of an upstream node has changed, causing a change to the
    // profile pressure without an explicit join or split operation.
    BaseProfileChanged,
  };

  template <PropagateOp Op>
  struct PropagateOpTag {
    constexpr operator PropagateOp() const { return Op; }
  };

  // Behavior control for wake/requeue operations
  enum class WakeOption { None, AssignOwner };

  static constexpr PropagateOpTag<PropagateOp::AddSingleEdge> AddSingleEdgeOp{};
  static constexpr PropagateOpTag<PropagateOp::RemoveSingleEdge> RemoveSingleEdgeOp{};
  static constexpr PropagateOpTag<PropagateOp::BaseProfileChanged> BaseProfileChangedOp{};

  static constexpr uint32_t kOwnedMagic = fbl::magic("ownq");
  constexpr OwnedWaitQueue() : WaitQueue(kOwnedMagic) {}
  ~OwnedWaitQueue();

  // No copy or move is permitted.
  DISALLOW_COPY_ASSIGN_AND_MOVE(OwnedWaitQueue);

  // Release ownership of all wait queues currently owned by |t| and update
  // bookkeeping as appropriate.  This is meant to be called from the thread
  // itself and therefore it is assumed that the thread in question is not
  // blocked on any other wait queues.
  static void DisownAllQueues(Thread* t) TA_EXCL(chainlock_transaction_token, t->get_lock());

  // Change a thread's base profile and deal with profile propagation side effects.
  static void SetThreadBaseProfileAndPropagate(Thread& thread,
                                               const SchedulerState::BaseProfile& profile)
      TA_EXCL(chainlock_transaction_token, thread.get_lock());

  // Attempt to lock the rest of a PI chain, starting from the (already locked)
  // node given by |start|.
  static ChainLock::LockResult LockPiChain(Thread& start)
      TA_REQ(chainlock_transaction_token, start.get_lock()) {
    WaitQueue* wq = start.wait_queue_state().blocking_wait_queue_;

    if (wq == nullptr) {
      return ChainLock::LockResult::kOk;
    }

    // Note: If we refuse cycles, the only possible variant return type for
    // LockPiChainCommon is ChainLock::LockResult.
    return LockPiChainCommonRefuseCycle(*wq);
  }

  static ChainLock::LockResult LockPiChain(WaitQueue& start)
      TA_REQ(chainlock_transaction_token, start.get_lock()) {
    Thread* t = GetQueueOwner(&start);

    if (t == nullptr) {
      return ChainLock::LockResult::kOk;
    }

    return LockPiChainCommonRefuseCycle(*t);
  }

  // const accessor for the owner member.
  Thread* owner() const TA_REQ(get_lock()) { return owner_; }

  // Debug Assert wrapper which skips the thread analysis checks just to
  // assert that a specific queue is unowned.  Used by FutexContext
  void AssertNotOwned() const TA_NO_THREAD_SAFETY_ANALYSIS { DEBUG_ASSERT(owner_ == nullptr); }

  // Assign ownership of this wait queue to |new_owner|, or explicitly release
  // ownership if |new_owner| is nullptr.  No change is made to ownership if it
  // would result in a cycle in the inheritance graph.
  //
  // Note, if the new owner exists, but is dead or dying, it will not be
  // permitted to become the new owner of the wait_queue.  Any existing owner
  // will be replaced with no owner in this situation.
  //
  // Returns ZX_ERR_BAD_STATE if a cycle would have been produced, and ZX_OK
  // otherwise.
  zx_status_t AssignOwner(Thread* new_owner) TA_EXCL(chainlock_transaction_token, get_lock());

  void ResetOwnerIfNoWaiters() TA_EXCL(chainlock_transaction_token, get_lock());

  // Block the current thread on this wait queue, and re-assign ownership to
  // the specified thread (or remove ownership if new_owner is null);  If a
  // cycle would have been produced by this operation, no changes are made and
  // ZX_ERR_BAD_STATE will be returned.
  //
  // Note, if the new owner exists, but is dead or dying, it will not be
  // permitted to become the new owner of the wait_queue.  Any existing owner
  // will be replaced with no owner in this situation.
  zx_status_t BlockAndAssignOwner(const Deadline& deadline, Thread* new_owner,
                                  ResourceOwnership resource_ownership, Interruptible interruptible)
      TA_EXCL(chainlock_transaction_token) TA_REQ(preempt_disabled_token);

  zx_status_t BlockAndAssignOwnerLocked(Thread* const current_thread, const Deadline& deadline,
                                        const BAAOLockingDetails& details,
                                        ResourceOwnership resource_ownership,
                                        Interruptible interruptible)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock(), preempt_disabled_token)
          TA_REL(get_lock());

  // Cancel a block and assign owner operation that we have already locked for.
  // Basically, just drop all of the locks and get out.
  void CancelBAAOOperationLocked(Thread* const current_thread, const BAAOLockingDetails& details)
      TA_REQ(chainlock_transaction_token) TA_REL(get_lock(), current_thread->get_lock());

  // Obtain all of the locks needed for a BlockAndAssignOwner operation,
  // handling the special case of new+old owner which share a PI graph target.
  BAAOLockingDetails LockForBAAOOperation(Thread* const current_thread, Thread* new_owner)
      TA_REQ(chainlock_transaction_token) TA_ACQ(get_lock(), current_thread->get_lock());

  // Attempt to obtain all of the locks needed for a BAAO operation while
  // already holding this queue's lock.  On success, the current thread's lock
  // will have been obtained in addition to any other locks in the PI chain
  // which are needed, and the details of the locking operation will be
  // returned.  On failure, returns ktl::nullopt after dropping any locks which
  // had been obtained.
  ktl::optional<BAAOLockingDetails> LockForBAAOOperationLocked(Thread* const current_thread,
                                                               Thread* new_owner)
      TA_REQ(chainlock_transaction_token, get_lock());

  // Wake the up to specified number of threads from the wait queue, removing
  // any current queue owner in the process.
  WakeThreadsResult WakeThreads(uint32_t wake_count,
                                IWakeRequeueHook& wake_hooks = default_wake_hooks_)
      TA_EXCL(chainlock_transaction_token, get_lock());

  // Attempt to wake exactly one thread, and assign ownership of the queue to
  // the woken thread (if any).
  WakeThreadsResult WakeThreadAndAssignOwner(IWakeRequeueHook& wake_hooks = default_wake_hooks_)
      TA_EXCL(chainlock_transaction_token, get_lock());

  WakeThreadsResult WakeThreadsLocked(Thread::UnblockList threads, IWakeRequeueHook& wake_hooks,
                                      WakeOption option = WakeOption::None)
      TA_REQ(chainlock_transaction_token, get_lock(), preempt_disabled_token);

  // Obtain all of the locks needed for a WakeThreads operation.
  Thread::UnblockList LockForWakeOperation(uint32_t max_wake, IWakeRequeueHook& wake_hooks)
      TA_REQ(chainlock_transaction_token) TA_ACQ(get_lock());

  ktl::optional<Thread::UnblockList> LockForWakeOperationLocked(uint32_t max_wake,
                                                                IWakeRequeueHook& wake_hooks)
      TA_REQ(chainlock_transaction_token, get_lock());

  WakeThreadsResult WakeAndRequeue(OwnedWaitQueue& requeue_target, Thread* new_requeue_owner,
                                   uint32_t wake_count, uint32_t requeue_count,
                                   IWakeRequeueHook& wake_hooks, IWakeRequeueHook& requeue_hooks,
                                   WakeOption wake_option)
      TA_EXCL(chainlock_transaction_token, get_lock(), requeue_target.get_lock());

  // Accessor used only by the scheduler's PiNodeAdapter to handle bookkeeping
  // during profile inheritance situations.
  SchedulerState::WaitQueueInheritedSchedulerState* inherited_scheduler_state_storage() {
    return inherited_scheduler_state_storage_;
  }

 private:
  // Give permission to the WaitQueue thunk to call the PropagateRemove method
  friend zx_status_t WaitQueue::UnblockThread(Thread* t, zx_status_t wait_queue_error);
  friend struct OwnedWaitQueueTopologyTests;

  struct DefaultWakeHooks : public IWakeRequeueHook {};
  static inline DefaultWakeHooks default_wake_hooks_;

  // Behavior control for locking pi paths.
  enum class LockingBehavior { RefuseCycle, StopAtCycle };

  // Assert that a thread (which we have locked exclusively) owns a specific
  // wait queue, meaning that we should be able to examine parts of that queue's
  // state (like, whether or not it has any blocked threads, and what the IPVs
  // of the queue currently are).  See comments in ApplyIpvDeltaToThread for
  // details.
  static inline void AssertOwnsWaitQueue(const Thread& t, const OwnedWaitQueue& owq)
      TA_REQ(t.get_lock()) TA_ASSERT_SHARED(owq.get_lock()) {
    [&]() TA_NO_THREAD_SAFETY_ANALYSIS { DEBUG_ASSERT(owq.owner_ == &t); }();
  }

  // Note that the locks for the entire PI chain starting from this OWQ, as well
  // as from the one starting from the new owner, need to be held at this point.
  void AssignOwnerInternal(Thread* new_owner)
      TA_REQ(chainlock_transaction_token, get_lock(), preempt_disabled_token);

  // A specialization of WakeThreads which will...
  //
  // 1) Wake the number of threads indicated by |wake_count|
  // 2) Move the number of threads indicated by |requeue_count| to the |requeue_target|.
  // 3) Update ownership bookkeeping as indicated by |owner_action| and |requeue_owner|.
  //
  // This method is used by futexes in order to implement futex_requeue.  It
  // is wrapped up into a specialized form instead of broken into individual
  // parts in order to minimize any thrash in re-computing effective
  // profiles for PI purposes.  We don't want to re-evaluate ownership or PI
  // pressure until after all of the changes to wait queue have taken place.
  //
  // Note, if the |requeue_owner| exists, but is dead or dying, it will not be
  // permitted to become the new owner of the |requeue_target|.  Any existing
  // owner will be replaced with no owner in this situation.
  WakeThreadsResult WakeAndRequeueInternal(RequeueLockingDetails& details,
                                           OwnedWaitQueue& requeue_target,
                                           IWakeRequeueHook& wake_hooks,
                                           IWakeRequeueHook& requeue_hooks,
                                           WakeOption wake_option = WakeOption::None)
      TA_REL(get_lock(), requeue_target.get_lock())
          TA_REQ(chainlock_transaction_token, preempt_disabled_token);

  void ValidateSchedStateStorageUnconditional() TA_REQ(get_lock());
  void ValidateSchedStateStorage() TA_REQ(get_lock()) {
    if constexpr (kSchedulerExtraInvariantValidation) {
      ValidateSchedStateStorageUnconditional();
    }
  }

  void UpdateSchedStateStorageThreadRemoved(Thread& t) TA_REQ(get_lock(), t.get_lock()) {
    DEBUG_ASSERT(inherited_scheduler_state_storage_ != nullptr);

    SchedulerState::WaitQueueInheritedSchedulerState& old_iss =
        t.wait_queue_state().inherited_scheduler_state_storage_;
    if (&old_iss == inherited_scheduler_state_storage_) {
      inherited_scheduler_state_storage_ = collection_.FindInheritedSchedulerStateStorage();
      if (inherited_scheduler_state_storage_) {
        DEBUG_ASSERT(&old_iss != inherited_scheduler_state_storage_);
        *inherited_scheduler_state_storage_ = old_iss;
      }
      old_iss.Reset();
    }
  }

  void UpdateSchedStateStorageThreadAdded(Thread& t) TA_REQ(get_lock(), t.get_lock()) {
    if (inherited_scheduler_state_storage_ == nullptr) {
      DEBUG_ASSERT_MSG(this->Count() == 1, "Expected count == 1, instead of %u", this->Count());
      inherited_scheduler_state_storage_ = &t.wait_queue_state().inherited_scheduler_state_storage_;
      inherited_scheduler_state_storage_->AssertIsReset();
    } else {
      DEBUG_ASSERT_MSG(this->Count() > 1, "Expected count > 1, instead of %u", this->Count());
    }
  }

  // Helper function for propagating inherited profile value add and remove
  // operations.  Snapshots and returns the combination of a thread's currently
  // inherited profile values along with its base profile, which should be the
  // profile pressure it is transmitting to the next node in the graph.
  static SchedulerState::InheritedProfileValues SnapshotThreadIpv(Thread& thread)
      TA_REQ(thread.get_lock());

  // Apply a change in IPV to a thread.  When non-null, |old_ipv| points to the
  // IPV values which need to be removed from the thread's IPVs, while |new_ipv|
  // points to the IPV vales which need to be added to the  thread's IPVs.
  //
  // 1) When a graph edge is added, |old_ipv| will be nullptr as no IPV pressure
  //    was removed.
  // 2) When a graph edge is removed, |new_ipv| will be nullptr as no IPV
  //    pressure was added.
  // 3) When a thread's base profile changes, neither |old_ipv| nor |new_ipv|
  //    will be nullptr, since the change of a thread's base profile is
  //    equivalent to the removal of one set of IPV, and the addition of
  //    another.
  // 4) It is never correct for both |old_ipv| and |new_ipv| to be nullptr.
  //
  static void ApplyIpvDeltaToThread(const SchedulerState::InheritedProfileValues* old_ipv,
                                    const SchedulerState::InheritedProfileValues* new_ipv,
                                    Thread& thread) TA_REQ(thread.get_lock());

  // Apply a change in IPV to an owned wait queue.  See ApplyIpvDeltaToThread
  // for an explanation of the parameters.
  static void ApplyIpvDeltaToOwq(const SchedulerState::InheritedProfileValues* old_ipv,
                                 const SchedulerState::InheritedProfileValues* new_ipv,
                                 OwnedWaitQueue& owq) TA_REQ(owq.get_lock());

  // Begin a propagation operation starting from an upstream thread, and going
  // through a downstream owned wait queue.  Only for use during edge add/remove
  // operations. Base profile changes call FinishPropagate directly when
  // required.  Note: Links between the upstream thread and downstream queue
  // should have already been added/removed by the time that this method is
  // called.
  //
  // The entire PI chain starting from the wait queue needs to be locked before
  // calling this, and the contents of the chain after the starting thread will
  // be released during the operation (the thread will remain locked).
  template <PropagateOp Op>
  static void BeginPropagate(Thread& upstream_node, OwnedWaitQueue& downstream_node,
                             PropagateOpTag<Op>)
      TA_REQ(chainlock_transaction_token, preempt_disabled_token, upstream_node.get_lock(),
             downstream_node.get_lock());

  // Begin a propagation operation starting from an upstream owned wait queue,
  // and going through a downstream thread.  Only for use during edge add/remove
  // operations. Base profile changes call FinishPropagate directly when
  // required.  Note: Links between the upstream queue and downstream thread
  // should *not* have already been added/removed by the time that this method
  // is called.  The method will handle updating the links.
  //
  // The entire PI chain starting from the wait queue needs to be locked before
  // calling this, and the entire chain will be released during the operation.
  //
  // Additionally, the upstream node must be an instance of an OwnedWaitQueue.
  // It is passed as a simple WaitQueue instead of an OwnedWaitQueue just to
  // work around some static analyzer issues.
  template <PropagateOp Op>
  static void BeginPropagate(OwnedWaitQueue& upstream_node, Thread& downstream_node,
                             PropagateOpTag<Op>)
      TA_REQ(chainlock_transaction_token, preempt_disabled_token, upstream_node.get_lock(),
             downstream_node.get_lock());

  // Finishing handling a propagation operations started from either version of
  // BeginPropagate, or from SetThreadBasePriority.  Traverses the PI chain
  // propagating IPV deltas, and calls into the scheduler to finish the
  // operation once the end of the chain is reached.
  template <PropagateOp Op, typename UpstreamNodeType, typename DownstreamNodeType>
  static void FinishPropagate(UpstreamNodeType& upstream_node, DownstreamNodeType& downstream_node,
                              const SchedulerState::InheritedProfileValues* added_ipv,
                              const SchedulerState::InheritedProfileValues* lost_ipv,
                              PropagateOpTag<Op>)
      TA_REQ(chainlock_transaction_token, internal::GetPiNodeLock(upstream_node),
             internal::GetPiNodeLock(downstream_node), preempt_disabled_token);

  // Upcast from a WaitQueue to an OwnedWaitQueue if possible, using the magic
  // number to detect the underlying nature of the object.  Returns nullptr if
  // the pointer passed is nullptr, or if the object is not an OwnedWaitQueue.
  static OwnedWaitQueue* DowncastToOwq(WaitQueue* wq) {
    return (wq != nullptr) && (wq->magic() == kOwnedMagic) ? static_cast<OwnedWaitQueue*>(wq)
                                                           : nullptr;
  }

  static Thread* GetQueueOwner(WaitQueue* wq) TA_REQ(chainlock_transaction_token, wq->get_lock()) {
    const OwnedWaitQueue* owq = DowncastToOwq(wq);

    if (owq != nullptr) {
      // The static analyzer is not clever enough to know that wq->get_lock() is
      // the same lock as owq->get_lock(), so we make it happy using a no-op
      // assert.
      owq->get_lock().MarkHeld();
      return owq->owner_;
    }

    return nullptr;
  }

  RequeueLockingDetails LockForRequeueOperation(OwnedWaitQueue& requeue_target,
                                                Thread* new_requeue_target_owner, uint32_t max_wake,
                                                uint32_t max_requeue, IWakeRequeueHook& wake_hooks,
                                                IWakeRequeueHook& requeue_hooks)
      TA_REQ(chainlock_transaction_token) TA_ACQ(get_lock(), requeue_target.get_lock());

  ktl::variant<ChainLock::LockResult, ReplaceOwnerLockingDetails> LockForOwnerReplacement(
      Thread* new_owner, const Thread* blocking_thread = nullptr,
      bool propagate_new_owner_cycle_error = false) TA_REQ(chainlock_transaction_token, get_lock());

  ktl::optional<Thread::UnblockList> LockAndMakeWaiterListLocked(zx_time_t now, uint32_t max_count,
                                                                 IWakeRequeueHook& hooks)
      TA_REQ(chainlock_transaction_token, get_lock());

  ktl::optional<WakeRequeueThreadDetails> LockAndMakeWakeRequeueThreadListsLocked(
      zx_time_t now, uint32_t max_wake_count, IWakeRequeueHook& wake_hooks,
      uint32_t max_requeue_count, IWakeRequeueHook& requeue_hooks)
      TA_REQ(chainlock_transaction_token, get_lock());

  ktl::optional<Thread::UnblockList> TryLockAndMakeWaiterListLocked(zx_time_t now,
                                                                    uint32_t max_count,
                                                                    IWakeRequeueHook& hooks)
      TA_REQ(chainlock_transaction_token, get_lock());

  void UnlockAndClearWaiterListLocked(Thread::UnblockList list)
      TA_REQ(chainlock_transaction_token, get_lock());

  // Common handler for PI chain locking/unlocking
  template <LockingBehavior Behavior, typename StartNodeType>
  static ktl::variant<ChainLock::LockResult, const void*> LockPiChainCommon(StartNodeType& start)
      TA_REQ(chainlock_transaction_token);

  template <typename StartNodeType>
  static inline ChainLock::LockResult LockPiChainCommonRefuseCycle(StartNodeType& start)
      TA_REQ(chainlock_transaction_token) {
    return ktl::get<ChainLock::LockResult>(LockPiChainCommon<LockingBehavior::RefuseCycle>(start));
  }

  template <typename StartNodeType>
  static void UnlockPiChainCommon(StartNodeType& start, const void* stop_point = nullptr)
      TA_REQ(chainlock_transaction_token) TA_REL(internal::GetPiNodeLock(start));

  TA_GUARDED(get_lock()) Thread* owner_ = nullptr;

  // A pointer to a thread (which _must_ be a current member of this owned wait queue's)
  // collection which holds the current inherited scheduler state storage for
  // this wait queue.
  //
  // Anytime that a thread is added to this queue, if the collection was
  // empty, then the thread added becomes the new location for the storage.
  //
  // Anytime that a thread is removed from this queue, if the thread being
  // removed was the active location of the storage, then a new thread needs to
  // be selected and the values moved into the new storage location.
  //
  // The primary purpose of this indirection is to keep the OwnedWaitQueue object
  // relatively-lightweight.  The only time that a wait queue has defined
  // inherited scheduler state is when there are one or more threads blocked in
  // the queue, so storing this bookkeeping in a blocked thread object seems
  // reasonable.
  //
  // TODO(johngro): An alternative approach would be to copy the token-ante
  // system used by Theads and FutexContext.
  //
  // Pros: Easier to reason about code and reduced risk of accidental
  //       use-after-free.  Also, no need to copy bookkeeping from one thread to
  //       another when the currently active thread leaves the OwnedWaitQueue.
  // Cons: Storage of the free tokens would require some central pool system
  //       which would act as a central choke point, which might become a
  //       scalability issue on systems where the processor count is high.
  //
  SchedulerState::WaitQueueInheritedSchedulerState* inherited_scheduler_state_storage_{nullptr};
};

namespace internal {
inline const ChainLock& GetPiNodeLock(const Thread& node) TA_RET_CAP(node.get_lock()) {
  return node.get_lock();
}
inline const ChainLock& GetPiNodeLock(const WaitQueue& node) TA_RET_CAP(node.get_lock()) {
  return node.get_lock();
}
inline const ChainLock& GetPiNodeLock(const OwnedWaitQueue& node) TA_RET_CAP(node.get_lock()) {
  return node.get_lock();
}
}  // namespace internal

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_OWNED_WAIT_QUEUE_H_
