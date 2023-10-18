// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver_loader.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <sched.h>
#include <unistd.h>
#include <zircon/threads.h>

#include <thread>
#include <variant>

#include <fbl/unique_fd.h>

#include "src/devices/bin/driver_manager/v1/manifest_parser.h"
#include "src/devices/lib/log/log.h"

namespace {

bool VerifyMatchedCompositeNodeParentInfo(fidl::VectorView<fdf::wire::CompositeParent> info) {
  if (info.empty()) {
    return false;
  }

  for (auto& spec : info) {
    if (!spec.has_composite() || !spec.composite().has_spec() ||
        !spec.composite().spec().has_name() || spec.composite().spec().name().empty() ||
        !spec.has_index()) {
      return false;
    }
  }

  return true;
}

}  // namespace

const Driver* DriverLoader::UrlToDriver(const std::string& url) {
  if (driver_index_drivers_.count(url) == 0) {
    return nullptr;
  }
  return driver_index_drivers_[url].get();
}

void DriverLoader::WatchForDrivers(fit::function<void()> on_driver_loaded) {
  if (!driver_index_.is_valid()) {
    LOGF(ERROR, "%s: DriverIndex is not initialized", __func__);
    return;
  }

  driver_index_->WatchForDriverLoad().Then(
      [this, on_driver_loaded = std::move(on_driver_loaded)](
          fidl::WireUnownedResult<fdi::DriverIndex::WatchForDriverLoad>& result) mutable {
        if (!result.ok()) {
          // Since IsolatedDevmgr doesn't use the ComponentFramework, DriverIndex can be
          // closed before DriverManager during tests, which would mean we would see
          // a ZX_ERR_PEER_CLOSED.
          if (result.status() == ZX_ERR_PEER_CLOSED) {
            LOGF(WARNING, "Connection to DriverIndex closed during WatchForDriverLoad.");
          } else {
            LOGF(ERROR, "DriverIndex::WatchForDriverLoad failed with: %s",
                 result.error().FormatDescription().c_str());
          }
          return;
        }

        on_driver_loaded();
        WatchForDrivers(std::move(on_driver_loaded));
      });
}

const Driver* DriverLoader::LoadDriverUrl(const std::string& manifest_url, bool use_full_resolver) {
  // Check if we've already loaded this driver. If we have then return it.
  if (const Driver* driver = UrlToDriver(manifest_url); driver != nullptr) {
    return driver;
  }
  // Pick the correct package resolver to use.
  internal::PackageResolverInterface* resolver = base_resolver_;
  if (use_full_resolver && full_resolver_) {
    resolver = full_resolver_;
  }

  // We've never seen the driver before so add it, then return it.
  zx::result fetched_driver = resolver->FetchDriver(manifest_url);
  if (fetched_driver.is_error()) {
    LOGF(ERROR, "Error fetching driver: %s: %d", manifest_url.data(), fetched_driver.error_value());
    return nullptr;
  }
  // It's possible the driver is nullptr if it was disabled.
  if (!fetched_driver.value()) {
    return nullptr;
  }

  // Success. Return driver.
  driver_index_drivers_[manifest_url] = std::move(fetched_driver.value());
  return driver_index_drivers_[manifest_url].get();
}

void DriverLoader::AddCompositeNodeSpec(fuchsia_driver_framework::wire::CompositeNodeSpec spec,
                                        AddToIndexCallback callback) {
  auto result = driver_index_.sync()->AddCompositeNodeSpec(spec);
  if (!result.ok()) {
    LOGF(ERROR, "DriverIndex::AddCompositeNodeSpec failed %d", result.status());
    callback(zx::error(result.status()));
    return;
  }
  if (result->is_error()) {
    callback(result->take_error());
    return;
  }

  callback(zx::ok());
}

const std::vector<MatchedDriver> DriverLoader::MatchDeviceDriverIndex(
    const fbl::RefPtr<Device>& dev, const MatchDeviceConfig& config) {
  return MatchDeviceDriverIndex(dev->name().data(), dev->props(), dev->str_props(),
                                dev->protocol_id(), config);
}

const std::vector<MatchedDriver> DriverLoader::MatchDeviceDriverIndex(
    std::string_view name, const fbl::Array<const zx_device_prop_t>& props,
    const fbl::Array<const StrProperty>& str_props, uint32_t protocol_id,
    const MatchDeviceConfig& config) {
  if (!driver_index_.is_valid()) {
    return std::vector<MatchedDriver>();
  }

  bool autobind = config.driver_url_suffix.empty();

  fidl::Arena allocator;
  size_t size = props.size() + str_props.size() + 2;
  fidl::VectorView<fdf::wire::NodeProperty> fidl_props(allocator, size);

  size_t index = 0;
  fidl_props[index++] = fdf::MakeProperty(allocator, BIND_PROTOCOL, protocol_id);
  fidl_props[index++] = fdf::MakeProperty(allocator, BIND_AUTOBIND, autobind);
  for (size_t i = 0; i < props.size(); i++) {
    fidl_props[index++] = fdf::MakeProperty(allocator, props[i].id, props[i].value);
  }

  for (size_t i = 0; i < str_props.size(); i++) {
    switch (str_props[i].value.index()) {
      case StrPropValueType::Integer: {
        fidl_props[index++] = fdf::MakeProperty(
            allocator, str_props[i].key, std::get<StrPropValueType::Integer>(str_props[i].value));
        break;
      }
      case StrPropValueType::String: {
        fidl_props[index++] = fdf::MakeProperty(
            allocator, str_props[i].key, std::get<StrPropValueType::String>(str_props[i].value));
        break;
      }
      case StrPropValueType::Bool: {
        fidl_props[index++] = fdf::MakeProperty(
            allocator, str_props[i].key, std::get<StrPropValueType::Bool>(str_props[i].value));
        break;
      }
      case StrPropValueType::Enum: {
        fidl_props[index++] = fdf::MakeProperty(
            allocator, str_props[i].key, std::get<StrPropValueType::Enum>(str_props[i].value));
        break;
      }
    }
  }

  return MatchPropertiesDriverIndex(name, fidl_props, config);
}

const std::vector<MatchedDriver> DriverLoader::MatchPropertiesDriverIndex(
    std::string_view name, fidl::VectorView<fdf::wire::NodeProperty> props,
    const MatchDeviceConfig& config) {
  std::vector<MatchedDriver> matched_drivers;
  if (!driver_index_.is_valid()) {
    return matched_drivers;
  }

  fidl::Arena allocator;
  auto args = fdi::wire::MatchDriverArgs::Builder(allocator);
  args.properties(std::move(props));
  if (!config.driver_url_suffix.empty()) {
    args.driver_url_suffix(config.driver_url_suffix);
  }

  auto result = driver_index_.sync()->MatchDriver(args.Build());
  if (!result.ok()) {
    if (result.status() != ZX_OK) {
      LOGF(ERROR, "DriverIndex::MatchDriversV1 for '%s' failed: %s", std::string(name).c_str(),
           result.status_string());
      return matched_drivers;
    }
  }
  // If there's no driver to match then DriverIndex will return ZX_ERR_NOT_FOUND.
  if (result->is_error()) {
    if (result->error_value() != ZX_ERR_NOT_FOUND) {
      LOGF(ERROR, "DriverIndex: MatchDriversV1 for '%s' returned error: %d",
           std::string(name).c_str(), result->error_value());
    }
    return matched_drivers;
  }

  const auto& match_driver_result = result->value();
  if (match_driver_result->is_composite_parents()) {
    if (!VerifyMatchedCompositeNodeParentInfo(match_driver_result->composite_parents())) {
      LOGF(ERROR,
           "DriverIndex: MatchDriverV1 response is missing fields in MatchedCompositeNodeSpecInfo");
      return matched_drivers;
    }

    matched_drivers.push_back(fidl::ToNatural(match_driver_result->composite_parents()).value());
    return matched_drivers;
  }

  ZX_ASSERT(match_driver_result->is_driver());

  MatchedDriverInfo matched_driver_info = {};

  auto fidl_driver_info = match_driver_result->driver();
  if (!fidl_driver_info.has_url()) {
    LOGF(ERROR, "DriverIndex: MatchDriversV1 response is missing url");
    return matched_drivers;
  }
  matched_driver_info.component_url = std::string(fidl_driver_info.url().get());

  if (fidl_driver_info.has_colocate()) {
    matched_driver_info.colocate = fidl_driver_info.colocate();
  }
  if (fidl_driver_info.has_package_type()) {
    matched_driver_info.package_type = fidl_driver_info.package_type();
  }

  if (fidl_driver_info.has_driver_framework_version()) {
    matched_driver_info.is_dfv2 = fidl_driver_info.driver_framework_version() == 2;
  }

  if (fidl_driver_info.has_package_type()) {
    matched_driver_info.package_type = fidl_driver_info.package_type();
  }

  if (config.driver_url_suffix.empty() ||
      cpp20::ends_with(std::string_view(matched_driver_info.component_url),
                       config.driver_url_suffix)) {
    matched_drivers.push_back(matched_driver_info);
  }

  return matched_drivers;
}
