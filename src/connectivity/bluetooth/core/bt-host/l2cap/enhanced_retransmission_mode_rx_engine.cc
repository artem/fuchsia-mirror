// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/l2cap/enhanced_retransmission_mode_rx_engine.h"

#include <type_traits>

namespace bt::l2cap::internal {

namespace {

template <typename T>
std::optional<T> TryCopyFromPdu(const PDU& pdu) {
  if (pdu.length() < sizeof(T))
    return std::nullopt;

  StaticByteBuffer<sizeof(T)> buf;
  pdu.Copy(&buf, 0, sizeof(T));
  return buf.template To<T>();
}

std::variant<std::monostate, const SimpleInformationFrameHeader, const SimpleStartOfSduFrameHeader,
             const SimpleSupervisoryFrame>
GetFrameHeaderFromPdu(const PDU& pdu) {
  const auto control_field_opt = TryCopyFromPdu<EnhancedControlField>(pdu);
  if (!control_field_opt) {
    // TODO(https://fxbug.dev/1306): Add metric counting runt frames.
    return std::monostate();
  }

  const auto& control_field = control_field_opt.value();
  if (control_field.designates_supervisory_frame()) {
    const auto frame_opt = TryCopyFromPdu<SimpleSupervisoryFrame>(pdu);
    if (!frame_opt) {
      // TODO(https://fxbug.dev/1306): Add metric counting runt S-frames.
      return std::monostate();
    }
    return frame_opt.value();
  }

  if (control_field.designates_start_of_segmented_sdu()) {
    const auto frame_opt = TryCopyFromPdu<SimpleStartOfSduFrameHeader>(pdu);
    if (!frame_opt) {
      // TODO(https://fxbug.dev/1306): Add metric counting runt Start-of-SDU frames.
      return std::monostate();
    }
    return frame_opt.value();
  }

  const auto frame_opt = TryCopyFromPdu<SimpleInformationFrameHeader>(pdu);
  if (!frame_opt) {
    // TODO(https://fxbug.dev/1306): Add metric counting runt I-frames.
    return std::monostate();
  }
  return frame_opt.value();
}

template <typename T>
constexpr bool kContainsEnhancedControlField = std::is_base_of_v<EnhancedControlField, T>;

bool IsMpsValid(const PDU& pdu) {
  // TODO(quiche): Check PDU's length against the MPS.
  return true;
}

}  // namespace

using Engine = EnhancedRetransmissionModeRxEngine;

Engine::EnhancedRetransmissionModeRxEngine(SendFrameCallback send_frame_callback,
                                           ConnectionFailureCallback connection_failure_callback)
    : next_seqnum_(0),
      remote_is_busy_(false),
      send_frame_callback_(std::move(send_frame_callback)),
      connection_failure_callback_(std::move(connection_failure_callback)) {}

ByteBufferPtr Engine::ProcessPdu(PDU pdu) {
  // A note on validation (see Vol 3, Part A, 3.3.7):
  //
  // We skip step 1 (validation of the Channel ID), as a frame with an
  // unrecognized Channel ID will not be delivered to us. (Various
  // ChannelManagerTest test cases verify that LogicalLink directs frames
  // to their proper channels.)
  //
  // We skip step 2 (validation of FCS), as we don't support FCS.
  //
  // Step 3 (size checking) is implemented in IsMpsValid(), and
  // GetFrameHeaderFromPdu().
  //
  // TODO(quiche): Implement step 4/5 (Check SAR bits, close connection on
  // error).

  if (!IsMpsValid(pdu)) {
    // TODO(quiche): Close connection.
    // TODO(https://fxbug.dev/1306): Add metric counting oversized frames.
    return nullptr;
  }

  auto header = GetFrameHeaderFromPdu(pdu);
  auto frame_processor = [this, pdu = std::move(pdu)](auto header) mutable {
    // Run ProcessFrame first so it can perform the highest-priority actions like assigning
    // RemoteBusy (Core Spec v5.0, Vol 3, Part A, Sec 8.6.5.9).
    auto sdu = ProcessFrame(header, std::move(pdu));

    // This implements the PassToTx action ("Pass the ReqSeq and F-bit value") per Core Spec v5.0,
    // Vol 3, Part A, 8.6.5.6 and must come after updates to the RemoteBusy variable in order to
    // avoid transmitting frames when the peer can't accept them.
    if constexpr (kContainsEnhancedControlField<decltype(header)>) {
      if (receive_seq_num_callback_) {
        receive_seq_num_callback_(header.receive_seq_num(), header.is_poll_response());
      }
    }
    return sdu;
  };
  return std::visit(std::move(frame_processor), header);
}

ByteBufferPtr Engine::ProcessFrame(const SimpleInformationFrameHeader header, PDU pdu) {
  if (header.tx_seq() != next_seqnum_) {
    // TODO(quiche): Send REJ frame.
    // TODO(quiche): Add histogram for |header.tx_seq() - next_seqnum_|. This
    // will give us an upper bound on the potential benefit of sending SREJ
    // frames.
    return nullptr;
  }

  // TODO(quiche): check if the frame is within the permitted window.

  if (header.designates_part_of_segmented_sdu()) {
    // TODO(quiche): Implement validation and handling of segmented frames.
    return nullptr;
  }

  AdvanceSeqNum();

  if (ack_seq_num_callback_) {
    ack_seq_num_callback_(next_seqnum_);
  }

  SimpleReceiverReadyFrame ack_frame;
  ack_frame.set_receive_seq_num(next_seqnum_);
  send_frame_callback_(
      std::make_unique<DynamicByteBuffer>(BufferView(&ack_frame, sizeof(ack_frame))));

  const auto header_len = sizeof(header);
  const auto footer_len = sizeof(FrameCheckSequence);
  if (pdu.length() < header_len + footer_len) {
    return nullptr;
  }
  const auto payload_len = pdu.length() - header_len - footer_len;
  auto sdu = std::make_unique<DynamicByteBuffer>(payload_len);
  pdu.Copy(sdu.get(), header_len, payload_len);
  return sdu;
}

ByteBufferPtr Engine::ProcessFrame(const SimpleStartOfSduFrameHeader, PDU pdu) {
  // TODO(quiche): Implement validation and handling of Start-of-SDU frames.
  return nullptr;
}

ByteBufferPtr Engine::ProcessFrame(const SimpleSupervisoryFrame sframe, PDU pdu) {
  // Core Spec v5, Vol 3, Part A, Sec 8.6.1.5: "S-Frames shall not be transmitted with both the
  // F-bit and the P-bit set to 1 at the same time."
  if (sframe.is_poll_request() && sframe.is_poll_response()) {
    connection_failure_callback_();
    return nullptr;
  }

  // Signal changes to our RemoteBusy variable per Core Spec v5.0, Vol 3, Part A, Sec 8.6.5.6.
  const bool remote_is_busy = sframe.function() == SupervisoryFunction::ReceiverNotReady;
  if (remote_is_busy && !remote_is_busy_) {
    if (remote_busy_set_callback_) {
      remote_busy_set_callback_();
    }
  } else if (!remote_is_busy && remote_is_busy_) {
    if (remote_busy_cleared_callback_) {
      remote_busy_cleared_callback_();
    }
  }
  remote_is_busy_ = remote_is_busy;

  // Implements the "Send RRorRNR (F=1)" action of Core Spec, v5, Vol 3, Part A, Section 8.6.5.9,
  // Table 8.6, "Recv RNR (P=1)" and "Send IorRRorRNR(F=1)" action of "Recv RR(P=1)." In the latter
  // case, responding with an I-Frame (F=1) is indistinguishable from responding with an RR (F=1)
  // then an I-Frame (F=0), so that optimization isn't implemented and we always respond with an RR
  // or RNR (F=1), but not an I-Frame (F=1).
  if (sframe.function() == SupervisoryFunction::ReceiverReady ||
      sframe.function() == SupervisoryFunction::ReceiverNotReady) {
    if (sframe.is_poll_request()) {
      // See Core Spec, v5, Vol 3, Part A, Section 8.6.5.9, Table 8.6, "Recv RR(P=1)".
      //
      // Note, however, that there may be additional work to do if we're in the
      // REJ_SENT state. See Core Spec, v5, Vol 3, Part A, Section 8.6.5.10, Table 8.7, "Recv
      // RR(P=1)".
      //
      // TODO(https://fxbug.dev/1039): Respond with RNR when LocalBusy.
      SimpleReceiverReadyFrame poll_response;
      poll_response.set_is_poll_response();
      poll_response.set_receive_seq_num(next_seqnum_);
      send_frame_callback_(
          std::make_unique<DynamicByteBuffer>(BufferView(&poll_response, sizeof(poll_response))));
      return nullptr;
    }
  }

  // REJ S-Frames will still result in forwarding the acknowledgment via ReceiveSeqNumCallback after
  // this call, per "PassToTx" actions for "Recv REJ" events in Core Spec v5.0, Vol 3, Part A,
  // Sec 8.6.5.9–11.
  if (sframe.function() == SupervisoryFunction::Reject) {
    if (range_retransmit_set_callback_) {
      range_retransmit_set_callback_(sframe.is_poll_request());
    }
  }

  // SREJ S-Frames will still result in forwarding the acknowledgment via ReceiveSeqNumCallback
  // after this call. The "Recv SREJ" events in Core Spec v5.0, Vol 3, Part A, Sec 8.6.5.9–11 call
  // for different actions ("PassToTx" vs "PassToTxFbit") but we always pass both receive seq and
  // poll response because the TxEngine has other behavior that branch on the same bit.
  if (sframe.function() == SupervisoryFunction::SelectiveReject) {
    if (single_retransmit_set_callback_) {
      single_retransmit_set_callback_(sframe.is_poll_request());
    }
  }

  return nullptr;
}

ByteBufferPtr Engine::ProcessFrame(std::monostate, PDU pdu) {
  // TODO(quiche): Close connection.
  return nullptr;
}

void Engine::AdvanceSeqNum() {
  ++next_seqnum_;
  if (next_seqnum_ > EnhancedControlField::kMaxSeqNum) {
    next_seqnum_ = 0;
  }
}

}  // namespace bt::l2cap::internal
