// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/camera/bin/device/device_impl.h"

#include <fidl/fuchsia.sysmem/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.sysmem2/cpp/hlcpp_conversion.h>
#include <lib/async/cpp/task.h>
#include <lib/fpromise/bridge.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <sstream>
#include <string>

#include "src/camera/bin/device/messages.h"
#include "src/camera/bin/device/metrics_reporter.h"
#include "src/camera/bin/device/util.h"
#include "src/lib/fsl/handles/object_info.h"

namespace camera {
namespace {

using ConfigPtr = std::unique_ptr<fuchsia::camera2::hal::Config>;

fpromise::promise<fuchsia::camera2::DeviceInfo> FetchDeviceInfo(
    const fuchsia::camera2::hal::ControllerPtr& controller) {
  fpromise::bridge<fuchsia::camera2::DeviceInfo> bridge;
  controller->GetDeviceInfo([completer = std::move(bridge.completer)](auto device_info) mutable {
    completer.complete_ok(std::move(device_info));
  });
  return bridge.consumer.promise();
}

fpromise::promise<std::vector<ConfigPtr>, zx_status_t> FetchConfigs(
    const fuchsia::camera2::hal::ControllerPtr& controller,
    std::vector<ConfigPtr> configs = std::vector<ConfigPtr>()) {
  fpromise::bridge<ConfigPtr, zx_status_t> bridge;
  controller->GetNextConfig(
      [completer = std::move(bridge.completer)](auto config, auto status) mutable {
        if (status == ZX_OK) {
          completer.complete_ok(std::move(config));
        } else {
          completer.complete_error(status);
        }
      });
  return bridge.consumer.promise().then(
      [&controller,
       configs = std::move(configs)](fpromise::result<ConfigPtr, zx_status_t>& result) mutable
      -> fpromise::promise<std::vector<ConfigPtr>, zx_status_t> {
        if (result.is_ok()) {
          // If we received a config, we need to call FetchConfigs again to get the next config.
          configs.push_back(result.take_value());
          return FetchConfigs(controller, std::move(configs));
        } else if (result.error() == ZX_ERR_STOP) {
          // This means we've already fetched all configs successfully.
          return fpromise::make_result_promise<std::vector<ConfigPtr>, zx_status_t>(
              fpromise::ok(std::move(configs)));
        } else {
          // Unexpected zx_status_t values will be a failure.
          return fpromise::make_result_promise<std::vector<ConfigPtr>, zx_status_t>(
              fpromise::error(result.take_error()));
        }
      });
}

}  // namespace

DeviceImpl::DeviceImpl(async_dispatcher_t* dispatcher, fpromise::executor& executor,
                       fuchsia::sysmem2::AllocatorHandle allocator, zx::event bad_state_event)
    : dispatcher_(dispatcher),
      executor_(executor),
      sysmem_allocator_(std::move(allocator)),
      bad_state_event_(std::move(bad_state_event)),
      button_listener_binding_(this) {}

DeviceImpl::~DeviceImpl() = default;

fpromise::promise<std::unique_ptr<DeviceImpl>, zx_status_t> DeviceImpl::Create(
    async_dispatcher_t* dispatcher, fpromise::executor& executor,
    fuchsia::camera2::hal::ControllerHandle controller, fuchsia::sysmem2::AllocatorHandle allocator,
    fuchsia::ui::policy::DeviceListenerRegistryHandle registry, zx::event bad_state_event) {
  auto device = std::make_unique<DeviceImpl>(dispatcher, executor, std::move(allocator),
                                             std::move(bad_state_event));
  ZX_ASSERT(device->controller_.Bind(std::move(controller)) == ZX_OK);

  // Bind the controller interface and get some initial startup information.
  device->controller_.set_error_handler([device = device.get()](zx_status_t status) {
    FX_PLOGS(ERROR, status) << "Controller server disconnected during initialization.";
    ZX_ASSERT(device->bad_state_event_.signal(0, ZX_EVENT_SIGNALED) == ZX_OK);
  });

  using DeviceInfoResult = fpromise::result<>;
  auto device_info_promise =
      FetchDeviceInfo(device->controller_)
          .and_then([device = device.get()](fuchsia::camera2::DeviceInfo& device_info) mutable {
            device->device_info_ = std::move(device_info);
          });
  using FetchConfigsResult = fpromise::result<void, zx_status_t>;
  auto configs_promise =
      FetchConfigs(device->controller_)
          .then([device =
                     device.get()](fpromise::result<std::vector<ConfigPtr>, zx_status_t>& result)
                    -> FetchConfigsResult {
            if (result.is_error()) {
              return fpromise::error(result.error());
            }
            for (uint32_t config_index = 0; config_index < result.value().size(); ++config_index) {
              const auto& config = result.value()[config_index];
              auto result = Convert(*config);
              if (result.is_error()) {
                FX_PLOGS(ERROR, result.error()) << "Failed to convert config";
                return fpromise::error(result.error());
              }

              auto num_streams = result.value().streams().size();
              auto config_metrics =
                  MetricsReporter::Get().CreateConfigurationRecord(config_index, num_streams);
              for (uint32_t stream_index = 0; stream_index < result.value().streams().size();
                   ++stream_index) {
                config_metrics->GetStreamRecord(stream_index)
                    .SetProperties(result.value().streams()[stream_index]);
              }
              device->records_.push_back(std::move(config_metrics));
              device->configurations_.push_back(result.take_value());
              device->configs_.push_back(std::move(*config));
            }
            device->SetConfiguration(0);
            return fpromise::ok();
          });

  // Wait for all expected callbacks to occur.
  return fpromise::join_promises(std::move(device_info_promise), std::move(configs_promise))
      .then([device = std::move(device), registry = std::move(registry)](
                fpromise::result<std::tuple<DeviceInfoResult, FetchConfigsResult>>& results) mutable
            -> fpromise::result<std::unique_ptr<DeviceImpl>, zx_status_t> {
        FX_CHECK(results.is_ok());
        if (std::get<1>(results.value()).is_error()) {
          return fpromise::error(std::get<1>(results.value()).error());
        }
        // Bind the registry interface and register the device as a listener.
        ZX_ASSERT(device->registry_.Bind(std::move(registry)) == ZX_OK);
        device->registry_->RegisterListener(device->button_listener_binding_.NewBinding(), [] {});

        // Rebind the controller error handler.
        device->controller_.set_error_handler(
            fit::bind_member(device.get(), &DeviceImpl::OnControllerDisconnected));
        return fpromise::ok(std::move(device));
      });
}

fidl::InterfaceRequestHandler<fuchsia::camera3::Device> DeviceImpl::GetHandler() {
  return fit::bind_member(this, &DeviceImpl::OnNewRequest);
}

void DeviceImpl::OnNewRequest(fidl::InterfaceRequest<fuchsia::camera3::Device> request) {
  Bind(std::move(request));
}

void DeviceImpl::Bind(fidl::InterfaceRequest<fuchsia::camera3::Device> request) {
  bool first_client = clients_.empty();
  auto client = std::make_unique<Client>(*this, client_id_next_, std::move(request));
  auto [it, emplaced] = clients_.emplace(client_id_next_++, std::move(client));
  auto& [id, new_client] = *it;
  if (first_client) {
    SetConfiguration(0);
  } else {
    new_client->ConfigurationUpdated(current_configuration_index_);
  }
  new_client->MuteUpdated(mute_state_);
}

void DeviceImpl::OnControllerDisconnected(zx_status_t status) {
  FX_PLOGS(ERROR, status) << "Controller disconnected unexpectedly.";
  ZX_ASSERT(bad_state_event_.signal(0, ZX_EVENT_SIGNALED) == ZX_OK);
}

void DeviceImpl::RemoveClient(uint64_t id) { clients_.erase(id); }

void DeviceImpl::SetConfiguration(uint32_t index) {
  records_[current_configuration_index_]->SetActive(false);
  current_configuration_index_ = index;
  records_[current_configuration_index_]->SetActive(true);

  std::vector<fpromise::promise<void, zx_status_t>> deallocation_promises;
  for (auto& event : deallocation_events_) {
    fpromise::bridge<void, zx_status_t> bridge;
    auto wait = std::make_shared<async::WaitOnce>(event.release(), ZX_EVENTPAIR_PEER_CLOSED, 0);
    wait->Begin(dispatcher_, [wait_ref = wait, completer = std::move(bridge.completer)](
                                 async_dispatcher_t* dispatcher, async::WaitOnce* wait,
                                 zx_status_t status, const zx_packet_signal_t* signal) mutable {
      if (status != ZX_OK) {
        completer.complete_error(status);
        return;
      }
      completer.complete_ok();
    });
    deallocation_promises.push_back(bridge.consumer.promise());
  }

  deallocation_events_.clear();
  deallocation_promises_ = std::move(deallocation_promises);

  for (auto& stream : streams_) {
    if (stream) {
      stream->CloseAllClients(ZX_OK);
    }
  }

  streams_.clear();

#if CAMERA_QUIRK_ADD_CONFIG_CHANGE_DELAY
  // TODO(b/224858687) - Temporary workaround to ensure that camera pipeline has an opportunity to
  // teardown buffers before the next stream (in the new configuration) is requested. The original
  // code to protect this race condition has some flaws. The real fix is in progress.
  zx_nanosleep(zx_deadline_after(ZX_MSEC(1000)));
#endif  // CAMERA_QUIRK_ADD_CONFIG_CHANGE_DELAY

  streams_.resize(configurations_[index].streams().size());
  FX_LOGS(INFO) << "Configuration set to " << index << ".";
  for (auto& client : clients_) {
    client.second->ConfigurationUpdated(current_configuration_index_);
  }
}

void DeviceImpl::SetSoftwareMuteState(
    bool muted, fuchsia::camera3::Device::SetSoftwareMuteStateCallback callback) {
  mute_state_.software_muted = muted;
  UpdateControllerStreamingState();
  for (auto& stream : streams_) {
    if (stream) {
      stream->SetMuteState(mute_state_);
    }
  }
  callback();
  for (auto& client : clients_) {
    client.second->MuteUpdated(mute_state_);
  }
}

void DeviceImpl::UpdateControllerStreamingState() {
  if (mute_state_.muted() && controller_streaming_) {
    std::string detail = "hardware + software";
    if (!mute_state_.hardware_muted) {
      detail = "software";
    }
    if (!mute_state_.software_muted) {
      detail = "hardware";
    }
    FX_LOGS(INFO) << "disabling streaming because device is muted (" << detail << ")";
    controller_->DisableStreaming();
    controller_streaming_ = false;
  }
  if (!mute_state_.muted() && !controller_streaming_) {
    FX_LOGS(INFO) << "enabling streaming because device is unmuted";
    controller_->EnableStreaming();
    controller_streaming_ = true;
  }
}

void DeviceImpl::ConnectToStream(uint32_t index,
                                 fidl::InterfaceRequest<fuchsia::camera3::Stream> request) {
  if (index > streams_.size()) {
    request.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (streams_[index]) {
    request.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }

  auto on_stream_requested = [this, index](fidl::InterfaceRequest<fuchsia::camera2::Stream> request,
                                           uint32_t format_index) {
    FX_LOGS(INFO) << "New request for legacy stream.";
    OnStreamRequested(index, std::move(request), format_index);
  };

  auto on_buffers_requested = [this, index](
                                  fuchsia::sysmem2::BufferCollectionTokenHandle token,
                                  fit::function<void(uint32_t)> max_camping_buffers_callback) {
    OnBuffersRequested(index, std::move(token), std::move(max_camping_buffers_callback));
  };

  // When the last client disconnects destroy the stream.
  auto on_no_clients = [this, index]() { streams_[index] = nullptr; };

  auto streaming_failure_record = MetricsReporter::Get().CreateFailureTestRecord(
      MetricsReporter::FailureTestRecordType::FramesStuckInPipeline, false,
      current_configuration_index_, index);
  auto description =
      "c" + std::to_string(current_configuration_index_) + "s" + std::to_string(index);
  streams_[index] = std::make_unique<StreamImpl>(
      dispatcher_, records_[current_configuration_index_]->GetStreamRecord(index),
      configurations_[current_configuration_index_].streams()[index],
      configs_[current_configuration_index_].stream_configs[index], std::move(request),
      std::move(on_stream_requested), std::move(on_buffers_requested), std::move(on_no_clients),
      description, std::move(streaming_failure_record));
}

void DeviceImpl::OnStreamRequested(uint32_t index,
                                   fidl::InterfaceRequest<fuchsia::camera2::Stream> request,
                                   uint32_t format_index) {
  auto connect_to_stream =
      [this, index, format_index, request = std::move(request)](
          const fpromise::result<std::vector<fpromise::result<void, zx_status_t>>>&
              results) mutable {
        std::string description = "c" + std::to_string(current_configuration_index_) + "s" +
                                  std::to_string(index) + "f" + std::to_string(format_index);
        bool deallocation_complete = true;
        if (results.is_error()) {
          FX_LOGS(WARNING) << "aggregate deallocation wait failed prior to connecting to "
                           << description;
          deallocation_complete = false;
        } else {
          auto& wait_results = results.value();
          for (const auto& wait_result : wait_results) {
            if (wait_result.is_error()) {
              FX_PLOGS(WARNING, wait_result.error()) << "wait failed for previous stream at index "
                                                     << &wait_result - wait_results.data();
              deallocation_complete = false;
            }
          }
        }
        fit::closure connect = [this, index, format_index, request = std::move(request),
                                description]() mutable {
          FX_LOGS(INFO) << "connecting to controller stream: " << description;
          controller_->CreateStream(current_configuration_index_, index, format_index,
                                    std::move(request));
        };
        // If the wait for deallocation failed, try to connect anyway after a fixed delay.
        if (!deallocation_complete) {
          constexpr uint32_t kFallbackDelayMsec = 5000;
          FX_LOGS(WARNING) << "deallocation wait failed; falling back to timed wait of "
                           << kFallbackDelayMsec << "ms";
          ZX_ASSERT(async::PostDelayedTask(dispatcher_, std::move(connect),
                                           zx::msec(kFallbackDelayMsec)) == ZX_OK);
          return;
        }
        // Otherwise, connect immediately.
        connect();
      };

  // Wait for any previous configurations buffers to finish deallocation, then connect
  // to stream. The move and clear are necessary to ensure subsequent accesses to the container
  // produces well-defined results.
  auto promises = std::move(deallocation_promises_);
  deallocation_promises_.clear();
  executor_.schedule_task(fpromise::join_promise_vector(std::move(promises))
                              .then(std::move(connect_to_stream))
                              .wrap_with(streams_[index]->Scope()));
}

void DeviceImpl::OnBuffersRequested(uint32_t index,
                                    fuchsia::sysmem2::BufferCollectionTokenHandle token,
                                    fit::function<void(uint32_t)> max_camping_buffers_callback) {
  // Assign friendly names to each buffer for debugging and profiling.
  const std::string friendly_name =
      "c" + std::to_string(current_configuration_index_) + "s" + std::to_string(index);

  auto controller_camping_buffers = configs_[current_configuration_index_]
                                        .stream_configs[index]
                                        .constraints.min_buffer_count_for_camping;

  auto allocation_complete =
      [this, friendly_name, controller_camping_buffers,
       max_camping_buffers_callback = std::move(max_camping_buffers_callback)](
          fpromise::result<BufferCollectionWithLifetime, zx_status_t>& result) mutable {
        if (result.is_error()) {
          FX_PLOGS(WARNING, result.error()) << friendly_name << ": failed to allocate buffers";
          return;
        }

        auto buffer_collection_lifetime = result.take_value();
        auto buffers = std::move(buffer_collection_lifetime.buffers);
        deallocation_events_.push_back(std::move(buffer_collection_lifetime.deallocation_complete));

        // Inform the stream of the maximum number of buffers it may hand out.
        uint32_t max_camping_buffers =
            static_cast<uint32_t>(buffers.buffers().size()) - controller_camping_buffers;
        FX_LOGS(INFO) << friendly_name << ": collection resolved: at most " << max_camping_buffers
                      << " of " << buffers.buffers().size()
                      << " buffers will be made available to clients concurrently";
        max_camping_buffers_callback(max_camping_buffers);
      };

  auto v1_hlcpp_constraints_clone =
      configs_[current_configuration_index_].stream_configs[index].constraints;
  auto v1_natural_constraints = fidl::HLCPPToNatural(std::move(v1_hlcpp_constraints_clone));
  auto v2_natural_constraints_result =
      sysmem::V2CopyFromV1BufferCollectionConstraints(&v1_natural_constraints);
  ZX_ASSERT(v2_natural_constraints_result.is_ok());
  auto v2_hlcpp_constraints =
      fidl::NaturalToHLCPP(std::move(v2_natural_constraints_result.value()));

  executor_.schedule_task(sysmem_allocator_
                              .BindSharedCollection(std::move(token),
                                                    std::move(v2_hlcpp_constraints),
                                                    "camera_" + friendly_name)
                              .then(std::move(allocation_complete))
                              .wrap_with(streams_[index]->Scope()));
}

void DeviceImpl::OnEvent(fuchsia::ui::input::MediaButtonsEvent event,
                         fuchsia::ui::policy::MediaButtonsListener::OnEventCallback callback) {
  if (event.has_mic_mute()) {
    mute_state_.hardware_muted = event.mic_mute();
    UpdateControllerStreamingState();
    for (auto& client : clients_) {
      client.second->MuteUpdated(mute_state_);
    }
  }
  callback();
}

}  // namespace camera
