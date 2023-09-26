// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_PAGER_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_PAGER_DISPATCHER_H_

#include <zircon/types.h>

#include <object/dispatcher.h>
#include <object/handle.h>
#include <object/pager_proxy.h>
#include <object/port_dispatcher.h>

class PagerDispatcher final : public SoloDispatcher<PagerDispatcher, ZX_DEFAULT_PAGER_RIGHTS> {
 public:
  static zx_status_t Create(KernelHandle<PagerDispatcher>* handle, zx_rights_t* rights);
  ~PagerDispatcher() final;

  zx_status_t CreateSource(fbl::RefPtr<PortDispatcher> port, uint64_t key, uint32_t options,
                           fbl::RefPtr<PageSource>* src_out);
  // Drop and return this object's reference to |proxy|. Must be called under
  // |proxy|'s lock to prevent races with dispatcher teardown.
  fbl::RefPtr<PagerProxy> ReleaseProxy(PagerProxy* proxy) TA_REQ(proxy->mtx_);

  zx_status_t RangeOp(uint32_t op, fbl::RefPtr<VmObject> vmo, uint64_t offset, uint64_t length,
                      uint64_t data);

  zx_status_t QueryDirtyRanges(fbl::RefPtr<VmObject> vmo, uint64_t offset, uint64_t length,
                               user_out_ptr<void> buffer, size_t buffer_size,
                               user_out_ptr<size_t> actual, user_out_ptr<size_t> avail);

  zx_status_t QueryPagerVmoStats(fbl::RefPtr<VmObject> vmo, uint32_t options,
                                 user_out_ptr<void> buffer, size_t buffer_size);

  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_PAGER; }

  void on_zero_handles() final;

 private:
  explicit PagerDispatcher();

  mutable DECLARE_MUTEX(PagerDispatcher) lock_;
  fbl::DoublyLinkedList<fbl::RefPtr<PagerProxy>> proxies_ TA_GUARDED(lock_);
  // Track whether zero handles has been triggered. This prevents race conditions where we might
  // create new sources after on_zero_handles has been called.
  bool triggered_zero_handles_ TA_GUARDED(lock_) = false;
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_PAGER_DISPATCHER_H_
