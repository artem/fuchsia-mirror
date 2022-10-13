// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_DRIVER_MANAGER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_DRIVER_MANAGER_H_

#include <lib/ddk/device.h>

#include <memory>

#include "src/devices/bin/driver_manager/driver_loader.h"

class Coordinator;
class CompositeDevice;

using CompositeDeviceMap = std::unordered_map<std::string, std::unique_ptr<CompositeDevice>>;

class BindDriverManager {
 public:
  BindDriverManager(const BindDriverManager&) = delete;
  BindDriverManager& operator=(const BindDriverManager&) = delete;
  BindDriverManager(BindDriverManager&&) = delete;
  BindDriverManager& operator=(BindDriverManager&&) = delete;

  explicit BindDriverManager(Coordinator* coordinator);
  ~BindDriverManager();

  zx_status_t BindDriverToDevice(const MatchedDriver& driver, const fbl::RefPtr<Device>& dev);

  // Try binding a driver to the device. Returns ZX_ERR_ALREADY_BOUND if there
  // is a driver bound to the device and the device is not allowed to be bound multiple times.
  zx_status_t BindDevice(const fbl::RefPtr<Device>& dev, std::string_view drvlibname,
                         bool new_device);

  // Given a device, return all of the Drivers whose bind programs match with the device.
  // The returned vector is organized by priority, so if only one driver is being bound it
  // should be the first in the vector.
  // If |drvlibname| is not empty then the device will only be checked against the driver
  // with that specific name.
  zx::status<std::vector<MatchedDriver>> GetMatchingDrivers(const fbl::RefPtr<Device>& dev,
                                                            std::string_view drvlibname);

  // Binds all the devices to the drivers in the Driver Index.
  void BindAllDevicesDriverIndex(const DriverLoader::MatchDeviceConfig& config);

  // Find matching device group nodes for |dev| through the Driver Index and then bind them.
  zx_status_t MatchAndBindDeviceGroups(const fbl::RefPtr<Device>& dev);

 private:
  // Find and return matching drivers for |dev| in the Driver Index.
  zx::status<std::vector<MatchedDriver>> MatchDeviceWithDriverIndex(
      const fbl::RefPtr<Device>& dev, const DriverLoader::MatchDeviceConfig& config) const;

  // Find matching drivers for |dev| through the Driver Index and then bind them.
  zx_status_t MatchAndBindWithDriverIndex(const fbl::RefPtr<Device>& dev,
                                          const DriverLoader::MatchDeviceConfig& config);

  // Binds the matched fragment in |driver| to |dev|. If a CompositeDevice for |driver| doesn't
  // exists in |driver_index_composite_devices_|, this function creates and adds it.
  zx_status_t BindDriverToFragment(const MatchedCompositeDriverInfo& driver,
                                   const fbl::RefPtr<Device>& dev);

  // Owner. Must outlive BindDriverManager.
  Coordinator* coordinator_;

  // All the composite devices received from the DriverIndex.
  // This maps driver URLs to the CompositeDevice object.
  CompositeDeviceMap driver_index_composite_devices_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_DRIVER_MANAGER_H_
