// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_FIRMWARE_LOADER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_FIRMWARE_LOADER_H_

// A loader for Intel Bluetooth Firmware files.

#include <lib/zx/channel.h>

#include "vendor_hci.h"

namespace btintel {

// Secure boot engine type. The algorithm used to sign the firmware binary.
enum class SecureBootEngineType {
  kRSA,    // Rivest–Shamir–Adleman
  kECDSA,  // Elliptic Curve Digital Signature Algorithm
};

class FirmwareLoader {
 public:
  // |cmd_channel| is expected to outlive this object.
  FirmwareLoader(zx::channel* cmd_channel, zx::channel* acl_channel)
      : hci_cmd_(cmd_channel), hci_acl_(acl_channel) {}
  ~FirmwareLoader() = default;

  enum class LoadStatus {
    // Firmware is complete, no patch loaded, ready.
    kComplete,
    // Patch is loaded, reset the controller with patches enabled to continue
    kPatched,
    // An unexpected event was returned from the controller
    kError,
  };

  // Loads a "bseq" firmware into the controller using the given channels.
  // Returns a LoadStatus indicating the result.
  //  - kComplete if the firmware was loaded successfully
  //  - kPatched if the firmware was loaded and a patch was added, meaning the
  //  controller should be reset.
  //  - kError otherwise.
  //  |firmware| should be a pointer into readable memory representing the file
  //  of at least |len| bytes.
  LoadStatus LoadBseq(const void* firmware, const size_t& len);

  // Loads "sfi" firmware into the controller using the channels.
  // |firmware| should be a pointer to firmware, which is at
  // least |len| bytes long.
  // | engine_type| shall indicate the secure boot engine type specified in the firmware binary.
  // if |boot_addr| is not null, the boot params boot address written to the device will be placed
  // within it.
  // Returns kComplete if the file was loaded, kError otherwise.
  LoadStatus LoadSfi(const void* firmware, const size_t& len, SecureBootEngineType engine_type,
                     uint32_t* boot_addr);

 private:
  bool ParseBseq();

  // The command channel from the USB transport
  VendorHci hci_cmd_;
  // The ACL data channel from the USB transport
  VendorHci hci_acl_;
};

}  // namespace btintel

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_FIRMWARE_LOADER_H_
