// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_TESTING_NETEMUL_NETWORK_CONTEXT_LIB_NETDUMP_H_
#define SRC_CONNECTIVITY_NETWORK_TESTING_NETEMUL_NETWORK_CONTEXT_LIB_NETDUMP_H_

#include <fuchsia/netemul/network/cpp/fidl.h>

#include <cstdint>
#include <iostream>
#include <optional>
#include <sstream>

#include "src/lib/fxl/macros.h"

namespace netemul {

class NetworkDump {
 public:
  NetworkDump() : out_(nullptr) {}
  NetworkDump(std::ostream* out) : out_(out) { WriteHeaders(); }
  virtual ~NetworkDump() = default;
  void WritePacket(const void* data, size_t len, uint32_t interface = 0);
  uint32_t AddInterface(const std::string& name);
  // Add an interface, allowing the caller to generate the interface name based
  // on the index.
  template <typename F>
  uint32_t AddInterfaceWith(F f) {
    return this->AddInterface(f(interface_counter_));
  }
  size_t packet_count() const { return packet_count_; }

 protected:
  void SetOut(std::ostream* out) {
    ZX_DEBUG_ASSERT(out_ == nullptr);
    ZX_DEBUG_ASSERT(out != nullptr);
    out_ = out;
    WriteHeaders();
  }

 private:
  void Write(const void* data, size_t len);
  void WriteOption(uint16_t type, const void* data, uint16_t len);
  void WriteHeaders();
  std::ostream* out_;
  uint32_t interface_counter_ = 0;
  size_t packet_count_ = 0;

  FXL_DISALLOW_COPY_AND_ASSIGN(NetworkDump);
};

class InMemoryDump : public NetworkDump {
 public:
  InMemoryDump() : NetworkDump(), mem_() { SetOut(&mem_); }

  void DumpHex(std::ostream* out) const;
  std::vector<uint8_t> CopyBytes() const;

 private:
  std::stringstream mem_;

  FXL_DISALLOW_COPY_AND_ASSIGN(InMemoryDump);
};

template <typename D>
class NetWatcher {
 public:
  template <typename... Args>
  explicit NetWatcher(Args... args) : dump_(args...), got_data_(false) {}

  void Watch(const std::string& name,
             fidl::InterfacePtr<fuchsia::netemul::network::FakeEndpoint> ep) {
    uint32_t id = dump_.AddInterface(name);
    size_t index = fake_eps_.size();
    fake_eps_.push_back(std::move(ep));
    ReadEp(index, id);
  }

  void ReadEp(size_t index, uint32_t id) {
    fake_eps_[index]->Read([this, index, id](std::vector<uint8_t> data, uint64_t _dropped) {
      got_data_ = true;
      dump_.WritePacket(data.data(), data.size(), id);
      ReadEp(index, id);
    });
  }

  bool HasData() const { return got_data_; }

  const D& dump() const { return dump_; }

 private:
  std::vector<fidl::InterfacePtr<fuchsia::netemul::network::FakeEndpoint>> fake_eps_;
  D dump_;
  bool got_data_;

  FXL_DISALLOW_COPY_AND_ASSIGN(NetWatcher);
};

}  // namespace netemul

#endif  // SRC_CONNECTIVITY_NETWORK_TESTING_NETEMUL_NETWORK_CONTEXT_LIB_NETDUMP_H_
