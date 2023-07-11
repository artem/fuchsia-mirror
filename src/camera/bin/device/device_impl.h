// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CAMERA_BIN_DEVICE_DEVICE_IMPL_H_
#define SRC_CAMERA_BIN_DEVICE_DEVICE_IMPL_H_

#include <fuchsia/camera2/hal/cpp/fidl.h>
#include <fuchsia/camera3/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/ui/policy/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/result.h>
#include <lib/sys/cpp/component_context.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <list>
#include <map>
#include <memory>
#include <vector>

#include "src/camera/bin/device/stream_impl.h"
#include "src/camera/bin/device/sysmem_allocator.h"
#include "src/camera/lib/hanging_get_helper/hanging_get_helper.h"

namespace camera {

// Represents a physical camera device, and serves multiple clients of the camera3.Device protocol.
class DeviceImpl : public fuchsia::ui::policy::MediaButtonsListener {
 public:
  // Creates a DeviceImpl using the given |controller|.
  //
  // References to |dispatcher|, |executor|, and |context| may be retained by the instance so the
  // caller must ensure these outlive the returned DeviceImpl.
  static fpromise::promise<std::unique_ptr<DeviceImpl>, zx_status_t> Create(
      async_dispatcher_t* dispatcher, fpromise::executor& executor,
      fuchsia::camera2::hal::ControllerHandle controller,
      fuchsia::sysmem::AllocatorHandle allocator,
      fuchsia::ui::policy::DeviceListenerRegistryHandle registry, zx::event bad_state_event);

  DeviceImpl(async_dispatcher_t* dispatcher, fpromise::executor& executor,
             fuchsia::sysmem::AllocatorHandle allocator, zx::event bad_state_event);
  ~DeviceImpl() override;

  // Returns a service handler for use with a service directory.
  fidl::InterfaceRequestHandler<fuchsia::camera3::Device> GetHandler();

  async_dispatcher_t* dispatcher() const { return dispatcher_; }

 private:
  // Called by the request handler returned by GetHandler, i.e. when a new client connects to the
  // published service.
  void OnNewRequest(fidl::InterfaceRequest<fuchsia::camera3::Device> request);

  // Posts a task to bind a new client.
  void Bind(fidl::InterfaceRequest<fuchsia::camera3::Device> request);

  // Called if the underlying controller disconnects.
  void OnControllerDisconnected(zx_status_t status);

  // Posts a task to remove the client with the given id.
  void RemoveClient(uint64_t id);

  // Sets the current configuration to the provided index.
  void SetConfiguration(uint32_t index);

  // Set the software mute state.
  void SetSoftwareMuteState(bool muted,
                            fuchsia::camera3::Device::SetSoftwareMuteStateCallback callback);

  // Toggles the streaming state of the controller if necessary.
  void UpdateControllerStreamingState();

  // Posts a task to connect to a stream.
  void PostConnectToStream(uint32_t index,
                           fidl::InterfaceRequest<fuchsia::camera3::Stream> request);

  // Connects to a stream.
  void ConnectToStream(uint32_t index, fidl::InterfaceRequest<fuchsia::camera3::Stream> request);

  // Called by a stream when it has sufficient information to connect to the legacy stream protocol.
  void OnStreamRequested(uint32_t index, fidl::InterfaceRequest<fuchsia::camera2::Stream> request,

                         uint32_t format_index);

  // Called by a stream when it has received a buffer token from the legacy stream.
  void OnBuffersRequested(uint32_t index, fuchsia::sysmem::BufferCollectionTokenHandle token,
                          fit::function<void(uint32_t)> max_camping_buffers_callback);

  // |fuchsia::ui::policy::MediaButtonsListener|
  void OnEvent(fuchsia::ui::input::MediaButtonsEvent event,
               fuchsia::ui::policy::MediaButtonsListener::OnEventCallback callback) override;

  // Represents a single client connection to the DeviceImpl class.
  class Client : public fuchsia::camera3::Device {
   public:
    Client(DeviceImpl& device, uint64_t id,
           fidl::InterfaceRequest<fuchsia::camera3::Device> request);
    ~Client() override;

    // Inform the client of a new configuration.
    void ConfigurationUpdated(uint32_t index);

    // Inform the client of a new mute state.
    void MuteUpdated(MuteState mute_state);

   private:
    // Closes |binding_| with the provided |status| epitaph, and removes the client instance from
    // the parent |clients_| map.
    void CloseConnection(zx_status_t status);

    // Called when the client endpoint of |binding_| is closed.
    void OnClientDisconnected(zx_status_t status);

    // |fuchsia::camera3::Device|
    void GetIdentifier(GetIdentifierCallback callback) override;
    void GetConfigurations(GetConfigurationsCallback callback) override;
    void GetConfigurations2(GetConfigurations2Callback callback) override;
    void WatchCurrentConfiguration(WatchCurrentConfigurationCallback callback) override;
    void SetCurrentConfiguration(uint32_t index) override;
    void WatchMuteState(WatchMuteStateCallback callback) override;
    void SetSoftwareMuteState(bool muted, SetSoftwareMuteStateCallback callback) override;
    void ConnectToStream(uint32_t index,
                         fidl::InterfaceRequest<fuchsia::camera3::Stream> request) override;
    void Rebind(fidl::InterfaceRequest<fuchsia::camera3::Device> request) override;

    DeviceImpl& device_;
    std::string log_prefix_;
    uint64_t id_;
    fidl::Binding<fuchsia::camera3::Device> binding_;
    HangingGetHelper<uint32_t> configuration_;
    HangingGetHelper<MuteState> mute_state_;
  };

  async_dispatcher_t* dispatcher_;
  fpromise::executor& executor_;
  SysmemAllocator sysmem_allocator_;
  zx::event bad_state_event_;
  fuchsia::camera2::hal::ControllerPtr controller_;
  fuchsia::sysmem::AllocatorPtr allocator_;
  fuchsia::ui::policy::DeviceListenerRegistryPtr registry_;
  fidl::Binding<fuchsia::ui::policy::MediaButtonsListener> button_listener_binding_;
  fuchsia::camera2::DeviceInfo device_info_;
  std::vector<fuchsia::camera2::hal::Config> configs_;
  std::vector<fuchsia::camera3::Configuration2> configurations_;
  std::vector<std::unique_ptr<MetricsReporter::ConfigurationRecord>> records_;
  std::map<uint64_t, std::unique_ptr<Client>> clients_;
  uint64_t client_id_next_ = 1;
  uint32_t current_configuration_index_ = 0;

  std::vector<zx::eventpair> deallocation_events_;
  std::vector<fpromise::promise<void, zx_status_t>> deallocation_promises_;
  std::vector<std::unique_ptr<StreamImpl>> streams_;
  MuteState mute_state_;
  bool controller_streaming_ = true;

  friend class Client;
};

}  // namespace camera

#endif  // SRC_CAMERA_BIN_DEVICE_DEVICE_IMPL_H_
