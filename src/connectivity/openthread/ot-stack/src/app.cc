// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "app.h"

#include <alarm.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/llcpp/server.h>
#include <lib/service/llcpp/service.h>
#include <lib/svc/dir.h>
#include <radio.h>
#include <zircon/compiler.h>
#include <zircon/status.h>

#include <fbl/auto_lock.h>
#include <openthread/tasklet.h>
#include <src/lib/files/file.h>

#define OT_STACK_ASSERT assert

namespace otstack {
namespace {
constexpr char kMigrationConfigPath[] = "/config/data/migration_config.json";

constexpr uint8_t kSpinelResetFrame[]{0x80, 0x06, 0x0};
}  // namespace

OtStackApp::LowpanSpinelDeviceFidlImpl::LowpanSpinelDeviceFidlImpl(OtStackApp& app) : app_(app) {}

void OtStackApp::ClientAllowanceInit() {
  client_outbound_allowance_ = kOutboundAllowanceInit;
  client_inbound_allowance_ = 0;
  (*binding_)->OnReadyForSendFrames(kOutboundAllowanceInit);
}

void OtStackApp::RadioAllowanceInit() {
  radio_inbound_allowance_ = kInboundAllowanceInit;
  radio_outbound_allowance_ = 0;

  // try to open the device
  auto fidl_result = device_client_ptr_->Open();
  if (fidl_result.status() != ZX_OK) {
    FX_LOGS(ERROR) << "FIDL error while sending open() req to ot-radio";
    Shutdown();
    return;
  }
  auto& result = fidl_result.value().result;
  if (result.is_err()) {
    FX_LOGS(DEBUG) << "ot-stack: radio returned err in spinel Open(): "
                   << static_cast<uint32_t>(result.err());
    return;
  }
  // send inbound allowance
  device_client_ptr_->ReadyToReceiveFrames(kInboundAllowanceInit);
}

void OtStackApp::HandleRadioOnReadyForSendFrame(uint32_t allowance) {
  radio_outbound_allowance_ += allowance;
}

void OtStackApp::HandleClientReadyToReceiveFrames(uint32_t allowance) {
  if (client_inbound_allowance_ == 0 && !client_inbound_queue_.empty()) {
    async::PostTask(loop_.dispatcher(), [this]() { this->SendOneFrameToClient(); });
  }
  client_inbound_allowance_ += allowance;
}

void OtStackApp::UpdateRadioOutboundAllowance() {
  OT_STACK_ASSERT(radio_outbound_allowance_ > 0);
  radio_outbound_allowance_--;
  radio_outbound_cnt++;
  FX_LOGS(DEBUG) << "ot-stack: updated radio_outbound_allowance_:" << radio_outbound_allowance_;
}

void OtStackApp::UpdateRadioInboundAllowance() {
  OT_STACK_ASSERT(radio_inbound_allowance_ > 0);
  radio_inbound_allowance_--;
  radio_inbound_cnt++;
  if (((radio_inbound_allowance_ & 1) == 0) && device_client_ptr_) {
    device_client_ptr_->ReadyToReceiveFrames(kInboundAllowanceInc);
    radio_inbound_allowance_ += kInboundAllowanceInc;
  }
  FX_LOGS(DEBUG) << "ot-stack: updated radio_inbound_allowance_:" << radio_inbound_allowance_;
}

void OtStackApp::UpdateClientOutboundAllowance() {
  OT_STACK_ASSERT(client_outbound_allowance_ > 0);
  client_outbound_allowance_--;
  client_outbound_cnt++;
  if (((client_outbound_allowance_ & 1) == 0) && device_client_ptr_) {
    FX_LOGS(DEBUG) << "ot-stack: OnReadyForSendFrames: " << client_outbound_allowance_;
    (*binding_)->OnReadyForSendFrames(kOutboundAllowanceInc);
    client_outbound_allowance_ += kOutboundAllowanceInc;
  }
  FX_LOGS(DEBUG) << "ot-stack: updated client_outbound_allowance_:" << client_outbound_allowance_;
}

void OtStackApp::UpdateClientInboundAllowance() {
  OT_STACK_ASSERT(client_inbound_allowance_ > 0);
  client_inbound_allowance_--;
  client_inbound_cnt++;
  FX_LOGS(DEBUG) << "ot-stack: updated client_inbound_allowance_:" << client_inbound_allowance_;
}

void OtStackApp::LowpanSpinelDeviceFidlImpl::Open(OpenCompleter::Sync& completer) {
  if (!app_.connected_to_device_) {
    FX_LOGS(ERROR) << "ot-radio not connected when client called Open()";
    completer.ReplyError(fidl_spinel::wire::Error::UNSPECIFIED);
    app_.Shutdown();
    return;
  }

  FX_LOGS(INFO) << "FIDL request Open got";

  async::PostTask(app_.loop_.dispatcher(), [this]() {
    otInstanceFinalize(static_cast<otInstance*>(app_.ot_instance_ptr_.value()));
    app_.ot_instance_ptr_.reset();
    otSysDeinit();
    this->app_.InitOpenThreadLibrary(true);
  });

  app_.ClientAllowanceInit();
  // Send out the reset frame
  app_.client_inbound_queue_.push_back(std::vector<uint8_t>{0x80, 0x06, 0x0, 0x70});
  completer.ReplySuccess();
}

void OtStackApp::LowpanSpinelDeviceFidlImpl::Close(CloseCompleter::Sync& completer) {
  if (!app_.connected_to_device_) {
    FX_LOGS(ERROR) << "ot-radio not connected";
    completer.ReplyError(fidl_spinel::wire::Error::UNSPECIFIED);
    app_.Shutdown();
    return;
  }
  auto fidl_result = app_.device_client_ptr_->Close();
  if (fidl_result.status() != ZX_OK) {
    FX_LOGS(ERROR) << "FIDL error while sending req to ot-radio";
    completer.ReplyError(fidl_spinel::wire::Error::UNSPECIFIED);
    app_.Shutdown();
    return;
  }
  completer.Reply(std::move(fidl_result.value().result));
}

void OtStackApp::LowpanSpinelDeviceFidlImpl::GetMaxFrameSize(
    GetMaxFrameSizeCompleter::Sync& completer) {
  if (!app_.connected_to_device_) {
    FX_LOGS(ERROR) << "ot-stack: ot-radio not connected";
    app_.Shutdown();
    return;
  }
  auto fidl_result = app_.device_client_ptr_->GetMaxFrameSize();
  if (fidl_result.status() != ZX_OK) {
    FX_LOGS(ERROR) << "ot-stack: FIDL error while sending req to ot-radio";
    app_.Shutdown();
    return;
  }
  completer.Reply(fidl_result.value().size);
}

void OtStackApp::PushFrameToOtLib() {
  assert(!client_outbound_queue_.empty());
  ot::Ncp::otNcpGetInstance()->HandleFidlReceiveDone(client_outbound_queue_.front().data(),
                                                     client_outbound_queue_.front().size());
  client_outbound_queue_.pop_front();
}

void OtStackApp::LowpanSpinelDeviceFidlImpl::SendFrame(::fidl::VectorView<uint8_t> data,
                                                       SendFrameCompleter::Sync& completer) {
  if (!app_.connected_to_device_) {
    FX_LOGS(ERROR) << "ot-radio not connected";
    return;
  }
  FX_LOGS(DEBUG) << "ot-stack: SendFrame() received";
  app_.UpdateClientOutboundAllowance();
  // Invoke ot-lib
  app_.client_outbound_queue_.emplace_back(data.cbegin(), data.cend());
  async::PostTask(app_.loop_.dispatcher(), [this]() { this->app_.PushFrameToOtLib(); });
}

void OtStackApp::LowpanSpinelDeviceFidlImpl::ReadyToReceiveFrames(
    uint32_t number_of_frames, ReadyToReceiveFramesCompleter::Sync& completer) {
  if (!app_.connected_to_device_) {
    FX_LOGS(ERROR) << "ot-radio not connected";
    return;
  }
  app_.HandleClientReadyToReceiveFrames(number_of_frames);
}

OtStackApp::OtStackCallBackImpl::OtStackCallBackImpl(OtStackApp& app) : app_(app) {}

// TODO (jiamingw): flow control, and timeout when it is unable to send out the packet
void OtStackApp::OtStackCallBackImpl::SendOneFrameToRadio(uint8_t* buffer, uint32_t size) {
  auto data = fidl::VectorView<uint8_t>::FromExternal(buffer, size);
  if (app_.radio_outbound_allowance_ == 0) {
    FX_LOGS(ERROR) << "ot-stack: radio_outbound_allowance_ is 0, cannot send packet";
    return;
  }
  app_.device_client_ptr_->SendFrame(std::move(data));
  app_.UpdateRadioOutboundAllowance();
}

std::vector<uint8_t> OtStackApp::OtStackCallBackImpl::WaitForFrameFromRadio(uint64_t timeout_us) {
  FX_LOGS(DEBUG) << "ot-stack-callbackform: radio-callback: waiting for frame";
  {
    fbl::AutoLock lock(&app_.radio_q_mtx_);
    if (app_.radio_inbound_queue_.empty()) {
      sync_completion_reset(&app_.radio_rx_complete_);
    } else {
      std::vector<uint8_t> vec = std::move(app_.radio_inbound_queue_.front());
      app_.radio_inbound_queue_.pop_front();
      return vec;
    }
  }
  zx_status_t res = sync_completion_wait(&app_.radio_rx_complete_, ZX_USEC(timeout_us));
  sync_completion_reset(&app_.radio_rx_complete_);
  if (res == ZX_ERR_TIMED_OUT) {
    // This method will be called multiple times by ot-lib. It is okay to timeout here.
    return std::vector<uint8_t>{};
  }
  if (res != ZX_OK) {
    FX_PLOGS(ERROR, res) << "ot-stack-callbackform: radio-callback: waiting frame end with err";
    return std::vector<uint8_t>{};
  }
  fbl::AutoLock lock0(&app_.radio_q_mtx_);
  assert(!app_.radio_inbound_queue_.empty());
  std::vector<uint8_t> vec = std::move(app_.radio_inbound_queue_.front());
  app_.radio_inbound_queue_.pop_front();
  return vec;
}

std::vector<uint8_t> OtStackApp::OtStackCallBackImpl::Process() {
  std::vector<uint8_t> vec;
  fbl::AutoLock lock(&app_.radio_q_mtx_);
  if (!app_.radio_inbound_queue_.empty()) {
    vec = std::move(app_.radio_inbound_queue_.front());
    app_.radio_inbound_queue_.pop_front();
    FX_LOGS(DEBUG) << "ot-stack-callbackform: radio-callback: check for frame: new frame";
  }
  return vec;
}

void OtStackApp::OtStackCallBackImpl::SendOneFrameToClient(uint8_t* buffer, uint32_t size) {
  if (memcmp(buffer, kSpinelResetFrame, sizeof(kSpinelResetFrame)) == 0) {
    // Reset frame
    FX_LOGS(WARNING) << "ot-stack: reset frame received from ot-radio";
    return;
  }
  app_.client_inbound_queue_.emplace_back(buffer, buffer + size);
  app_.SendOneFrameToClient();
}

void OtStackApp::OtStackCallBackImpl::PostNcpFidlInboundTask() {
  async::PostTask(app_.loop_.dispatcher(),
                  []() { ot::Ncp::otNcpGetInstance()->HandleFrameAddedToNcpBuffer(); });
}

void OtStackApp::OtStackCallBackImpl::PostOtLibTaskletProcessTask() {
  async::PostTask(app_.loop_.dispatcher(), [this]() {
    otTaskletsProcess(static_cast<otInstance*>(this->app_.ot_instance_ptr_.value()));
  });
}

void OtStackApp::OtStackCallBackImpl::PostDelayedAlarmTask(zx::duration delay) {
  async::PostDelayedTask(
      app_.loop_.dispatcher(), [this]() { this->app_.AlarmTask(); }, delay);
}

zx_status_t OtStackApp::InitOutgoingAndServe() {
  // Ensure that outgoing_ is not already initialized.
  // Current code cannot result in outgoing_ being reinitialized.
  // If there is a change in future and this gets called, that
  // may be a bug, as it may result in a unexpected behavior.
  OT_STACK_ASSERT(!outgoing_);

  async_dispatcher_t* dispatcher = loop_.dispatcher();
  outgoing_ = std::make_unique<svc::Outgoing>(dispatcher);
  return outgoing_->ServeFromStartupInfo();
}

zx_status_t OtStackApp::SetupBootstrapFidlService() {
  zx_status_t status = InitOutgoingAndServe();
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Init outgoing failed during SetupBootstrapFidlService: " << status
                   << std::endl;
    return status;
  }

  // Now add entry for bootstrap fidl:
  bootstrap_impl_ = std::make_unique<ot::Fuchsia::BootstrapThreadImpl>();

  status = outgoing_->svc_dir()->AddEntry(
      fidl::DiscoverableProtocolName<fuchsia_lowpan_bootstrap::Thread>,
      fbl::MakeRefCounted<fs::Service>(
          [this](fidl::ServerEnd<fuchsia_lowpan_bootstrap::Thread> request) {
            zx_status_t status =
                bootstrap_impl_->Bind(std::move(request), loop_.dispatcher(), outgoing_->svc_dir());
            if (status != ZX_OK) {
              FX_LOGS(ERROR) << "error binding new server: " << status << std::endl;
            }
            return status;
          }));

  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Error adding service in ot-stack for bootstrap fidl";
    return status;
  }
  return ZX_OK;
}

// Setup FIDL server side which handle requests from upper layer components.
zx_status_t OtStackApp::SetupFidlService() {
  zx_status_t status = InitOutgoingAndServe();
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Init outgoing failed during SetupFidlService: " << status << std::endl;
    return status;
  }

  fidl_request_handler_ptr_ = std::make_unique<LowpanSpinelDeviceFidlImpl>(*this);

  status = outgoing_->svc_dir()->AddEntry(
      fidl::DiscoverableProtocolName<fidl_spinel::Device>,
      fbl::MakeRefCounted<fs::Service>([this](fidl::ServerEnd<fidl_spinel::Device> request) {
        if (binding_) {  // TODO (jiamingw) add support for multiple clients
          FX_LOGS(ERROR) << "FIDL connect request rejected: already bound";
          return ZX_ERR_ALREADY_BOUND;
        }
        binding_ = fidl::BindServer(
            loop_.dispatcher(), std::move(request), fidl_request_handler_ptr_.get(),
            [](LowpanSpinelDeviceFidlImpl* /*unused*/, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_lowpan_spinel::Device> /*unused*/) {
              FX_LOGS(INFO) << "channel handle unbound with reason: "
                            << static_cast<uint32_t>(info.reason);
            });
        return ZX_OK;
      }));

  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Error adding service in ot-stack";
    return status;
  }
  return ZX_OK;
}

void OtStackApp::SendOneFrameToClient() {
  if (!binding_.has_value()) {
    FX_LOGS(ERROR) << "ot-stack: Sending frame to client, but client is not connected";
    assert(0);
  }
  if (!client_inbound_queue_.empty() && client_inbound_allowance_ > 0) {
    auto data = fidl::VectorView<uint8_t>::FromExternal(client_inbound_queue_.front());
    (*binding_)->OnReceiveFrame(std::move(data));
    UpdateClientInboundAllowance();
    client_inbound_queue_.pop_front();
    if (!client_inbound_queue_.empty() && client_inbound_allowance_ > 0) {
      async::PostTask(loop_.dispatcher(), [this]() { this->SendOneFrameToClient(); });
    }
    FX_LOGS(DEBUG) << "ot-stack: sent one frame to the client of ot-stack";
  } else {
    FX_LOGS(WARNING) << "ot-stack: unable to sent one frame to the client of ot-stack, q size:"
                     << client_inbound_queue_.size()
                     << " client_inbound_allowance_:" << client_inbound_allowance_;
  }
}

zx_status_t OtStackApp::InitRadioDriver() {
  zx_status_t result = ConnectToOtRadioDev();
  if (result != ZX_OK) {
    return result;
  }
  RadioAllowanceInit();
  return ZX_OK;
}

void OtStackApp::InitOpenThreadLibrary(bool reset_rcp) {
  FX_LOGS(INFO) << "init ot-lib";
  otPlatformConfig config;
  config.callback_ptr = lowpan_spinel_ptr_.get();
  config.m_speed_up_factor = 1;
  config.reset_rcp = reset_rcp;
  ot_instance_ptr_ = static_cast<void*>(otSysInit(&config));
  ot::Ncp::otNcpInit(static_cast<otInstance*>(ot_instance_ptr_.value()));
  ot::Ncp::otNcpGetInstance()->Init(lowpan_spinel_ptr_.get());
}

zx_status_t OtStackApp::Init(const std::string& path, bool is_test_env) {
  is_test_env_ = is_test_env;
  device_path_ = path;

  bool bootstrap_only = files::IsFile(kMigrationConfigPath);
  if (bootstrap_only) {
    return SetupBootstrapFidlService();
  }

  lowpan_spinel_ptr_ = std::make_unique<OtStackCallBackImpl>(*this);

  zx_status_t status = InitRadioDriver();
  if (status != ZX_OK) {
    return status;
  }

  InitOpenThreadLibrary(false);

  status = SetupFidlService();
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

void OtStackApp::AlarmTask() {
  zx_time_t remaining;
  platformAlarmUpdateTimeout(&remaining);
  if (remaining == 0) {
    FX_LOGS(DEBUG) << "ot-stack: calling platformAlarmProcess()";
    platformAlarmProcess(static_cast<otInstance*>(ot_instance_ptr_.value()));
  } else {
    // If remaining is not 0, then the alarm is likely already being reset.
    // do not need to do anything here
    FX_LOGS(DEBUG) << "ot-stack: alarm process not called, remaining: " << remaining;
  }
}

// Connect to ot-radio device driver which allows ot-stack to talk to lower layer
zx_status_t OtStackApp::ConnectToOtRadioDev() {
  zx_status_t result = ZX_OK;
  if (is_test_env_) {
    result = SetDeviceSetupClientInIsolatedDevmgr(device_path_);
  } else {
    result = SetDeviceSetupClientInDevmgr(device_path_);
  }
  if (result != ZX_OK) {
    FX_LOGS(ERROR) << "failed to set device setup client";
    return result;
  }
  return SetupOtRadioDev();
}

// Get the spinel setup client from a file path. Set `client_ptr_` on success.
zx_status_t OtStackApp::SetDeviceSetupClientInDevmgr(const std::string& path) {
  auto client_end = service::Connect<fidl_spinel::DeviceSetup>(path.c_str());
  if (client_end.is_error()) {
    FX_LOGS(ERROR) << "failed to connect to device: " << client_end.status_string();
    return client_end.status_value();
  }

  device_setup_client_ptr_ =
      std::make_unique<fidl::WireSyncClient<fidl_spinel::DeviceSetup>>(std::move(*client_end));
  return ZX_OK;
}

// Get the spinel setup client from a file path. Set `client_ptr_` on success.
zx_status_t OtStackApp::SetDeviceSetupClientInIsolatedDevmgr(const std::string& path) {
  auto isolated_devfs = service::Connect<fuchsia_openthread_devmgr::IsolatedDevmgr>();
  if (isolated_devfs.is_error()) {
    FX_LOGS(ERROR) << "failed to connect to isolated devmgr: " << isolated_devfs.status_string();
    return isolated_devfs.status_value();
  }
  // IsolatedDevmgr composes fuchsia.io.Directory, but FIDL bindings don't know.
  auto client_end = service::ConnectAt<fidl_spinel::DeviceSetup>(
      fidl::UnownedClientEnd<fuchsia_io::Directory>(isolated_devfs->channel().borrow()),
      path.c_str());
  if (client_end.is_error()) {
    FX_LOGS(ERROR) << "failed to connect to device setup: " << client_end.status_string();
    return client_end.status_value();
  }

  device_setup_client_ptr_ =
      std::make_unique<fidl::WireSyncClient<fidl_spinel::DeviceSetup>>(std::move(*client_end));
  return ZX_OK;
}

zx_status_t OtStackApp::SetupOtRadioDev() {
  if (device_setup_client_ptr_ == nullptr) {
    return ZX_ERR_BAD_STATE;
  }

  auto endpoints = fidl::CreateEndpoints<fidl_spinel::Device>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }
  auto [client_end, server_end] = std::move(endpoints.value());

  auto fidl_result = device_setup_client_ptr_->SetChannel(std::move(server_end));
  if (fidl_result.status() != ZX_OK) {
    FX_LOGS(ERROR) << "Cannot set the channel to device: " << fidl_result.status_string();
    return fidl_result.status();
  }

  auto& result = fidl_result.value().result;
  if (result.is_err()) {
    FX_LOGS(ERROR) << "Cannot set the channel to device: " << static_cast<uint32_t>(result.err());
    return ZX_ERR_INTERNAL;
  }
  FX_LOGS(INFO) << "successfully connected to driver";

  event_thread_ = std::thread(&OtStackApp::EventThread, this);
  zx_status_t status = zx::port::create(0, &port_);
  if (status != ZX_OK) {
    return status;
  }

  device_channel_ = zx::unowned_channel(client_end.channel());
  device_client_ptr_ =
      std::make_unique<fidl::WireSyncClient<fidl_spinel::Device>>(std::move(client_end));
  connected_to_device_ = true;

  status = device_channel_->wait_async(port_, kPortRadioChannelRead,
                                       ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED, 0);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to wait for events";
  }
  return status;
}

void OtStackApp::EventLoopHandleInboundFrame(::fidl::VectorView<uint8_t> data) {
  fbl::AutoLock lock(&radio_q_mtx_);
  radio_inbound_queue_.emplace_back(data.cbegin(), data.cend());
  sync_completion_signal(&radio_rx_complete_);
  async::PostTask(loop_.dispatcher(), [this]() {
    platformRadioProcess(static_cast<otInstance*>(this->ot_instance_ptr_.value()));
  });
}

void OtStackApp::OnReadyForSendFrames(
    fidl::WireResponse<fidl_spinel::Device::OnReadyForSendFrames>* event) {
  HandleRadioOnReadyForSendFrame(event->number_of_frames);
}

void OtStackApp::OnReceiveFrame(fidl::WireResponse<fidl_spinel::Device::OnReceiveFrame>* event) {
  EventLoopHandleInboundFrame(std::move(event->data));
  UpdateRadioInboundAllowance();
}

void OtStackApp::OnError(fidl::WireResponse<fidl_spinel::Device::OnError>* event) {
  handler_status_ = (*binding_)->OnError(event->error, event->did_close);
}

zx_status_t OtStackApp::Unknown() {
  (*binding_)->OnError(fidl_spinel::wire::Error::IO_ERROR, true);
  DisconnectDevice();
  return ZX_ERR_IO;
}

void OtStackApp::EventThread() {
  while (true) {
    zx_port_packet_t packet = {};
    port_.wait(zx::time::infinite(), &packet);
    switch (packet.key) {
      case kPortRadioChannelRead: {
        if (packet.signal.observed & ZX_CHANNEL_PEER_CLOSED) {
          FX_LOGS(ERROR) << "ot-radio channel closed, terminating event thread";
          return;
        }
        ::fidl::Result result = HandleOneEvent(device_client_ptr_->client_end());
        if (!result.ok() || (handler_status_ != ZX_OK)) {
          FX_PLOGS(ERROR, result.ok() ? handler_status_ : result.status())
              << "error calling fidl::WireSyncClient<fidl_spinel::Device>::HandleEvents(), "
                 "terminating event "
                 "thread";
          DisconnectDevice();
          loop_.Shutdown();
          return;
        }
        zx_status_t status = device_channel_->wait_async(
            port_, kPortRadioChannelRead, ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED, 0);
        if (status != ZX_OK) {
          FX_PLOGS(ERROR, status) << "failed to wait for events, terminating event thread";
          return;
        }
      } break;
      case kPortTerminate:
        FX_LOGS(INFO) << "terminating event thread";
        return;
    }
  }
}

void OtStackApp::TerminateEventThread() {
  zx_port_packet packet = {kPortTerminate, ZX_PKT_TYPE_USER, ZX_OK, {}};
  port_.queue(&packet);
  event_thread_.join();
}

void OtStackApp::DisconnectDevice() {
  device_channel_ = zx::unowned_channel(ZX_HANDLE_INVALID);
  device_client_ptr_.reset();
  device_setup_client_ptr_.reset();
  connected_to_device_ = false;
}

void OtStackApp::Shutdown() {
  FX_LOGS(ERROR) << "terminating message loop in ot-stack";
  if (binding_) {
    binding_->Close(ZX_ERR_INTERNAL);
  }
  TerminateEventThread();
  DisconnectDevice();
  loop_.Quit();
}

}  // namespace otstack
