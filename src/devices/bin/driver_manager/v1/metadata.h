// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_METADATA_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_METADATA_H_

#include <stdint.h>

#include <memory>

#include <fbl/intrusive_double_list.h>

struct Metadata : public fbl::DoublyLinkedListable<std::unique_ptr<Metadata>> {
  uint32_t type;
  uint32_t length;

  char* Data() { return reinterpret_cast<char*>(this + 1); }

  const char* Data() const { return reinterpret_cast<const char*>(this + 1); }

  static zx_status_t Create(size_t data_len, std::unique_ptr<Metadata>* out) {
    uint8_t* buf = new uint8_t[sizeof(Metadata) + data_len];
    if (!buf) {
      return ZX_ERR_NO_MEMORY;
    }
    new (buf) Metadata();

    out->reset(reinterpret_cast<Metadata*>(buf));
    return ZX_OK;
  }

  // Implement a custom delete to deal with the allocation mechanism used in
  // Create().  Since the ctor is private, all Metadata* will come from
  // Create().
  void operator delete(void* ptr) { delete[] reinterpret_cast<uint8_t*>(ptr); }

 private:
  Metadata() = default;

  Metadata(const Metadata&) = delete;
  Metadata& operator=(const Metadata&) = delete;

  Metadata(Metadata&&) = delete;
  Metadata& operator=(Metadata&&) = delete;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_METADATA_H_
