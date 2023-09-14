// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <algorithm>
#include <limits>

#include <fuzzer/FuzzedDataProvider.h>
#include <pw_async/fake_dispatcher.h>
#include <pw_async_fuchsia/dispatcher.h>

#include "enhanced_retransmission_mode_engines.h"
#include "src/connectivity/bluetooth/core/bt-host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/l2cap/fragmenter.h"
#include "src/connectivity/bluetooth/core/bt-host/l2cap/l2cap_defs.h"

constexpr static bt::hci_spec::ConnectionHandle kTestHandle = 0x0001;
constexpr bt::l2cap::ChannelId kTestChannelId = 0x0001;

void NoOpTxCallback(bt::ByteBufferPtr) {}

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  FuzzedDataProvider provider(data, size);

  pw::async::test::FakeDispatcher dispatcher;

  uint8_t tx_window = std::max(provider.ConsumeIntegral<uint8_t>(), static_cast<uint8_t>(1u));

  uint8_t max_transmissions = provider.ConsumeIntegral<uint8_t>();
  uint16_t max_tx_sdu_size = std::max(provider.ConsumeIntegral<uint16_t>(), bt::l2cap::kMinACLMTU);

  bool failure = false;
  auto failure_cb = [&failure] { failure = true; };

  auto [rx_engine, tx_engine] = bt::l2cap::internal::MakeLinkedEnhancedRetransmissionModeEngines(
      kTestChannelId, max_tx_sdu_size, max_transmissions, tx_window, NoOpTxCallback, failure_cb,
      dispatcher);

  // In the real stack, the engines are shut down on failure, so we do the same here.
  while (provider.remaining_bytes() > 0 && !failure) {
    bool tx = provider.ConsumeBool();
    if (tx) {
      auto n_bytes = provider.ConsumeIntegral<uint16_t>();
      auto bytes = provider.ConsumeBytes<uint8_t>(n_bytes);
      tx_engine->QueueSdu(std::make_unique<bt::DynamicByteBuffer>(bt::BufferView(bytes)));
    } else {
      bt::l2cap::Fragmenter fragmenter(kTestHandle);
      auto n_bytes = provider.ConsumeIntegral<uint16_t>();
      auto bytes = provider.ConsumeBytes<uint8_t>(n_bytes);
      bool append_fcs = provider.ConsumeBool();
      if (append_fcs) {
        const size_t bounded_size =
            std::min(bytes.size(),
                     std::numeric_limits<uint16_t>::max() - sizeof(bt::l2cap::FrameCheckSequence));
        bytes.resize(bounded_size);
      }
      auto fcs_option = append_fcs ? bt::l2cap::FrameCheckSequenceOption::kIncludeFcs
                                   : bt::l2cap::FrameCheckSequenceOption::kNoFcs;
      auto pdu = fragmenter.BuildFrame(kTestChannelId, bt::BufferView(bytes), fcs_option);
      rx_engine->ProcessPdu(std::move(pdu));
    }

    // Run for 0-255 seconds, which is enough to trigger poll timer and monitor timer.
    auto run_duration = std::chrono::seconds(provider.ConsumeIntegral<uint8_t>());
    dispatcher.RunFor(run_duration);
  }

  return 0;
}
