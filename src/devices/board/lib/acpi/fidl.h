// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_LIB_ACPI_FIDL_H_
#define SRC_DEVICES_BOARD_LIB_ACPI_FIDL_H_

#include <fidl/fuchsia.hardware.acpi/cpp/wire.h>
#include <lib/ddk/driver.h>

#include <forward_list>

#include <acpica/acpi.h>

#include "src/devices/board/lib/acpi/acpi.h"

namespace acpi {

// Helper class used to implement AcpiEvaluateObject over FIDL.
class EvaluateObjectFidlHelper {
 public:
  using EvaluateObjectRequestView =
      fidl::WireServer<fuchsia_hardware_acpi::Device>::EvaluateObjectRequestView;
  EvaluateObjectFidlHelper(zx_handle_t mmio_resource, acpi::Acpi* acpi, ACPI_HANDLE device,
                           std::string request_path,
                           fuchsia_hardware_acpi::wire::EvaluateObjectMode mode,
                           fidl::VectorView<fuchsia_hardware_acpi::wire::Object> params)
      : acpi_(acpi),
        device_handle_(device),
        request_path_(std::move(request_path)),
        mode_(mode),
        request_params_(params),
        mmio_resource_(std::move(mmio_resource)) {}

  static EvaluateObjectFidlHelper FromRequest(zx_handle_t mmio_resource, acpi::Acpi* acpi,
                                              ACPI_HANDLE device,
                                              EvaluateObjectRequestView& request);

  // Calls AcpiEvaluateObject using the arguments supplied to the constructor in |request|, and
  // replies on the completer given in |completer| if the call succeeds.
  // If something fails, acpi::error() is returned with a value that indicates what went wrong.
  acpi::status<fuchsia_hardware_acpi::wire::DeviceEvaluateObjectResult> Evaluate(
      fidl::AnyArena& alloc);

  // These methods are public for unit tests only.
  // Validate that the path supplied in the request is a child of the device,
  // and returns the absolute path to the supplied path.
  acpi::status<std::string> ValidateAndLookupPath(const char* request_path,
                                                  ACPI_HANDLE* hnd = nullptr);

  // Convert the given FIDL parameters to ACPI_OBJECTs, and put them in the params_ vector.
  acpi::status<std::vector<ACPI_OBJECT>> DecodeParameters(
      fidl::VectorView<fuchsia_hardware_acpi::wire::Object>& request_params);

  // Take the given ACPI_OBJECT and turn it into a DeviceEvaluateObjectResult.
  acpi::status<fuchsia_hardware_acpi::wire::DeviceEvaluateObjectResult> EncodeReturnValue(
      fidl::AnyArena& alloc, ACPI_OBJECT* value);

  // Take the given ACPI_OBJECT and parse it as resources, turning it into a
  // DeviceEvaluateObjectResult.
  acpi::status<fuchsia_hardware_acpi::wire::DeviceEvaluateObjectResult> EncodeResourcesReturnValue(
      fidl::AnyArena& alloc, ACPI_OBJECT* value);

  // Encode an ACPICA MMIO resource into a FIDL resource.
  acpi::status<fuchsia_hardware_acpi::wire::Resource> EncodeMmioResource(fidl::AnyArena& alloc,
                                                                         ACPI_RESOURCE* resource);

  // Encode an ACPICA object into a FIDL object.
  acpi::status<fuchsia_hardware_acpi::wire::Object> EncodeObject(fidl::AnyArena& alloc,
                                                                 ACPI_OBJECT* value);
  // Decode a FIDL object into an ACPICA object.
  acpi::status<> DecodeObject(const fuchsia_hardware_acpi::wire::Object& obj, ACPI_OBJECT* out);

  // For unit testing.
  void SetMmioResource(zx_handle_t rsrc) { mmio_resource_ = rsrc; }

 private:
  // State that comes from outside.
  acpi::Acpi* acpi_;
  ACPI_HANDLE device_handle_;
  std::string request_path_;
  fuchsia_hardware_acpi::wire::EvaluateObjectMode mode_;
  fidl::VectorView<fuchsia_hardware_acpi::wire::Object> request_params_;
  zx_handle_t mmio_resource_;

  // Objects allocated while converting from FIDL to ACPICA objects.
  std::forward_list<std::vector<ACPI_OBJECT>> allocated_packages_;
  std::forward_list<std::string> allocated_strings_;
};

}  // namespace acpi

#endif  // SRC_DEVICES_BOARD_LIB_ACPI_FIDL_H_
