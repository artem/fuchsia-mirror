// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_PROFILE_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_PROFILE_SERVER_H_

#include <fuchsia/bluetooth/bredr/cpp/fidl.h>

#include "lib/fidl/cpp/binding.h"
#include "pw_intrusive_ptr/intrusive_ptr.h"
#include "src/connectivity/bluetooth/core/bt-host/fidl/server_base.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/macros.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/bredr_connection_manager.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sdp/server.h"
#include "src/connectivity/bluetooth/core/bt-host/socket/socket_factory.h"

namespace bthost {

// Implements the bredr::Profile FIDL interface.
class ProfileServer : public ServerBase<fuchsia::bluetooth::bredr::Profile> {
 public:
  ProfileServer(bt::gap::Adapter::WeakPtr adapter,
                fidl::InterfaceRequest<fuchsia::bluetooth::bredr::Profile> request);
  ~ProfileServer() override;

 private:
  class AudioDirectionExt;

  class L2capParametersExt final
      : public ServerBase<fuchsia::bluetooth::bredr::L2capParametersExt> {
   public:
    L2capParametersExt(
        fidl::InterfaceRequest<fuchsia::bluetooth::bredr::L2capParametersExt> request,
        bt::l2cap::Channel::WeakPtr channel)
        : ServerBase(this, std::move(request)),
          unique_id_(channel->unique_id()),
          channel_(std::move(channel)) {}

    bt::l2cap::Channel::UniqueId unique_id() const { return unique_id_; }

    void RequestParameters(fuchsia::bluetooth::bredr::ChannelParameters requested,
                           RequestParametersCallback callback) override;

    void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

   private:
    bt::l2cap::Channel::UniqueId unique_id_;
    bt::l2cap::Channel::WeakPtr channel_;
  };

  class AudioOffloadExt final : public ServerBase<fuchsia::bluetooth::bredr::AudioOffloadExt> {
   public:
    AudioOffloadExt(ProfileServer& profile_server,
                    fidl::InterfaceRequest<fuchsia::bluetooth::bredr::AudioOffloadExt> request,
                    bt::l2cap::Channel::WeakPtr channel, bt::gap::Adapter::WeakPtr adapter)
        : ServerBase(this, std::move(request)),
          profile_server_(profile_server),
          unique_id_(channel->unique_id()),
          channel_(std::move(channel)),
          adapter_(std::move(adapter)) {}
    void GetSupportedFeatures(GetSupportedFeaturesCallback callback) override;
    void StartAudioOffload(
        fuchsia::bluetooth::bredr::AudioOffloadConfiguration audio_offload_configuration,
        fidl::InterfaceRequest<fuchsia::bluetooth::bredr::AudioOffloadController> controller)
        override;

    bt::l2cap::Channel::UniqueId unique_id() const { return unique_id_; }

    void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

   private:
    std::unique_ptr<bt::l2cap::A2dpOffloadManager::Configuration> AudioOffloadConfigFromFidl(
        fuchsia::bluetooth::bredr::AudioOffloadConfiguration& audio_offload_configuration);

    ProfileServer& profile_server_;
    bt::l2cap::Channel::UniqueId unique_id_;
    bt::l2cap::Channel::WeakPtr channel_;
    bt::gap::Adapter::WeakPtr adapter_;
  };

  class AudioOffloadController
      : public ServerBase<fuchsia::bluetooth::bredr::AudioOffloadController> {
   public:
    explicit AudioOffloadController(
        fidl::InterfaceRequest<fuchsia::bluetooth::bredr::AudioOffloadController> request,
        bt::l2cap::Channel::WeakPtr channel)
        : ServerBase(this, std::move(request)),
          unique_id_(channel->unique_id()),
          channel_(std::move(channel)) {}

    void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

    bt::l2cap::Channel::UniqueId unique_id() const { return unique_id_; }

    void Stop(StopCallback callback) override {}

    void SendOnStartedEvent() { binding()->events().OnStarted(); }

    void Close(zx_status_t epitaph) { binding()->Close(epitaph); }

    WeakPtr<AudioOffloadController> GetWeakPtr() { return weak_self_.GetWeakPtr(); }

   private:
    bt::l2cap::Channel::UniqueId unique_id_;
    bt::l2cap::Channel::WeakPtr channel_;
    WeakSelf<AudioOffloadController> weak_self_{this};
  };

  class ScoConnectionServer final : public ServerBase<fuchsia::bluetooth::bredr::ScoConnection> {
   public:
    explicit ScoConnectionServer(
        fidl::InterfaceRequest<fuchsia::bluetooth::bredr::ScoConnection> request,
        ProfileServer* profile_server);
    ~ScoConnectionServer() override;
    // Call bt::gap::ScoConnection::Activate with the appropriate callbacks. On error, destroys this
    // server.
    void Activate();

    void OnConnectedParams(::fuchsia::bluetooth::bredr::ScoConnectionParameters params) {
      OnConnectionComplete(::fuchsia::bluetooth::bredr::ScoConnectionOnConnectionCompleteRequest::
                               WithConnectedParams(std::move(params)));
    }

    // Sends the OnConnectionComplete event with `error` and shuts down the server.
    void OnError(::fuchsia::bluetooth::bredr::ScoErrorCode error);

    const std::vector<fuchsia::bluetooth::bredr::ScoConnectionParameters>& parameters() const {
      return parameters_;
    }

    void set_parameters(std::vector<fuchsia::bluetooth::bredr::ScoConnectionParameters> params) {
      parameters_ = std::move(params);
    }

    void set_request_handle(bt::gap::Adapter::BrEdr::ScoRequestHandle handle) {
      request_handle_ = std::move(handle);
    };

    void set_connection(bt::sco::ScoConnection::WeakPtr connection) {
      connection_ = std::move(connection);
    }

    WeakPtr<ScoConnectionServer> GetWeakPtr() { return weak_self_.GetWeakPtr(); }

   private:
    // ScoConnection overrides:
    void Read(ReadCallback callback) override;
    void Write(::fuchsia::bluetooth::bredr::ScoConnectionWriteRequest request,
               WriteCallback callback) override;
    void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

    void TryRead();

    // Closes the server with `epitaph` and destroys the server.
    void Close(zx_status_t epitaph);

    void OnConnectionComplete(
        ::fuchsia::bluetooth::bredr::ScoConnectionOnConnectionCompleteRequest request) {
      binding()->events().OnConnectionComplete(std::move(request));
    }

    std::optional<bt::gap::Adapter::BrEdr::ScoRequestHandle> request_handle_;
    std::vector<fuchsia::bluetooth::bredr::ScoConnectionParameters> parameters_;

    bt::sco::ScoConnection::WeakPtr connection_;
    // Non-null when a read request is waiting for an inbound packet.
    fit::callback<void(::fuchsia::bluetooth::bredr::ScoConnection_Read_Result)> read_cb_;

    ProfileServer* profile_server_;

    // Keep this as the last member to make sure that all weak pointers are
    // invalidated before other members get destroyed.
    WeakSelf<ScoConnectionServer> weak_self_;
  };

  // fuchsia::bluetooth::bredr::Profile overrides:
  void Advertise(fuchsia::bluetooth::bredr::ProfileAdvertiseRequest request,
                 AdvertiseCallback callback) override;
  void Search(::fuchsia::bluetooth::bredr::ProfileSearchRequest request) override;
  void Connect(fuchsia::bluetooth::PeerId peer_id,
               fuchsia::bluetooth::bredr::ConnectParameters connection,
               ConnectCallback callback) override;
  void ConnectSco(::fuchsia::bluetooth::bredr::ProfileConnectScoRequest request) override;
  void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

  // Callback when clients close or revoke their connection targets
  void OnConnectionReceiverClosed(uint64_t ad_id);

  // Callback when clients close their search results
  void OnSearchResultError(uint64_t search_id, zx_status_t status);

  // Callback for incoming connections
  void OnChannelConnected(uint64_t ad_id, bt::l2cap::Channel::WeakPtr channel,
                          const bt::sdp::DataElement& protocol_list);

  // Callback for services found on connected device
  void OnServiceFound(uint64_t search_id, bt::PeerId peer_id,
                      const std::map<bt::sdp::AttributeId, bt::sdp::DataElement>& attributes);

  // Callback for SCO connections requests.
  static void OnScoConnectionResult(WeakPtr<ScoConnectionServer>& server,
                                    bt::sco::ScoConnectionManager::AcceptConnectionResult result);

  // Callback when clients close their audio direction extension.
  void OnAudioDirectionExtError(AudioDirectionExt* ext_server, zx_status_t status);

  // Create an AudioDirectionExt server for the given channel and set up callbacks.
  // Returns the client end of the channel.
  fidl::InterfaceHandle<fuchsia::bluetooth::bredr::AudioDirectionExt> BindAudioDirectionExtServer(
      bt::l2cap::Channel::WeakPtr channel);

  // Callback when clients close their l2cap parameters extension.
  void OnL2capParametersExtError(L2capParametersExt* ext_server, zx_status_t status);

  // Create an L2capParametersExt server for the given channel and set up callbacks.
  // Returns the client end of the channel.
  fidl::InterfaceHandle<fuchsia::bluetooth::bredr::L2capParametersExt> BindL2capParametersExtServer(
      bt::l2cap::Channel::WeakPtr channel);

  // Callback when clients close their audio offload extension.
  void OnAudioOffloadExtError(AudioOffloadExt* ext_server, zx_status_t status);

  // Create an AudioOffloadExt server for the given channel and set up callbacks.
  // Returns the client end of the channel.
  fidl::InterfaceHandle<fuchsia::bluetooth::bredr::AudioOffloadExt> BindAudioOffloadExtServer(
      bt::l2cap::Channel::WeakPtr channel);

  // Create a FIDL Channel from an l2cap::Channel. A socket channel relay is created from |channel|
  // and returned in the FIDL Channel.
  fuchsia::bluetooth::bredr::Channel ChannelToFidl(bt::l2cap::Channel::WeakPtr channel);

  const bt::gap::Adapter::WeakPtr& adapter() const { return adapter_; }

  // Advertised Services
  struct AdvertisedService {
    AdvertisedService(fidl::InterfacePtr<fuchsia::bluetooth::bredr::ConnectionReceiver> receiver,
                      bt::sdp::Server::RegistrationHandle registration_handle)
        : receiver(std::move(receiver)), registration_handle(registration_handle) {}
    fidl::InterfacePtr<fuchsia::bluetooth::bredr::ConnectionReceiver> receiver;
    bt::sdp::Server::RegistrationHandle registration_handle;
  };

  uint64_t advertised_total_;
  std::map<uint64_t, AdvertisedService> current_advertised_;

  // Searches registered
  struct RegisteredSearch {
    RegisteredSearch(fidl::InterfacePtr<fuchsia::bluetooth::bredr::SearchResults> results,
                     bt::gap::BrEdrConnectionManager::SearchId search_id)
        : results(std::move(results)), search_id(search_id) {}
    fidl::InterfacePtr<fuchsia::bluetooth::bredr::SearchResults> results;
    bt::gap::BrEdrConnectionManager::SearchId search_id;
  };

  uint64_t searches_total_;
  std::map<uint64_t, RegisteredSearch> searches_;

  class AudioDirectionExt final : public ServerBase<fuchsia::bluetooth::bredr::AudioDirectionExt> {
   public:
    // Calls to SetPriority() are forwarded to |priority_cb|.
    AudioDirectionExt(fidl::InterfaceRequest<fuchsia::bluetooth::bredr::AudioDirectionExt> request,
                      bt::l2cap::Channel::WeakPtr channel)
        : ServerBase(this, std::move(request)),
          unique_id_(channel->unique_id()),
          channel_(std::move(channel)) {}

    bt::l2cap::Channel::UniqueId unique_id() const { return unique_id_; }

    // fuchsia::bluetooth::bredr::AudioDirectionExt overrides:
    void SetPriority(fuchsia::bluetooth::bredr::A2dpDirectionPriority priority,
                     SetPriorityCallback callback) override;

    void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

   private:
    bt::l2cap::Channel::UniqueId unique_id_;
    bt::l2cap::Channel::WeakPtr channel_;
  };
  std::unordered_map<bt::l2cap::Channel::UniqueId, std::unique_ptr<L2capParametersExt>>
      l2cap_parameters_ext_servers_;

  std::unordered_map<bt::l2cap::Channel::UniqueId, std::unique_ptr<AudioDirectionExt>>
      audio_direction_ext_servers_;

  std::unordered_map<bt::l2cap::Channel::UniqueId, std::unique_ptr<AudioOffloadExt>>
      audio_offload_ext_servers_;

  std::unique_ptr<AudioOffloadController> audio_offload_controller_server_;

  bt::gap::Adapter::WeakPtr adapter_;

  // Creates sockets that bridge L2CAP channels to profile processes.
  bt::socket::SocketFactory<bt::l2cap::Channel> l2cap_socket_factory_;

  std::unordered_map<ScoConnectionServer*, std::unique_ptr<ScoConnectionServer>>
      sco_connection_servers_;

  // Keep this as the last member to make sure that all weak pointers are
  // invalidated before other members get destroyed.
  WeakSelf<ProfileServer> weak_self_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(ProfileServer);
};

}  // namespace bthost

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_PROFILE_SERVER_H_
