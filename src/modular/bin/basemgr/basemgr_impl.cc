// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/modular/bin/basemgr/basemgr_impl.h"

#include <lib/async/default.h>
#include <lib/fpromise/bridge.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_ref_pair.h>
#include <lib/ui/scenic/cpp/view_token_pair.h>
#include <zircon/time.h>

#include "src/lib/fsl/vmo/strings.h"
#include "src/modular/bin/basemgr/cobalt/cobalt.h"
#include "src/modular/lib/common/teardown.h"
#include "src/modular/lib/fidl/app_client.h"
#include "src/modular/lib/fidl/clone.h"
#include "src/modular/lib/modular_config/modular_config.h"
#include "src/modular/lib/modular_config/modular_config_constants.h"

namespace modular {

using cobalt_registry::ModularLifetimeEventsMetricDimensionEventType;

// Implementation of the |fuchsia::modular::session::Launcher| protocol.
class LauncherImpl : public fuchsia::modular::session::Launcher {
 public:
  explicit LauncherImpl(modular::BasemgrImpl* basemgr_impl) : basemgr_impl_(basemgr_impl) {}

  // |Launcher|
  void LaunchSessionmgr(fuchsia::mem::Buffer config) override {
    FX_DCHECK(binding_);

    if (basemgr_impl_->state() == BasemgrImpl::State::SHUTTING_DOWN) {
      binding_->Close(ZX_ERR_BAD_STATE);
      return;
    }

    // Read the configuration from the buffer.
    std::string config_str;
    if (auto is_read_ok = fsl::StringFromVmo(config, &config_str); !is_read_ok) {
      binding_->Close(ZX_ERR_INVALID_ARGS);
      return;
    }

    // Parse the configuration.
    auto config_result = modular::ParseConfig(config_str);
    if (config_result.is_error()) {
      binding_->Close(ZX_ERR_INVALID_ARGS);
      return;
    }

    basemgr_impl_->LaunchSessionmgr(config_result.take_value());
  }

  void set_binding(modular::BasemgrImpl::LauncherBinding* binding) { binding_ = binding; }

  DISALLOW_COPY_ASSIGN_AND_MOVE(LauncherImpl);

 private:
  modular::BasemgrImpl* basemgr_impl_;                        // Not owned.
  modular::BasemgrImpl::LauncherBinding* binding_ = nullptr;  // Not owned.
};

BasemgrImpl::BasemgrImpl(modular::ModularConfigAccessor config_accessor,
                         std::shared_ptr<sys::OutgoingDirectory> outgoing_services,
                         BasemgrInspector* inspector, fuchsia::sys::LauncherPtr launcher,
                         fuchsia::ui::policy::PresenterPtr presenter,
                         fuchsia::hardware::power::statecontrol::AdminPtr device_administrator,
                         fit::function<void()> on_shutdown)
    : config_accessor_(std::move(config_accessor)),
      outgoing_services_(std::move(outgoing_services)),
      inspector_(inspector),
      launcher_(std::move(launcher)),
      presenter_(std::move(presenter)),
      device_administrator_(std::move(device_administrator)),
      on_shutdown_(std::move(on_shutdown)),
      session_provider_("SessionProvider"),
      executor_(async_get_default_dispatcher()) {
  outgoing_services_->AddPublicService<fuchsia::modular::Lifecycle>(
      lifecycle_bindings_.GetHandler(this));
  outgoing_services_->AddPublicService(process_lifecycle_bindings_.GetHandler(this),
                                       "fuchsia.process.lifecycle.Lifecycle");
  outgoing_services_->AddPublicService(GetLauncherHandler(),
                                       fuchsia::modular::session::Launcher::Name_);

  ReportEvent(ModularLifetimeEventsMetricDimensionEventType::BootedToBaseMgr);
}

BasemgrImpl::~BasemgrImpl() = default;

void BasemgrImpl::Connect(
    fidl::InterfaceRequest<fuchsia::modular::internal::BasemgrDebug> request) {
  basemgr_debug_bindings_.AddBinding(this, std::move(request));
}

void BasemgrImpl::Start() {
  CreateSessionProvider(&config_accessor_, fuchsia::sys::ServiceList());

  auto start_session_result = StartSession();
  if (start_session_result.is_error()) {
    FX_PLOGS(FATAL, start_session_result.error()) << "Could not start session";
  }
}

void BasemgrImpl::Shutdown() {
  FX_LOGS(INFO) << "Shutting down basemgr";

  // Prevent the shutdown sequence from running twice.
  if (state_ == State::SHUTTING_DOWN) {
    return;
  }

  state_ = State::SHUTTING_DOWN;

  // Teardown the session provider if it exists.
  // Always completes successfully.
  auto teardown_session_provider = [this]() {
    auto bridge = fpromise::bridge();
    if (session_provider_.get()) {
      session_provider_.Teardown(kSessionProviderTimeout, bridge.completer.bind());
    } else {
      bridge.completer.complete_ok();
    }
    return bridge.consumer.promise();
  };

  auto shutdown = teardown_session_provider().and_then([this]() {
    basemgr_debug_bindings_.CloseAll(ZX_OK);
    on_shutdown_();
  });

  executor_.schedule_task(std::move(shutdown));
}

void BasemgrImpl::Terminate() { Shutdown(); }

void BasemgrImpl::Stop() { Shutdown(); }

void BasemgrImpl::CreateSessionProvider(const ModularConfigAccessor* const config_accessor,
                                        fuchsia::sys::ServiceList services_for_sessionmgr) {
  FX_DCHECK(!session_provider_.get());

  session_provider_.reset(new SessionProvider(
      /*delegate=*/this, launcher_.get(), device_administrator_.get(), config_accessor,
      std::move(services_for_sessionmgr),
      /*on_zero_sessions=*/[this] {
        if (state_ == State::SHUTTING_DOWN) {
          return;
        }
        FX_DLOGS(INFO) << "Re-starting due to session closure";
        auto start_session_result = StartSession();
        if (start_session_result.is_error()) {
          FX_PLOGS(FATAL, start_session_result.error()) << "Could not restart session";
        }
      }));
}

BasemgrImpl::StartSessionResult BasemgrImpl::StartSession() {
  if (state_ == State::SHUTTING_DOWN || !session_provider_.get() ||
      session_provider_->is_session_running()) {
    return fpromise::error(ZX_ERR_BAD_STATE);
  }

  auto [view_token, view_holder_token] = scenic::ViewTokenPair::New();
  scenic::ViewRefPair view_ref_pair = scenic::ViewRefPair::New();
  auto view_ref_clone = fidl::Clone(view_ref_pair.view_ref);
  auto start_session_result =
      session_provider_->StartSession(std::move(view_token), std::move(view_ref_pair));
  FX_CHECK(start_session_result.is_ok());

  inspector_->AddSessionStartedAt(zx_clock_get_monotonic());

  // TODO(fxbug.dev/56132): Ownership of the Presenter should be moved to the session shell.
  if (presenter_) {
    presentation_container_ = std::make_unique<PresentationContainer>(
        presenter_.get(), std::move(view_holder_token), std::move(view_ref_clone));
    presenter_.set_error_handler(
        [this](zx_status_t /* unused */) { presentation_container_.reset(); });
  }

  return fpromise::ok();
}

void BasemgrImpl::RestartSession(RestartSessionCallback on_restart_complete) {
  if (state_ == State::SHUTTING_DOWN || !session_provider_.get()) {
    return;
  }
  session_provider_->RestartSession(std::move(on_restart_complete));
}

void BasemgrImpl::StartSessionWithRandomId() {
  // If there is a session already running, exit.
  if (session_provider_.get()) {
    return;
  }
  FX_CHECK(!session_provider_.get());

  Start();
}

void BasemgrImpl::GetPresentation(
    fidl::InterfaceRequest<fuchsia::ui::policy::Presentation> request) {
  if (!presentation_container_) {
    request.Close(ZX_ERR_NOT_FOUND);
    return;
  }
  presentation_container_->GetPresentation(std::move(request));
}

void BasemgrImpl::LaunchSessionmgr(fuchsia::modular::session::ModularConfig config) {
  // If there is a session provider, tear it down and try again. This stops any running session.
  if (session_provider_.get()) {
    session_provider_.Teardown(kSessionProviderTimeout,
                               [this, config = std::move(config)]() mutable {
                                 session_provider_.reset(nullptr);
                                 LaunchSessionmgr(std::move(config));
                               });
    return;
  }

  launch_sessionmgr_config_accessor_ =
      std::make_unique<modular::ModularConfigAccessor>(std::move(config));
  fuchsia::sys::ServiceList additional_services{};

  CreateSessionProvider(launch_sessionmgr_config_accessor_.get(), std::move(additional_services));

  if (auto result = StartSession(); result.is_error()) {
    FX_PLOGS(ERROR, result.error()) << "Could not start session";
  }
}

fidl::InterfaceRequestHandler<fuchsia::modular::session::Launcher>
BasemgrImpl::GetLauncherHandler() {
  return [this](fidl::InterfaceRequest<fuchsia::modular::session::Launcher> request) {
    auto impl = std::make_unique<LauncherImpl>(this);
    session_launcher_bindings_.AddBinding(std::move(impl), std::move(request),
                                          /*dispatcher=*/nullptr);
    const auto& binding = session_launcher_bindings_.bindings().back().get();
    binding->impl()->set_binding(binding);
  };
}

}  // namespace modular
