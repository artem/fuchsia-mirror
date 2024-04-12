// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fidl/fuchsia.tee/cpp/wire.h>
#include <lib/async/default.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fdf/dispatcher.h>
#include <zircon/errors.h>
#include <zircon/syscalls/object.h>
#include <zircon/threads.h>

#include <array>
#include <cinttypes>
#include <memory>

#include <ddktl/fidl.h>

#include "log.h"

namespace amlogic_secure_mem {

zx_status_t AmlogicSecureMemDevice::Create(void* ctx, zx_device_t* parent) {
  std::unique_ptr<AmlogicSecureMemDevice> sec_mem(new AmlogicSecureMemDevice(parent));

  zx_status_t status = sec_mem->Bind();
  if (status == ZX_OK) {
    // devmgr should now own the lifetime
    [[maybe_unused]] auto ptr = sec_mem.release();
  }

  return status;
}

zx_status_t AmlogicSecureMemDevice::Bind() {
  zx_status_t status = ZX_OK;
  status = ddk::PDevFidl::FromFragment(parent(), &pdev_proto_client_);
  if (status != ZX_OK) {
    LOG(ERROR, "Unable to get pdev protocol - status: %d", status);
    return status;
  }

  zx::result sysmem =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::Sysmem>(parent(), "sysmem");
  if (sysmem.is_error()) {
    LOG(ERROR, "Failed to get sysmem protocol: %s", sysmem.status_string());
    return sysmem.status_value();
  }
  sysmem_.Bind(std::move(sysmem.value()));

  zx::result client_end =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_tee::Service::DeviceConnector>("tee");
  if (client_end.is_error()) {
    LOG(ERROR, "Unable to connect to fidl protocol - status: %d", client_end.status_value());
    return client_end.status_value();
  }
  tee_proto_client_.Bind(std::move(client_end.value()));

  // See note on the constraints of |bti_| in the header.
  constexpr uint32_t kBtiIndex = 0;
  status = pdev_proto_client_.GetBti(kBtiIndex, &bti_);
  if (status != ZX_OK) {
    LOG(ERROR, "Unable to get bti handle - status: %d", status);
    return status;
  }

  status = CreateAndServeSysmemTee();
  if (status != ZX_OK) {
    LOG(ERROR, "CreateAndServeSysmemTee() failed - status: %d", status);
    return status;
  }

  status = DdkAdd(kDeviceName);
  if (status != ZX_OK) {
    LOG(ERROR, "Failed to add device");
    return status;
  }

  return status;
}

// TODO(https://fxbug.dev/42112465): Determine if we only ever use mexec to reboot from zedboot into
// a netboot(ed) image. Iff so, we could avoid some complexity here by not loading aml-securemem in
// zedboot, and not handling suspend(mexec) here, and not having UnregisterSecureMem().
void AmlogicSecureMemDevice::DdkSuspend(ddk::SuspendTxn txn) {
  LOG(DEBUG, "aml-securemem: begin DdkSuspend() - Suspend Reason: %d", txn.suspend_reason());

  if ((txn.suspend_reason() & DEVICE_MASK_SUSPEND_REASON) != DEVICE_SUSPEND_REASON_MEXEC) {
    // When a driver doesn't set a suspend function, the default impl returns
    // ZX_OK.
    txn.Reply(ZX_OK, txn.requested_state());
    return;
  }

  // Sysmem loads first (by design, to maximize chance of getting contiguous
  // memory), and aml-securemem depends on sysmem.  This means aml-securemem
  // will suspend before sysmem, so we have aml-securemem clean up secure memory
  // during its suspend (instead of sysmem trying to call aml-securemem after
  // aml-securemem has already suspended).
  ZX_DEBUG_ASSERT((txn.suspend_reason() & DEVICE_MASK_SUSPEND_REASON) ==
                  DEVICE_SUSPEND_REASON_MEXEC);

  // If the server is running, rendezvous with server shutdown and suspend asynchronously.
  if (sysmem_secure_mem_server_.has_value()) {
    is_suspend_mexec_ = true;
    suspend_txn_.emplace(std::move(txn));

    // We are shutting down the sysmem_secure_mem_server_ intentionally before any channel close. In
    // this case, tell sysmem that all is well, before the sysmem_secure_mem_server_ closes the
    // channel (which sysmem would otherwise intentionally interpret as justifying a hard reboot).
    LOG(DEBUG, "Sending UnregisterSecureMem request");
    fidl::WireResult result = sysmem_->UnregisterSecureMem();
    LOG(DEBUG, "Received UnregisterSecureMem response");
    if (!result.ok()) {
      LOG(ERROR, "Failed to send UnregisterSecureMem request: %s", result.status_string());
      txn.Reply(result.status(), txn.requested_state());
      return;
    }
    if (result->is_error()) {
      LOG(ERROR, "Failed to unregister secure mem: %s",
          zx_status_get_string(result->error_value()));
    }

    sysmem_secure_mem_server_.AsyncCall(&SysmemSecureMemServer::Unbind);
    // Suspend will continue at |SysmemSecureMemServerOnUnbound|.
    return;
  }

  LOG(DEBUG, "aml-securemem: end DdkSuspend()");
  txn.Reply(ZX_OK, txn.requested_state());
}

void AmlogicSecureMemDevice::SysmemSecureMemServerOnUnbound(bool is_success) {
  // We can assert this because we set up the call to this method using `receiver_.Once`.
  ZX_DEBUG_ASSERT(fdf_dispatcher_get_current_dispatcher() == fdf_dispatcher_);
  // Else the current lambda wouldn't be running.
  ZX_DEBUG_ASSERT(sysmem_secure_mem_server_.has_value());

  if (!is_success) {
    // This unexpected loss of connection to sysmem should never happen.  Complain if it
    // does happen.
    //
    // TODO(dustingreen): Determine if there's a way to cause the aml-securemem's devhost
    // to get re-started cleanly.  Currently this is leaving the overall device in a state
    // where DRM playback will likely be impossible (we should never get here).
    //
    // We may or may not see this message, depending on whether the sysmem failure causes a
    // hard reboot first.
    LOG(ERROR, "fuchsia::sysmem::Tee channel close !is_success - DRM playback will fail");
  } else {
    // If is_success, that means the sysmem_secure_mem_server_ is being shut down
    // intentionally before any channel close.  So far, we only do this for suspend(mexec).
    // See the initiation logic in AmlogicSecureMemDevice::DdkSuspend.
    ZX_DEBUG_ASSERT(is_suspend_mexec_);
  }

  // Regardless of whether this is due to DdkSuspend() or unexpected channel closure, we
  // won't be serving the fuchsia::sysmem::Tee channel any more. Destroy the SysmemSecureMemServer.
  sysmem_secure_mem_server_.reset();
  LOG(DEBUG, "Done serving fuchsia::sysmem::Tee");

  if (suspend_txn_) {
    suspend_txn_->Reply(ZX_OK, suspend_txn_->requested_state());
    suspend_txn_.reset();
  }
}

void AmlogicSecureMemDevice::GetSecureMemoryPhysicalAddress(
    GetSecureMemoryPhysicalAddressRequestView request,
    GetSecureMemoryPhysicalAddressCompleter::Sync& completer) {
  auto result = GetSecureMemoryPhysicalAddress(std::move(request->secure_mem));
  if (result.is_error()) {
    completer.Reply(result.error(), static_cast<zx_paddr_t>(0));
  }

  completer.Reply(ZX_OK, result.value());
}

fpromise::result<zx_paddr_t, zx_status_t> AmlogicSecureMemDevice::GetSecureMemoryPhysicalAddress(
    zx::vmo secure_mem) {
  ZX_DEBUG_ASSERT(secure_mem.is_valid());
  ZX_ASSERT(bti_.is_valid());

  // Validate that the VMO handle passed meets additional constraints.
  zx_info_vmo_t secure_mem_info;
  zx_status_t status = secure_mem.get_info(ZX_INFO_VMO, reinterpret_cast<void*>(&secure_mem_info),
                                           sizeof(secure_mem_info), nullptr, nullptr);
  if (status != ZX_OK) {
    LOG(ERROR, "Failed to get VMO info - status: %d", status);
    return fpromise::error(status);
  }

  // Only allow pinning on VMOs that are contiguous.
  if ((secure_mem_info.flags & ZX_INFO_VMO_CONTIGUOUS) != ZX_INFO_VMO_CONTIGUOUS) {
    LOG(ERROR, "Received non-contiguous VMO type to pin");
    return fpromise::error(ZX_ERR_WRONG_TYPE);
  }

  // Pin the VMO to get the physical address.
  zx_paddr_t paddr;
  zx::pmt pmt;
  status = bti_.pin(ZX_BTI_CONTIGUOUS | ZX_BTI_PERM_READ, secure_mem, 0 /* offset */,
                    secure_mem_info.size_bytes, &paddr, 1u, &pmt);
  if (status != ZX_OK) {
    LOG(ERROR, "Failed to pin memory - status: %d", status);
    return fpromise::error(status);
  }

  // Unpinning the PMT should never fail
  status = pmt.unpin();
  ZX_DEBUG_ASSERT(status == ZX_OK);

  return fpromise::ok(paddr);
}

AmlogicSecureMemDevice::AmlogicSecureMemDevice(zx_device_t* device)
    : AmlogicSecureMemDeviceBase(device),
      fdf_dispatcher_(fdf_dispatcher_get_current_dispatcher()),
      receiver_(this, fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_)) {
  thrd_t thrd;
  sysmem_secure_mem_server_loop_.StartThread("sysmem_secure_mem_server_loop", &thrd);

  // Apply scheduler role to server thread.
  const char* kRoleName = "fuchsia.devices.securemem.drivers.aml-securemem.server-loop";
  const zx_status_t status =
      device_set_profile_by_role(device, thrd_get_zx_handle(thrd), kRoleName, strlen(kRoleName));
  if (status != ZX_OK) {
    LOG(WARNING,
        "Failed to apply role \"%s\" to dispatch thread: %s."
        " Dispatch thread will run at default priority.",
        kRoleName, zx_status_get_string(status));
  }
}

zx_status_t AmlogicSecureMemDevice::CreateAndServeSysmemTee() {
  ZX_DEBUG_ASSERT(tee_proto_client_.is_valid());

  auto [tee_client, tee_server] = fidl::Endpoints<fuchsia_tee::Application>::Create();
  sysmem_secure_mem_server_.emplace(async_patterns::PassDispatcher, tee_client.TakeChannel());

  const fuchsia_tee::wire::Uuid kSecmemUuid = {
      0x2c1a33c0, 0x44cc, 0x11e5, {0xbc, 0x3b, 0x00, 0x02, 0xa5, 0xd5, 0xc5, 0x1b}};

  {
    fidl::OneWayStatus result = tee_proto_client_->ConnectToApplication(
        kSecmemUuid, fidl::ClientEnd<::fuchsia_tee_manager::Provider>(), std::move(tee_server));
    if (!result.ok()) {
      LOG(ERROR, "optee: tee_client_.ConnectToApplication() failed - status: %d", result.status());
      return result.status();
    }
  }

  auto [sysmem_secure_mem_client, sysmem_secure_mem_server] =
      fidl::Endpoints<fuchsia_sysmem::SecureMem>::Create();

  sysmem_secure_mem_server_.AsyncCall(
      &SysmemSecureMemServer::Bind, std::move(sysmem_secure_mem_server),
      receiver_.Once(&AmlogicSecureMemDevice::SysmemSecureMemServerOnUnbound).ignore_result());

  // Tell sysmem about the fidl::sysmem::Tee channel that sysmem will use (async) to configure
  // secure memory ranges.  Sysmem won't fidl call back during this banjo call.
  {
    LOG(DEBUG, "Sending RegisterSecureMem request");
    fidl::OneWayStatus result = sysmem_->RegisterSecureMem(std::move(sysmem_secure_mem_client));
    LOG(DEBUG, "Received RegisterSecureMem response");
    if (!result.ok()) {
      LOG(ERROR, "Failed to send RegisterSecureMem request: %s", result.status_string());
      return result.status();
    }
  }

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AmlogicSecureMemDevice::Create;
  return ops;
}();

}  // namespace amlogic_secure_mem

ZIRCON_DRIVER(amlogic_secure_mem, amlogic_secure_mem::driver_ops, "zircon", "0.1");
