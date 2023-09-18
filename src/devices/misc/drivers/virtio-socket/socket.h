// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_MISC_DRIVERS_VIRTIO_SOCKET_SOCKET_H_
#define SRC_DEVICES_MISC_DRIVERS_VIRTIO_SOCKET_SOCKET_H_

#include <fidl/fuchsia.hardware.vsock/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/wait.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/io-buffer.h>
#include <lib/fit/function.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/channel.h>
#include <lib/zx/socket.h>
#include <lib/zx/timer.h>

#include <cstddef>
#include <memory>
#include <utility>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/array.h>
#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/intrusive_hash_table.h>
#include <fbl/ref_counted.h>
#include <virtio/vsock.h>

namespace virtio {

namespace vsock = fuchsia_hardware_vsock;

class SocketDevice;
using DeviceType =
    ddk::Device<SocketDevice, ddk::Unbindable, ddk::Messageable<vsock::Device>::Mixin>;

class SocketDevice : public Device,
                     public DeviceType,
                     public ddk::EmptyProtocol<ZX_PROTOCOL_VSOCK> {
 public:
  struct ConnectionKey;

  explicit SocketDevice(zx_device_t* device, zx::bti, std::unique_ptr<Backend> backend);
  ~SocketDevice() override;

  // DDKTL hooks:
  void DdkRelease();
  void DdkUnbind(ddk::UnbindTxn txn) { txn.Reply(); }

  // fuchsia.hardware.vsock.Device implementation.
  void Start(::fuchsia_hardware_vsock::wire::DeviceStartRequest* request,
             StartCompleter::Sync& completer) override;
  void SendRequest(::fuchsia_hardware_vsock::wire::DeviceSendRequestRequest* request,
                   SendRequestCompleter::Sync& completer) override;
  void SendShutdown(::fuchsia_hardware_vsock::wire::DeviceSendShutdownRequest* request,
                    SendShutdownCompleter::Sync& completer) override;
  void SendRst(::fuchsia_hardware_vsock::wire::DeviceSendRstRequest* request,
               SendRstCompleter::Sync& completer) override;
  void SendResponse(::fuchsia_hardware_vsock::wire::DeviceSendResponseRequest* request,
                    SendResponseCompleter::Sync& completer) override;
  void GetCid(GetCidCompleter::Sync& completer) override;
  void SendVmo(::fuchsia_hardware_vsock::wire::DeviceSendVmoRequest* request,
               SendVmoCompleter::Sync& completer) override;

  zx_status_t Init() override;

  // VirtIO callbacks
  void IrqRingUpdate() override;
  void IrqConfigChange() override;
  const char* tag() const override { return "virtio-vsock"; }

  // ConnectionKey is mostly a wrapper around vsock::wire::Addr that provides
  // an equality operation for use as the Key in a HashMap.
  struct ConnectionKey {
    explicit ConnectionKey(const vsock::wire::Addr& addr) : addr(addr) {}
    ConnectionKey(uint32_t local_port, uint32_t remote_cid, uint32_t remote_port)
        : addr({.local_port = local_port, .remote_cid = remote_cid, .remote_port = remote_port}) {}
    static ConnectionKey FromHdr(virtio_vsock_hdr_t* hdr) {
      return ConnectionKey(hdr->dst_port, static_cast<uint32_t>(hdr->src_cid), hdr->src_port);
    }
    vsock::wire::Addr addr;
    bool operator==(const ConnectionKey& key) const {
      return addr.local_port == key.addr.local_port && addr.remote_cid == key.addr.remote_cid &&
             addr.remote_port == key.addr.remote_port;
    }
  };

  struct CreditInfo {
    // The size of the buffer in bytes.
    uint32_t buf_alloc = 0;

    // The number of bytes that have been received.
    uint32_t fwd_count = 0;
  };

  // Wrapper around a virtio Ring that uses a single contiguous io_buffer to
  // fill the descriptors.
  class IoBufferRing {
   public:
    IoBufferRing(virtio::Device* device, uint16_t count, uint32_t buf_size, bool host_write_only);
    virtual ~IoBufferRing();
    // Initialize the Ring and allocate the io_buffer. index is the virtio ring index
    // in the device. This must be called prior to using any other members of
    // this class.
    zx_status_t Init(uint16_t index, const zx::bti& bti);
    // Frees the resources allocated by this ring.
    void FreeBuffers();

    inline void Kick() {
      assert(io_buffer_is_valid(&io_buffer_));
      ring_.Kick();
    }

   protected:
    void* GetRawDesc(uint16_t id, uint32_t len, uint32_t offset = 0) {
      assert(len + offset <= buf_size_);
      assert(io_buffer_is_valid(&io_buffer_));
      uintptr_t base = reinterpret_cast<uintptr_t>(io_buffer_virt(&io_buffer_));
      return reinterpret_cast<void*>(base + static_cast<uintptr_t>(id) * buf_size_ + offset);
    }
    Ring& ring() { return ring_; }
    io_buffer_t& io_buffer() { return io_buffer_; }
    uint32_t buf_size() const { return buf_size_; }

   private:
    Ring ring_;
    bool host_write_only_;
    io_buffer_t io_buffer_;
    uint16_t count_;
    uint32_t buf_size_;
  };

  class RxIoBufferRing : public IoBufferRing {
   public:
    RxIoBufferRing(virtio::Device* device, uint16_t count, uint32_t buf_size);

    // Submit descriptors into the ring. Typically only needs to be called on
    // init, as ProcessDescriptors will automatically call this.
    void RefillRing();

    // Calls the provided function on any completed descriptors giving header
    // and any extra data. Drops any descriptors that are chained.
    // Will automatically refill and kick the ring.
    template <typename H, typename F>
    void ProcessDescriptors(F func);
  };

  class TxIoBufferRing : public IoBufferRing {
   public:
    TxIoBufferRing(virtio::Device* device, uint16_t count, uint32_t buf_size);
    // Allocates a descriptor returning a pointer to the location to fill with
    // data.
    void* AllocInPlace(uint16_t* id);

    // Allocate a descriptor chain for sending an indirect payload. The
    // chain should have SetIndirectPayload and SetHeader called on it
    // prior to being given to SubmitChain.
    bool AllocIndirect(const ConnectionKey& key, uint16_t* id);

    // Attaches the indirect payload to the descriptor chain allocated by
    // AllocIndirect.
    void SetIndirectPayload(uint16_t id, uintptr_t payload);

    void SetHeader(uint16_t id, const virtio_vsock_hdr_t& hdr) {
      *reinterpret_cast<virtio_vsock_hdr_t*>(GetRawDesc(id, sizeof(virtio_vsock_hdr_t))) = hdr;
    }

    // Submit a chain for TX. Does not kick the ring.
    void SubmitChain(uint16_t id, uint32_t data_len);

    void FreeChain(uint16_t id);

    // Processes the completed tx descriptors and calls the provided function
    // with the key and indirect payload for any indirect descriptors.
    template <typename F>
    void ProcessDescriptors(F func);
  };

  // A Connection may get placed in three different containers. A hash table
  // (connections_), which requires the SinglyLinkedListable storage, a double
  // linked list (has_pending_tx_), and a second double linked list (has_pending_op_).
  struct ConnectionHashTag {};
  struct PendingTxTag {};
  struct PendingOpTag {};
  class Connection
      : public fbl::RefCounted<Connection>,
        public fbl::ContainableBaseClasses<
            fbl::TaggedSinglyLinkedListable<fbl::RefPtr<Connection>, ConnectionHashTag>,
            fbl::TaggedDoublyLinkedListable<fbl::RefPtr<Connection>, PendingTxTag>,
            fbl::TaggedDoublyLinkedListable<fbl::RefPtr<Connection>, PendingOpTag>> {
   private:
    // A connection moves through different states over its lifetime. These
    // states have a very simple transition system in that they can only
    // go 'forward', hence we consider a connection progressing through
    // a lifetime at different rates. A connection can jump into existence
    // at either WAIT_RESPONSE or ACTIVE, and can be deleted from
    // any state. SHUTTING_DOWN is an optional 'grace' state and
    // ZOMBIE is only needed in cases where the connection cannot be
    // immediately deleted.
    enum State {
      // A connection is attempting to be established and is waiting for
      // a response from a remote.
      WAIT_RESPONSE,
      // The 'normal' state of a connection. It can TX/RX, has valid
      // credit.
      ACTIVE,
      // If a graceful shutdown is requested, but there is still pending TX
      // data, then this state indicates that no more TX data should be
      // accepted, but we have not yet told the remote we are shutting down.
      WILL_SHUT_DOWN,
      // Trying to perform a graceful shutdown. Any pending TX will happen
      // and RX will still be passed on, but further TX is denied.
      SHUTTING_DOWN,
      // Connection is considered terminated but resource cleanup is still
      // happening due to race conditions with dispatchers.
      ZOMBIE,
    };

   public:
    using SignalHandler =
        fit::function<void(zx_status_t, const zx_packet_signal_t*, fbl::RefPtr<Connection>)>;
    Connection(const ConnectionKey& key, zx::socket data, SignalHandler wait_handler, uint32_t cid,
               fbl::Mutex& lock);
    ~Connection() = default;

    bool PendingTx();

    // Whether or not the connection is in the process of closing.
    bool IsShuttingDown();

    // Tell the connection to begin a client requested graceful shutdown.
    // This means we will drain any pending TX before completing the shutdown.
    bool BeginShutdown();

    // Informs the connection that a TX has completed that was sending the
    // specified |paddr|. This returns 'true' to indicate the current VMO has
    // completed sending.
    bool NotifyVmoTxComplete(uintptr_t paddr);

    void UpdateCredit(uint32_t buf, uint32_t fwd);

    // Marks a connection as active and able to send/receive data. Is ignored
    // if the connection is shutting down.
    void MakeActive(async_dispatcher_t* disp);

    // Receive data on the connection. Returns false if there is a client error
    // and the connection should be RST.
    bool Rx(void* data, size_t len);

    // Returns the credit information for this connection.
    CreditInfo GetCreditInfo();

    // Helper for making a header that is filled out with our connection key
    // and credit information.
    virtio_vsock_hdr_t MakeHdr(uint16_t op);

    // Close a connection indicating no more data shall be sent and received and
    // it should enter the zombie state until it gets fully deleted.
    void Close(async_dispatcher_t* dispatcher);

    // Performs any outstanding TX for this connection by filling the providing
    // ring with descriptors. This may generate credit requests and HasPendingOp
    // can be checked afterwards to see if this is the case. This returns
    // ZX_OK if there is no more pending tx, ZX_ERR_SHOULD_WAIT if there is
    // still data to send and we should retry when there is either more credit
    // or more TX descriptors. Any other return status indicates an underlying
    // transport error and the connection should be closed.
    zx_status_t ContinueTx(bool force_credit_request, TxIoBufferRing& tx,
                           async_dispatcher_t* dispatcher);

    zx_status_t SetVmo(zx::bti& bti, zx::vmo vmo, uint64_t offset, uint64_t len,
                       uint64_t bti_contiguity);

    void QueueOp(uint16_t new_op);
    bool HasPendingOp();
    uint16_t TakePendingOp();

    static size_t GetHash(const ConnectionKey& addr);
    const ConnectionKey& GetKey() const;

   private:
    // Helper class for walking the physical addresses of a VMO.
    class VmoWalker {
     public:
      VmoWalker() = default;
      ~VmoWalker() { Release(); }
      zx_status_t Set(zx::bti& bti, zx::vmo vmo, uint64_t offset, uint64_t len,
                      uint64_t bti_contiguity);
      void Release();
      uint64_t NextChunkLen(uint64_t max);
      zx_paddr_t Consume(uint64_t len);
      zx::pmt pinned_pages_;
      zx::vmo vmo_;
      uint64_t contiguity_;
      uint64_t base_addr_;
      fbl::Array<zx_paddr_t> paddrs_;

      uint64_t transfer_offset_;
      uint64_t transfer_length_;

      zx_paddr_t final_paddr_;
    };

    void CountTx(uint32_t len);
    bool SocketTxPending();
    bool DoVmoTx(bool force_credit_request, TxIoBufferRing& tx);
    zx_status_t DoSocketTx(bool force_credit_request, TxIoBufferRing& tx,
                           async_dispatcher_t* dispatcher);
    void BeginWait(async_dispatcher_t* disp);
    uint32_t GetPeerFree(bool request_credit);

    // Reference to the lock that we will hold when performing BeginWait.
    fbl::Mutex& lock_;

    ConnectionKey key_;

    State state_;
    // Free running tx counter
    uint32_t tx_count_;
    // Free running rx counter
    uint32_t rx_count_;
    // Last known peer buffer information.
    uint32_t buf_alloc_;
    uint32_t fwd_cnt_;
    // Socket for TX/RX to the application
    zx::socket data_;

    async::Wait wait_handler_;
    // To resolve races with the dispatcher whenever wait_handler_ has a valid
    // handler this ref ptr must be valid. This ref ptr can be cleared only
    // when the wait is successfully canceled, or by the handler itself once
    // it completes.
    fbl::RefPtr<Connection> wait_handler_ref_;
    // Are we trying to send a vmo?
    bool pending_vmo_;
    VmoWalker vmo_;

    bool has_pending_op_;
    uint16_t pending_op_;
    uint32_t cid_;

    DISALLOW_COPY_ASSIGN_AND_MOVE(Connection);
  };

 private:
  using ConnectionHashTable =
      fbl::TaggedHashTable<ConnectionKey, fbl::RefPtr<Connection>, ConnectionHashTag>;
  using ConnectionIterator = ConnectionHashTable::iterator;
  friend Connection;

  void ProcessRxDescriptor(virtio_vsock_hdr_t* header, void* data, uint32_t data_len) TA_REQ(lock_);
  void UpdateRxRingLocked() TA_REQ(lock_);
  void RxOpLocked(ConnectionIterator conn, const ConnectionKey& key, uint16_t op) TA_REQ(lock_);

  bool SendOp_RawLocked(const ConnectionKey& key, uint16_t op, const CreditInfo& credit)
      TA_REQ(lock_);
  void SendOpLocked(fbl::RefPtr<Connection> conn, uint16_t op) TA_REQ(lock_);

  void RetryTxLocked(bool ask_for_credit) TA_REQ(lock_);
  void ContinueTxLocked(bool force_credit_request, fbl::RefPtr<Connection> conn) TA_REQ(lock_);

  void SendRstLocked(const ConnectionKey& key) TA_REQ(lock_);
  void CleanupConLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);
  void NotifyAndCleanupConLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);

  // Forcibly cleans up any outstanding connection and sends an RST to the host
  // This does not notify the callbacks and is for use when there are no callbacks
  // or the service requested the rst
  void CleanupConAndRstLocked(const ConnectionKey& key) TA_REQ(lock_);

  void RemoveCallbacksLocked() TA_REQ(lock_);

  bool QueuedForTxLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);
  void QueueForTxLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);
  void DequeueTxLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);

  bool QueuedForOpLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);
  void QueueForOpLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);
  void DequeueOpLocked(fbl::RefPtr<Connection> conn) TA_REQ(lock_);

  void EnableTxRetryTimerLocked() TA_REQ(lock_);
  void TimerWaitHandler(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                        const zx_packet_signal_t* signal);
  void CallbacksSignalled(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                          const zx_packet_signal_t* signal);
  void ConnectionSocketSignalled(zx_status_t status, const zx_packet_signal_t* signal,
                                 fbl::RefPtr<Connection> conn);

  void UpdateCidLocked() TA_REQ(lock_);
  void ReleaseLocked() TA_REQ(lock_);
  void TransportResetLocked() TA_REQ(lock_);

  DISALLOW_COPY_ASSIGN_AND_MOVE(SocketDevice);
  uint32_t cid_ TA_GUARDED(lock_);
  async_dispatcher_t* dispatcher_ TA_GUARDED(lock_) = nullptr;
  RxIoBufferRing rx_ TA_GUARDED(lock_);
  TxIoBufferRing tx_ TA_GUARDED(lock_);
  RxIoBufferRing event_ TA_GUARDED(lock_);
  fidl::WireSyncClient<vsock::Callbacks> callbacks_ TA_GUARDED(lock_);

  // List of connections that have pending TX but are waiting for either more
  // credit from the remote, or more TX descriptors.
  fbl::TaggedDoublyLinkedList<fbl::RefPtr<Connection>, PendingTxTag> has_pending_tx_
      TA_GUARDED(lock_);
  // List of connections that still need to send an op.
  fbl::TaggedDoublyLinkedList<fbl::RefPtr<Connection>, PendingOpTag> has_pending_op_
      TA_GUARDED(lock_);

  ConnectionHashTable connections_ TA_GUARDED(lock_);

  zx::timer tx_retry_timer_ TA_GUARDED(lock_);
  async::WaitMethod<SocketDevice, &SocketDevice::TimerWaitHandler> timer_wait_handler_
      TA_GUARDED(lock_);
  async::WaitMethod<SocketDevice, &SocketDevice::CallbacksSignalled> callback_closed_handler_
      TA_GUARDED(lock_);
  uint64_t bti_contiguity_ TA_GUARDED(lock_);
};

}  // namespace virtio

#endif  // SRC_DEVICES_MISC_DRIVERS_VIRTIO_SOCKET_SOCKET_H_
