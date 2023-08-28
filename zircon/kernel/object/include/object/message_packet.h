// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_MESSAGE_PACKET_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_MESSAGE_PACKET_H_

#include <lib/user_copy/user_ptr.h>
#include <stdint.h>
#include <zircon/types.h>

#include <cstdint>

#include <fbl/intrusive_double_list.h>
#include <fbl/intrusive_single_list.h>
#include <ktl/unique_ptr.h>
#include <object/buffer_chain.h>
#include <object/handle.h>

constexpr uint32_t kMaxMessageSize = 65536u;
constexpr uint32_t kMaxMessageHandles = 64u;
constexpr uint32_t kMaxIovecsCount = 8192u;

// ensure public constants are aligned
static_assert(ZX_CHANNEL_MAX_MSG_BYTES == kMaxMessageSize, "");
static_assert(ZX_CHANNEL_MAX_MSG_HANDLES == kMaxMessageHandles, "");
static_assert(ZX_CHANNEL_MAX_MSG_IOVECS == kMaxIovecsCount, "");

class Handle;
class MessagePacket;
namespace internal {
struct MessagePacketDeleter;
}  // namespace internal

// Definition of a MessagePacket's specific pointer type.  Message packets must
// be managed using this specific type of pointer, because MessagePackets have a
// specific custom deletion requirement.
using MessagePacketPtr = ktl::unique_ptr<MessagePacket, internal::MessagePacketDeleter>;

class MessagePacket final : public fbl::DoublyLinkedListable<MessagePacketPtr> {
 public:
  // The number of iovecs to read and process at a time.
  // If number of iovecs <= kIovecChunkSize, then the message buf size will be computed and the
  // minimum required number of pages will be allocated. Otherwise, a large enough buffer for
  // the largest possible message will be allocated.
  static constexpr uint32_t kIovecChunkSize = 16;

  // Creates a message packet containing the provided data and space for
  // |num_handles| handles. The handles array is uninitialized and must
  // be completely overwritten by clients.
  static zx_status_t Create(user_in_ptr<const char> data, uint32_t data_size, uint32_t num_handles,
                            MessagePacketPtr* msg);
  static zx_status_t Create(user_in_ptr<const zx_channel_iovec_t> iovecs, uint32_t num_iovecs,
                            uint32_t num_handles, MessagePacketPtr* msg);
  static zx_status_t Create(const char* data, uint32_t data_size, uint32_t num_handles,
                            MessagePacketPtr* msg);

  uint32_t data_size() const { return data_size_; }

  // Copies the packet's |data_size()| bytes to |buf|.
  // Returns an error if |buf| points to a bad user address.
  zx_status_t CopyDataTo(user_out_ptr<char> buf) const {
    return buffer_chain_->CopyOut(buf, payload_offset_, data_size_);
  }

  uint32_t num_handles() const { return num_handles_; }
  Handle* const* handles() const { return handles_; }
  Handle** mutable_handles() { return handles_; }

  void set_owns_handles(bool own_handles) { owns_handles_ = own_handles; }

  // zx_channel_call treats the leading bytes of the payload as
  // a transaction id of type zx_txid_t.
  zx_txid_t get_txid() const {
    if (data_size_ < sizeof(zx_txid_t)) {
      return 0;
    }
    // The first few bytes of the payload are a zx_txid_t.
    return *static_cast<const zx_txid_t*>(payload());
  }

  void set_txid(zx_txid_t txid) {
    if (data_size_ >= sizeof(zx_txid_t)) {
      *(static_cast<zx_txid_t*>(payload())) = txid;
    }
  }

  struct FidlHeader {
    zx_txid_t txid{};
    uint8_t flags[3]{0, 0, 0};
    uint8_t magic{0};
    uint64_t ordinal{0};
  };
  static_assert(sizeof(FidlHeader) == 2 * sizeof(uint64_t));

  FidlHeader fidl_header() const {
    if (data_size_ >= sizeof(FidlHeader)) {
      return *static_cast<const FidlHeader*>(payload());
    }
    return FidlHeader{};
  }

 private:
  // A private constructor ensures that users must use the static factory
  // Create method to create a MessagePacket.  This, in turn, guarantees that
  // when a user creates a MessagePacket, they end up with the proper
  // MessagePacket::UPtr type for managing the message packet's life cycle.
  MessagePacket(BufferChain* chain, uint32_t data_size, uint32_t payload_offset,
                uint16_t num_handles, Handle** handles)
      : buffer_chain_(chain),
        handles_(handles),
        data_size_(data_size),
        payload_offset_(payload_offset),
        num_handles_(num_handles),
        owns_handles_(false) {}

  // A private destructor helps to make sure that only our custom deleter is
  // ever used to destroy this object which, in turn, makes it very difficult
  // to not properly recycle the object.
  ~MessagePacket() {
    DEBUG_ASSERT(!InContainer());
    if (owns_handles_) {
      for (size_t ix = 0; ix != num_handles_; ++ix) {
        // Delete the handle via HandleOwner dtor.
        HandleOwner ho(handles_[ix]);
      }
    }
  }

  friend struct internal::MessagePacketDeleter;
  static void recycle(MessagePacket* packet);

  static zx_status_t CreateIovecBounded(user_in_ptr<const zx_channel_iovec_t> iovecs,
                                        uint32_t num_iovecs, uint32_t num_handles,
                                        MessagePacketPtr* msg);
  static zx_status_t CreateIovecUnbounded(user_in_ptr<const zx_channel_iovec_t> iovecs,
                                          uint32_t num_iovecs, uint32_t num_handles,
                                          MessagePacketPtr* msg);
  static zx_status_t CreateCommon(size_t data_size, size_t num_handles, MessagePacketPtr* msg);

  void set_data_size(uint32_t data_size) { data_size_ = data_size; }

  const void* payload() const { return buffer_chain_->buffers()->front().data() + payload_offset_; }
  void* payload() { return buffer_chain_->buffers()->front().data() + payload_offset_; }

  BufferChain* buffer_chain_;
  Handle** const handles_;
  uint32_t data_size_;
  const uint32_t payload_offset_;
  const uint16_t num_handles_;
  bool owns_handles_;
};

namespace internal {
struct MessagePacketDeleter {
  void operator()(MessagePacket* packet) const noexcept { MessagePacket::recycle(packet); }
};
}  // namespace internal

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_MESSAGE_PACKET_H_
