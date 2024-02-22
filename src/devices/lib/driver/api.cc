// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/syslog/logger.h>

extern "C" {

__EXPORT zx_status_t device_add_from_driver(zx_driver_t* drv, zx_device_t* parent,
                                            device_add_args_t* args, zx_device_t** out) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT void device_init_reply(zx_device_t* dev, zx_status_t status,
                                const device_init_reply_args_t* args) {}

__EXPORT void device_async_remove(zx_device_t* dev) {}

__EXPORT void device_unbind_reply(zx_device_t* dev) {}

__EXPORT void device_suspend_reply(zx_device_t* dev, zx_status_t status, uint8_t out_state) {}

__EXPORT void device_resume_reply(zx_device_t* dev, zx_status_t status, uint8_t out_power_state,
                                  uint32_t out_perf_state) {}

__EXPORT zx_status_t device_set_profile_by_role(zx_device_t* device, zx_handle_t thread,
                                                const char* role, size_t role_size) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_get_protocol(const zx_device_t* dev, uint32_t proto_id, void* out) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_connect_runtime_protocol(zx_device_t* dev, const char* service_name,
                                                     const char* protocol_name,
                                                     fdf_handle_t request) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_connect_fragment_runtime_protocol(zx_device_t* dev,
                                                              const char* fragment_name,
                                                              const char* service_name,
                                                              const char* protocol_name,
                                                              fdf_handle_t request) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_handle_t get_root_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_ioport_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_mmio_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_irq_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_info_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_smc_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_framebuffer_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_iommu_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_msi_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_handle_t get_power_resource(zx_device_t* dev) { return ZX_ERR_NOT_SUPPORTED; }

__EXPORT zx_status_t load_firmware_from_driver(zx_driver_t* drv, zx_device_t* dev, const char* path,
                                               zx_handle_t* fw, size_t* size) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_get_metadata(zx_device_t* dev, uint32_t type, void* buf, size_t buflen,
                                         size_t* actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_get_metadata_size(zx_device_t* dev, uint32_t type, size_t* out_size) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_add_metadata(zx_device_t* dev, uint32_t type, const void* data,
                                         size_t length) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_add_composite_deprecated(zx_device_t* dev, const char* name,
                                                     const composite_device_desc_t* comp_desc) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT bool driver_log_severity_enabled_internal(const zx_driver_t* drv, fx_log_severity_t flag) {
  return false;
}

__EXPORT void driver_logvf_internal(const zx_driver_t* drv, fx_log_severity_t flag, const char* tag,
                                    const char* file, int line, const char* msg, va_list args) {}

__EXPORT void driver_logf_internal(const zx_driver_t* drv, fx_log_severity_t flag, const char* tag,
                                   const char* file, int line, const char* msg, ...) {}

__EXPORT zx_status_t device_get_fragment_protocol(zx_device_t* dev, const char* name,
                                                  uint32_t proto_id, void* out) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_get_fragment_metadata(zx_device_t* dev, const char* name, uint32_t type,
                                                  void* buf, size_t buflen, size_t* actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_connect_fidl_protocol(zx_device_t* device, const char* protocol_name,
                                                  zx_handle_t request) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_get_variable(zx_device_t* device, const char* name, char* out,
                                         size_t out_size, size_t* size_actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_add_composite_spec(zx_device_t* dev, const char* name,
                                               const composite_node_spec_t* spec) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_connect_fidl_protocol2(zx_device_t* device, const char* service_name,
                                                   const char* protocol_name, zx_handle_t request) {
  return ZX_ERR_NOT_SUPPORTED;
}

__EXPORT zx_status_t device_connect_fragment_fidl_protocol(zx_device_t* device,
                                                           const char* fragment_name,
                                                           const char* service_name,
                                                           const char* protocol_name,
                                                           zx_handle_t request) {
  return ZX_ERR_NOT_SUPPORTED;
}
}
