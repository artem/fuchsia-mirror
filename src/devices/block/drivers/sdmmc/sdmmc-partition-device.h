// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_PARTITION_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_PARTITION_DEVICE_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <zircon/types.h>

#include <cinttypes>

#include "sdmmc-types.h"

namespace sdmmc {

class SdmmcBlockDevice;

class PartitionDevice : public ddk::BlockImplProtocol<PartitionDevice>,
                        public ddk::BlockPartitionProtocol<PartitionDevice> {
 public:
  PartitionDevice(SdmmcBlockDevice* sdmmc_parent, const block_info_t& block_info,
                  EmmcPartition partition);

  zx_status_t AddDevice();

  void BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out);
  void BlockImplQueue(block_op_t* btxn, block_impl_queue_callback completion_cb, void* cookie);

  zx_status_t BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid);
  zx_status_t BlockPartitionGetName(char* out_name, size_t capacity);

  // Visible for testing.
  EmmcPartition partition() const { return partition_; }
  const block_impl_protocol_ops_t& block_impl_protocol_ops() const {
    return block_impl_protocol_ops_;
  }

  fdf::Logger& logger();

 private:
  SdmmcBlockDevice* const sdmmc_parent_;
  const block_info_t block_info_;
  const EmmcPartition partition_;

  std::string partition_name_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  compat::BanjoServer block_impl_server_{ZX_PROTOCOL_BLOCK_IMPL, this, &block_impl_protocol_ops_};
  std::optional<compat::BanjoServer> block_partition_server_;
  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_PARTITION_DEVICE_H_
