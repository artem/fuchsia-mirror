// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_L2CAP_ENHANCED_RETRANSMISSION_MODE_RX_ENGINE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_L2CAP_ENHANCED_RETRANSMISSION_MODE_RX_ENGINE_H_

#include <variant>

#include "src/connectivity/bluetooth/core/bt-host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/l2cap/frame_headers.h"
#include "src/connectivity/bluetooth/core/bt-host/l2cap/rx_engine.h"

namespace bt::l2cap::internal {

// Implements the receiver state and logic for an L2CAP channel operating in
// Enhanced Retransmission Mode.
//
// THREAD-SAFETY: This class is not thread-safe.
class EnhancedRetransmissionModeRxEngine final : public RxEngine {
 public:
  using SendFrameCallback = fit::function<void(ByteBufferPtr pdu)>;
  using ConnectionFailureCallback = fit::function<void()>;

  EnhancedRetransmissionModeRxEngine(SendFrameCallback send_frame_callback,
                                     ConnectionFailureCallback connection_failure_callback);
  ~EnhancedRetransmissionModeRxEngine() override = default;

  ByteBufferPtr ProcessPdu(PDU) override;

  // Set a callback to be invoked when any frame is received that indicates the peer's
  // acknowledgment for the sequence of packets that it received from the local host. The values are
  // not checked against the local sender's TxWindow. |is_poll_response| reflects the 'F' bit in the
  // header of the received frame.
  using ReceiveSeqNumCallback = fit::function<void(uint8_t receive_seq_num, bool is_poll_response)>;
  void set_receive_seq_num_callback(ReceiveSeqNumCallback receive_seq_num_callback) {
    receive_seq_num_callback_ = std::move(receive_seq_num_callback);
  }

  // Set a callback to be invoked that reports our acknowledgment of inbound frames from the peer.
  // |ack_seq_num| is the TxSeq of the next I-frame we expect from the peer.
  using AckSeqNumCallback = fit::function<void(uint8_t ack_seq_num)>;
  void set_ack_seq_num_callback(AckSeqNumCallback ack_seq_num_callback) {
    ack_seq_num_callback_ = std::move(ack_seq_num_callback);
  }

  // Set callbacks to be invoked when the RemoteBusy state variable (Core Spec v5.0, Vol 3, Part A,
  // Section 8.6.5.3) changes to indicate whether the peer can receive additional I-Frames.
  using RemoteBusyChangedCallback = fit::closure;
  void set_remote_busy_set_callback(RemoteBusyChangedCallback remote_busy_set_callback) {
    remote_busy_set_callback_ = std::move(remote_busy_set_callback);
  }

  void set_remote_busy_cleared_callback(RemoteBusyChangedCallback remote_busy_cleared_callback) {
    remote_busy_cleared_callback_ = std::move(remote_busy_cleared_callback);
  }

  // Set a callback to be invoked when a Reject function (Core Spec v5.0, Vol 3, Part A, Sec
  // 8.6.1.2) is received. This invocation precedes the ReceiveSeqNumCallback invocation, which
  // delivers the SeqNum that the TxEngine is expected to retransmit first.
  //
  // |is_poll_request| reflects the 'P' bit in the header of the received frame.
  using RangeRetransmitSetCallback = fit::function<void(bool is_poll_request)>;
  void set_range_retransmit_set_callback(RangeRetransmitSetCallback range_retransmit_set_callback) {
    range_retransmit_set_callback_ = std::move(range_retransmit_set_callback);
  }

  // Set a callback to be invoked when a Selective Reject function (Core Spec v5.0, Vol 3, Part A,
  // Sec 8.6.1.4) is received. This invocation precedes the ReceiveSeqNumCallback invocation, which
  // delivers the SeqNum of the I-Frame that the TxEngine is expected to retransmit.
  //
  // |is_poll_request| reflects the 'P' bit in the header of the received frame.
  using SingleRetransmitSetCallback = fit::function<void(bool is_poll_request)>;
  void set_single_retransmit_set_callback(
      SingleRetransmitSetCallback single_retransmit_set_callback) {
    single_retransmit_set_callback_ = std::move(single_retransmit_set_callback);
  }

 private:
  ByteBufferPtr ProcessFrame(const SimpleInformationFrameHeader, PDU);
  ByteBufferPtr ProcessFrame(const SimpleStartOfSduFrameHeader, PDU);
  ByteBufferPtr ProcessFrame(const SimpleSupervisoryFrame, PDU);
  ByteBufferPtr ProcessFrame(std::monostate, PDU);
  void AdvanceSeqNum();

  // We assume that the Extended Window Size option is _not_ enabled. In such
  // cases, the sequence number is a 6-bit counter that wraps on overflow. See
  // Core Spec Ver 5, Vol 3, Part A, Secs 5.7 and 8.3.
  uint8_t next_seqnum_;  // (AKA Expected-TxSeq)

  // Represents the RemoteBusy state variable (Core Spec v5.0, Vol 3, Part A, Section 8.6.5.3) for
  // whether the peer has sent a Receiver Not Ready.
  bool remote_is_busy_;

  SendFrameCallback send_frame_callback_;

  // Invoked when the connection encounters a fatal error.
  const ConnectionFailureCallback connection_failure_callback_;

  // TODO(https://fxbug.dev/52554): Refactor these delegates into a single interface for TxEngine to
  // implement.
  ReceiveSeqNumCallback receive_seq_num_callback_;
  AckSeqNumCallback ack_seq_num_callback_;
  RemoteBusyChangedCallback remote_busy_set_callback_;
  RemoteBusyChangedCallback remote_busy_cleared_callback_;
  RangeRetransmitSetCallback range_retransmit_set_callback_;
  SingleRetransmitSetCallback single_retransmit_set_callback_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(EnhancedRetransmissionModeRxEngine);
};

}  // namespace bt::l2cap::internal

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_L2CAP_ENHANCED_RETRANSMISSION_MODE_RX_ENGINE_H_
