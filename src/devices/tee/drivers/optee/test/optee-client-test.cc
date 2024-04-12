// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/tee/drivers/optee/optee-client.h"

#include <endian.h>
#include <fidl/fuchsia.hardware.rpmb/cpp/wire.h>
#include <fidl/fuchsia.tee.manager/cpp/wire.h>
#include <fidl/fuchsia.tee/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fake-bti/bti.h>
#include <lib/fake-resource/resource.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/sync/completion.h>
#include <lib/zx/bti.h>
#include <stdlib.h>
#include <zircon/types.h>

#include <memory>
#include <set>

#include <ddktl/suspend-txn.h>
#include <tee-client-api/tee-client-types.h>
#include <zxtest/zxtest.h>

#include "optee-message.h"
#include "src/devices/tee/drivers/optee/optee-controller.h"
#include "src/devices/tee/drivers/optee/optee-rpmb.h"
#include "src/devices/tee/drivers/optee/optee-smc.h"
#include "src/devices/tee/drivers/optee/tee-smc.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace optee {
namespace {

namespace frpmb = fuchsia_hardware_rpmb;

constexpr fuchsia_tee::wire::Uuid kOpteeOsUuid = {
    0x486178E0, 0xE7F8, 0x11E3, {0xBC, 0x5E, 0x00, 0x02, 0xA5, 0xD5, 0xC5, 0x1B}};

class OpteeClientTestBase : public OpteeControllerBase, public zxtest::Test {
 public:
  static const size_t kMaxParamCount = 4;

  struct MessageRaw {
    MessageHeader hdr;
    MessageParam params[kMaxParamCount];
  };

  OpteeClientTestBase() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    ASSERT_OK(loop_.StartThread("thread-id-1"));
    ASSERT_OK(loop_.StartThread("thread-id-2"));

    // Allocate memory for shared memory buffer
    constexpr size_t kSharedMemorySize = 0x20000;

    fake_bti_create(fake_bti_.reset_and_get_address());

    zx::vmo fake_vmo;
    size_t size = 0x20000;
    EXPECT_OK(zx::vmo::create_contiguous(fake_bti_, size, 0, &fake_vmo));

    EXPECT_OK(fake_bti_.pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, fake_vmo, 0, kSharedMemorySize,
                            &shared_memory_paddr_, 1, &pmt_));

    zx::result<fdf::MmioBuffer> mmio =
        fdf::MmioBuffer::Create(0, size, std::move(fake_vmo), ZX_CACHE_POLICY_CACHED);
    ASSERT_OK(mmio.status_value());

    shared_memory_vaddr_ = reinterpret_cast<zx_vaddr_t>(mmio.value().get());
    EXPECT_OK(SharedMemoryManager::Create(std::move(mmio.value()), shared_memory_paddr_,
                                          &shared_memory_manager_));
  }

  SharedMemoryManager::DriverMemoryPool *driver_pool() const override {
    return shared_memory_manager_->driver_pool();
  }

  SharedMemoryManager::ClientMemoryPool *client_pool() const override {
    return shared_memory_manager_->client_pool();
  }

  zx::result<fidl::ClientEnd<frpmb::Rpmb>> RpmbConnectServer() const override {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }

  zx_device_t *GetDevice() const override { return parent_.get(); }

  void SetUp() override {}

  void TearDown() override {}

 protected:
  void AllocMemory(size_t size, uint64_t *paddr, uint64_t *mem_id, RpcHandler &rpc_handler) {
    RpcFunctionArgs args;
    RpcFunctionResult result;
    args.generic.status = kReturnRpcPrefix | kRpcFunctionIdAllocateMemory;
    args.allocate_memory.size = size;

    EXPECT_OK(rpc_handler(args, &result));

    *paddr = result.allocate_memory.phys_addr_upper32;
    *paddr = (*paddr << 32) | result.allocate_memory.phys_addr_lower32;
    *mem_id = result.allocate_memory.mem_id_upper32;
    *mem_id = (*mem_id << 32) | result.allocate_memory.mem_id_lower32;
    EXPECT_TRUE(*paddr > shared_memory_paddr_);
  }

  void FreeMemory(uint64_t &mem_id, RpcHandler &rpc_handler) {
    RpcFunctionArgs args;
    RpcFunctionResult result;
    args.generic.status = kReturnRpcPrefix | kRpcFunctionIdFreeMemory;
    args.free_memory.mem_id_upper32 = mem_id >> 32;
    args.free_memory.mem_id_lower32 = mem_id & 0xFFFFFFFF;

    EXPECT_OK(rpc_handler(args, &result));
  }

  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();

  std::unique_ptr<SharedMemoryManager> shared_memory_manager_;

  zx::bti fake_bti_;
  zx::pmt pmt_;
  zx_paddr_t shared_memory_paddr_;
  zx_vaddr_t shared_memory_vaddr_;
  async::Loop loop_;
};

class OpteeClientTest : public OpteeClientTestBase {
 public:
  OpteeClientTest() {}

  CallResult CallWithMessage(const optee::Message &message, RpcHandler rpc_handler) override {
    size_t offset = message.paddr() - shared_memory_paddr_;

    MessageHeader *hdr = reinterpret_cast<MessageHeader *>(shared_memory_vaddr_ + offset);
    hdr->return_origin = TEEC_ORIGIN_TEE;
    hdr->return_code = TEEC_SUCCESS;

    switch (hdr->command) {
      case Message::Command::kOpenSession: {
        hdr->session_id = next_session_id_++;
        open_sessions_.insert(hdr->session_id);
        break;
      }
      case Message::Command::kCloseSession: {
        EXPECT_EQ(open_sessions_.erase(hdr->session_id), 1u);
        break;
      }
      default:
        hdr->return_code = TEEC_ERROR_NOT_IMPLEMENTED;
    }

    return CallResult{.return_code = kReturnOk};
  }

  const std::set<uint32_t> &open_sessions() const { return open_sessions_; }

 private:
  uint32_t next_session_id_ = 1;
  std::set<uint32_t> open_sessions_;
};

TEST_F(OpteeClientTest, OpenSessionsClosedOnClientUnbind) {
  auto [client_end, server_end] = fidl::Endpoints<fuchsia_tee::Application>::Create();
  auto optee_client = std::make_unique<OpteeClient>(
      this, fidl::ClientEnd<fuchsia_tee_manager::Provider>(), optee::Uuid{kOpteeOsUuid});

  sync_completion_t unbound = {};

  fidl::BindServer(
      loop_.dispatcher(), std::move(server_end), optee_client.get(),
      [&unbound](OpteeClient *, fidl::UnbindInfo, fidl::ServerEnd<fuchsia_tee::Application>) {
        sync_completion_signal(&unbound);
      });

  {
    fidl::WireSyncClient<fuchsia_tee::Application> fidl_client(std::move(client_end));
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client->OpenSession2(std::move(parameter_set));
    EXPECT_OK(res.status());
    ASSERT_FALSE(open_sessions().empty());
  }  // ~WireSyncClient will close the channel, so just wait until it has been unbound and then we
     // can destroy the client

  sync_completion_wait(&unbound, ZX_TIME_INFINITE);

  ASSERT_FALSE(open_sessions().empty());

  optee_client = nullptr;

  EXPECT_TRUE(open_sessions().empty());
}

class FakeRpmb : public fidl::WireServer<frpmb::Rpmb> {
 public:
  using RpmbRequestCallback = fit::function<void(fuchsia_hardware_rpmb::wire::Request &request,
                                                 RequestCompleter::Sync &completer)>;
  using GetInfoCallback = fit::function<void(GetDeviceInfoCompleter::Sync &completer)>;
  FakeRpmb() {}

  void GetDeviceInfo(GetDeviceInfoCompleter::Sync &completer) override {
    if (info_callback_) {
      info_callback_(completer);
    } else {
      completer.Close(ZX_ERR_NOT_SUPPORTED);
    }
  }

  void Request(RequestRequestView request, RequestCompleter::Sync &completer) override {
    if (request_callback_) {
      request_callback_(request->request, completer);
    } else {
      completer.Close(ZX_ERR_NOT_SUPPORTED);
    }
  }

  void Reset() {
    info_callback_ = nullptr;
    request_callback_ = nullptr;
  }
  void SetRequestCallback(RpmbRequestCallback &&callback) {
    request_callback_ = std::move(callback);
  }
  void SetInfoCallback(GetInfoCallback &&callback) { info_callback_ = std::move(callback); }

 private:
  RpmbRequestCallback request_callback_{nullptr};
  GetInfoCallback info_callback_{nullptr};
};

class OpteeClientTestRpmb : public OpteeClientTestBase {
 public:
  static const size_t kMaxFramesSize = 4096;
  const size_t kMessageSize = 160;

  const int kDefaultSessionId = 1;
  const int kDefaultCommand = 1;

  static constexpr uint8_t kMarker[] = {0xd, 0xe, 0xa, 0xd, 0xb, 0xe, 0xe, 0xf};

  OpteeClientTestRpmb() : rpmb_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    ASSERT_OK(rpmb_loop_.StartThread());

    auto [client_end, server_end] = fidl::Endpoints<fuchsia_tee::Application>::Create();
    optee_client_.reset(new OpteeClient(this, fidl::ClientEnd<fuchsia_tee_manager::Provider>(),
                                        optee::Uuid{kOpteeOsUuid}));
    fidl::BindServer(loop_.dispatcher(), std::move(server_end), optee_client_.get());
    optee_client_fidl_ = fidl::WireSyncClient<fuchsia_tee::Application>(std::move(client_end));

    // Create fake RPMB
    fake_rpmb_.reset(new FakeRpmb());
  }

  CallResult CallWithMessage(const optee::Message &message, RpcHandler rpc_handler) override {
    size_t offset = message.paddr() - shared_memory_paddr_;

    MessageHeader *hdr = reinterpret_cast<MessageHeader *>(shared_memory_vaddr_ + offset);
    hdr->return_origin = TEEC_ORIGIN_TEE;
    hdr->return_code = TEEC_SUCCESS;

    switch (hdr->command) {
      case Message::Command::kOpenSession: {
        AllocMemory(kMessageSize, &message_paddr_, &message_mem_id_, rpc_handler);
        AllocMemory(kMaxFramesSize, &tx_frames_paddr_, &tx_frames_mem_id_, rpc_handler);
        AllocMemory(kMaxFramesSize, &rx_frames_paddr_, &rx_frames_mem_id_, rpc_handler);

        hdr->session_id = kDefaultSessionId;
        break;
      }
      case Message::Command::kCloseSession: {
        EXPECT_EQ(hdr->session_id, kDefaultSessionId);
        FreeMemory(message_mem_id_, rpc_handler);
        FreeMemory(tx_frames_mem_id_, rpc_handler);
        FreeMemory(rx_frames_mem_id_, rpc_handler);
        break;
      }
      case Message::Command::kInvokeCommand: {
        offset = message_paddr_ - shared_memory_paddr_;
        MessageRaw *rpmb_access = reinterpret_cast<MessageRaw *>(shared_memory_vaddr_ + offset);
        rpmb_access->hdr.command = RpcMessage::Command::kAccessReplayProtectedMemoryBlock;
        rpmb_access->hdr.num_params = 2;

        rpmb_access->params[0].attribute = MessageParam::kAttributeTypeTempMemInput;
        rpmb_access->params[0].payload.temporary_memory.shared_memory_reference = tx_frames_mem_id_;
        rpmb_access->params[0].payload.temporary_memory.buffer = tx_frames_paddr_;
        rpmb_access->params[0].payload.temporary_memory.size = tx_frames_size_;

        rpmb_access->params[1].attribute = MessageParam::kAttributeTypeTempMemOutput;
        rpmb_access->params[1].payload.temporary_memory.shared_memory_reference = rx_frames_mem_id_;
        rpmb_access->params[1].payload.temporary_memory.buffer = rx_frames_paddr_;
        rpmb_access->params[1].payload.temporary_memory.size = rx_frames_size_;

        RpcFunctionArgs args;
        RpcFunctionResult result;
        args.generic.status = kReturnRpcPrefix | kRpcFunctionIdExecuteCommand;
        args.execute_command.msg_mem_id_upper32 = message_mem_id_ >> 32;
        args.execute_command.msg_mem_id_lower32 = message_mem_id_ & 0xFFFFFFFF;

        zx_status_t status = rpc_handler(args, &result);
        if (status != ZX_OK) {
          hdr->return_code = rpmb_access->hdr.return_code;
        }

        break;
      }
      default:
        hdr->return_code = TEEC_ERROR_NOT_IMPLEMENTED;
    }

    return CallResult{.return_code = kReturnOk};
  }

  zx::result<fidl::ClientEnd<frpmb::Rpmb>> RpmbConnectServer() const override {
    auto endpoints = fidl::CreateEndpoints<frpmb::Rpmb>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    fidl::BindServer(rpmb_loop_.dispatcher(), std::move(endpoints->server), fake_rpmb_.get());
    return zx::ok(std::move(endpoints->client));
  }

  void SetUp() override {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = optee_client_fidl_->OpenSession2(std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().session_id, kDefaultSessionId);
  }

  void TearDown() override {
    auto res = optee_client_fidl_->CloseSession(kDefaultSessionId);
    EXPECT_OK(res.status());

    message_paddr_ = 0;
    message_mem_id_ = 0;
    tx_frames_paddr_ = 0;
    tx_frames_mem_id_ = 0;
    rx_frames_paddr_ = 0;
    rx_frames_mem_id_ = 0;
    tx_frames_size_ = 0;
    rx_frames_size_ = 0;

    fake_rpmb_->Reset();
  }

 protected:
  uint8_t *GetTxBuffer() const {
    size_t offset = tx_frames_paddr_ - shared_memory_paddr_;
    return reinterpret_cast<uint8_t *>(shared_memory_vaddr_ + offset);
  }

  uint8_t *GetRxBuffer() const {
    size_t offset = rx_frames_paddr_ - shared_memory_paddr_;
    return reinterpret_cast<uint8_t *>(shared_memory_vaddr_ + offset);
  }

  uint64_t message_paddr_{0};
  uint64_t message_mem_id_{0};
  uint64_t tx_frames_paddr_{0};
  uint64_t tx_frames_mem_id_{0};
  uint64_t rx_frames_paddr_{0};
  uint64_t rx_frames_mem_id_{0};
  size_t tx_frames_size_{0};
  size_t rx_frames_size_{0};

  std::unique_ptr<FakeRpmb> fake_rpmb_;
  async::Loop rpmb_loop_;

  std::unique_ptr<OpteeClient> optee_client_;
  fidl::WireSyncClient<fuchsia_tee::Application> optee_client_fidl_;
};

TEST_F(OpteeClientTestRpmb, InvalidRequestCommand) {
  rx_frames_size_ = 512;
  tx_frames_size_ = 512;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = 5;

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
}

TEST_F(OpteeClientTestRpmb, RpmbError) {
  int req_cnt = 0;
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestKey);

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    req_cnt++;
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_ITEM_NOT_FOUND);
  EXPECT_EQ(req_cnt, 1);
}

TEST_F(OpteeClientTestRpmb, RpmbCommunicationError) {
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestKey);

  fake_rpmb_->SetRequestCallback(
      [&](auto &request, auto &completer) { completer.Close(ZX_ERR_NOT_SUPPORTED); });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_COMMUNICATION);
}

TEST_F(OpteeClientTestRpmb, GetDeviceInfo) {
  tx_frames_size_ = sizeof(RpmbReq);
  rx_frames_size_ = sizeof(RpmbDevInfo);
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdGetDevInfo;

  fake_rpmb_->SetInfoCallback([&](auto &completer) {
    using DeviceInfo = fuchsia_hardware_rpmb::wire::DeviceInfo;
    using EmmcDeviceInfo = fuchsia_hardware_rpmb::wire::EmmcDeviceInfo;

    EmmcDeviceInfo emmc_info = {};
    emmc_info.rpmb_size = 0x74;
    emmc_info.reliable_write_sector_count = 1;

    EmmcDeviceInfo aligned_emmc_info(emmc_info);
    auto emmc_info_ptr = fidl::ObjectView<EmmcDeviceInfo>::FromExternal(&aligned_emmc_info);

    completer.Reply(DeviceInfo::WithEmmcInfo(emmc_info_ptr));
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);

  RpmbDevInfo *info = reinterpret_cast<RpmbDevInfo *>(GetRxBuffer());
  EXPECT_EQ(info->ret_code, RpmbDevInfo::kRpmbCmdRetOK);
  EXPECT_EQ(info->rpmb_size, 0x74);
  EXPECT_EQ(info->rel_write_sector_count, 1);
}

TEST_F(OpteeClientTestRpmb, GetDeviceInfoWrongFrameSize) {
  tx_frames_size_ = sizeof(RpmbReq) + 1;
  rx_frames_size_ = sizeof(RpmbDevInfo);
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdGetDevInfo;

  printf("Size of RpmbReq %zu, RpmbFrame %zu\n", sizeof(RpmbReq), sizeof(RpmbFrame));

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
}

TEST_F(OpteeClientTestRpmb, InvalidDataRequest) {
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = 10;

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
}

TEST_F(OpteeClientTestRpmb, InvalidDataRequestFrameSize) {
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame) + 1;
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = 10;

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
}

TEST_F(OpteeClientTestRpmb, RequestKeyOk) {
  int req_cnt = 0;
  uint8_t data[fuchsia_hardware_rpmb::wire::kFrameSize];

  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestKey);
  memcpy(rpmb_req->frames->stuff, kMarker, sizeof(kMarker));

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    if (req_cnt == 0) {  // first call
      EXPECT_EQ(request.tx_frames.size, fuchsia_hardware_rpmb::wire::kFrameSize);
      EXPECT_FALSE(request.rx_frames);

      EXPECT_OK(request.tx_frames.vmo.read(data, request.tx_frames.offset, sizeof(kMarker)));
      EXPECT_EQ(memcmp(data, kMarker, sizeof(kMarker)), 0);

    } else if (req_cnt == 1) {  // second call
      EXPECT_EQ(request.tx_frames.size, fuchsia_hardware_rpmb::wire::kFrameSize);
      EXPECT_TRUE(request.rx_frames);
      EXPECT_EQ(request.rx_frames->size, fuchsia_hardware_rpmb::wire::kFrameSize);

      EXPECT_OK(request.tx_frames.vmo.read(data, request.tx_frames.offset, sizeof(data)));
      RpmbFrame *frame = reinterpret_cast<RpmbFrame *>(data);
      EXPECT_EQ(frame->request, htobe16(RpmbFrame::kRpmbRequestStatus));
      EXPECT_OK(request.rx_frames->vmo.write(kMarker, request.rx_frames->offset, sizeof(kMarker)));
    }
    req_cnt++;

    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  EXPECT_EQ(req_cnt, 2);
  EXPECT_EQ(memcmp(GetRxBuffer(), kMarker, sizeof(kMarker)), 0);
}

TEST_F(OpteeClientTestRpmb, RequestKeyInvalid) {
  int req_cnt = 0;
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize * 2;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestKey);

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    req_cnt++;
    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
  EXPECT_EQ(req_cnt, 0);
}

TEST_F(OpteeClientTestRpmb, RequestWCounterOk) {
  int req_cnt = 0;
  uint8_t data[sizeof(kMarker)];

  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestWCounter);
  memcpy(rpmb_req->frames->stuff, kMarker, sizeof(kMarker));

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    EXPECT_EQ(request.tx_frames.size, fuchsia_hardware_rpmb::wire::kFrameSize);
    EXPECT_TRUE(request.rx_frames);
    EXPECT_EQ(request.rx_frames->size, fuchsia_hardware_rpmb::wire::kFrameSize);

    EXPECT_OK(request.tx_frames.vmo.read(data, request.tx_frames.offset, sizeof(kMarker)));
    EXPECT_EQ(memcmp(data, kMarker, sizeof(kMarker)), 0);
    EXPECT_OK(request.rx_frames->vmo.write(kMarker, request.rx_frames->offset, sizeof(kMarker)));
    req_cnt++;

    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  EXPECT_EQ(req_cnt, 1);
  EXPECT_EQ(memcmp(GetRxBuffer(), kMarker, sizeof(kMarker)), 0);
}

TEST_F(OpteeClientTestRpmb, RequestWCounterInvalid) {
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize * 2;
  int req_cnt = 0;

  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestWCounter);

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    req_cnt++;
    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
  EXPECT_EQ(req_cnt, 0);
}

TEST_F(OpteeClientTestRpmb, ReadDataOk) {
  int req_cnt = 0;
  uint8_t data[sizeof(kMarker)];

  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize * 2;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestReadData);
  memcpy(rpmb_req->frames->stuff, kMarker, sizeof(kMarker));

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    EXPECT_EQ(request.tx_frames.size, fuchsia_hardware_rpmb::wire::kFrameSize);
    EXPECT_TRUE(request.rx_frames);

    EXPECT_OK(request.tx_frames.vmo.read(data, request.tx_frames.offset, sizeof(kMarker)));
    EXPECT_EQ(memcmp(data, kMarker, sizeof(kMarker)), 0);
    EXPECT_OK(request.rx_frames->vmo.write(kMarker, request.rx_frames->offset, sizeof(kMarker)));
    req_cnt++;

    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  EXPECT_EQ(req_cnt, 1);
  EXPECT_EQ(memcmp(GetRxBuffer(), kMarker, sizeof(kMarker)), 0);
}

TEST_F(OpteeClientTestRpmb, RequestReadInvalid) {
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame) + fuchsia_hardware_rpmb::wire::kFrameSize;
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  int req_cnt = 0;

  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestReadData);

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    req_cnt++;
    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
  EXPECT_EQ(req_cnt, 0);
}

TEST_F(OpteeClientTestRpmb, WriteDataOk) {
  int req_cnt = 0;
  uint8_t data[fuchsia_hardware_rpmb::wire::kFrameSize];

  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize;
  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestWriteData);
  memcpy(rpmb_req->frames->stuff, kMarker, sizeof(kMarker));

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    if (req_cnt == 0) {  // first call
      EXPECT_EQ(request.tx_frames.size, fuchsia_hardware_rpmb::wire::kFrameSize);
      EXPECT_FALSE(request.rx_frames);

      EXPECT_OK(request.tx_frames.vmo.read(data, request.tx_frames.offset, sizeof(kMarker)));
      EXPECT_EQ(memcmp(data, kMarker, sizeof(kMarker)), 0);

    } else if (req_cnt == 1) {  // second call
      EXPECT_EQ(request.tx_frames.size, fuchsia_hardware_rpmb::wire::kFrameSize);
      EXPECT_TRUE(request.rx_frames);
      EXPECT_EQ(request.rx_frames->size, fuchsia_hardware_rpmb::wire::kFrameSize);

      EXPECT_OK(request.tx_frames.vmo.read(data, request.tx_frames.offset, sizeof(data)));
      RpmbFrame *frame = reinterpret_cast<RpmbFrame *>(data);
      EXPECT_EQ(frame->request, htobe16(RpmbFrame::kRpmbRequestStatus));
      EXPECT_OK(request.rx_frames->vmo.write(kMarker, request.rx_frames->offset, sizeof(kMarker)));
    }
    req_cnt++;

    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  EXPECT_EQ(req_cnt, 2);
  EXPECT_EQ(memcmp(GetRxBuffer(), kMarker, sizeof(kMarker)), 0);
}

TEST_F(OpteeClientTestRpmb, RequestWriteInvalid) {
  tx_frames_size_ = sizeof(RpmbReq) + sizeof(RpmbFrame);
  rx_frames_size_ = fuchsia_hardware_rpmb::wire::kFrameSize * 2;
  int req_cnt = 0;

  RpmbReq *rpmb_req = reinterpret_cast<RpmbReq *>(GetTxBuffer());
  rpmb_req->cmd = RpmbReq::kCmdDataRequest;

  rpmb_req->frames->request = htobe16(RpmbFrame::kRpmbRequestWriteData);

  fake_rpmb_->SetRequestCallback([&](auto &request, auto &completer) {
    req_cnt++;
    completer.ReplySuccess();
  });

  fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
  auto res = optee_client_fidl_->InvokeCommand(kDefaultSessionId, kDefaultCommand,
                                               std::move(parameter_set));
  EXPECT_OK(res.status());
  EXPECT_EQ(res.value().op_result.return_code(), TEEC_ERROR_BAD_PARAMETERS);
  EXPECT_EQ(req_cnt, 0);
}

class OpteeClientTestWaitQueue : public OpteeClientTestBase {
 public:
  const int kInitSessionId = 0;
  static constexpr int kSleepCommand = 1;
  static constexpr int kWakeUpCommand = 2;
  static constexpr int kNopeCommand = 3;

  OpteeClientTestWaitQueue() : clients_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    ASSERT_OK(clients_loop_.StartThread());
  }

  CallResult CallWithMessage(const optee::Message &message, RpcHandler rpc_handler) override {
    size_t offset = message.paddr() - shared_memory_paddr_;

    MessageHeader *hdr = reinterpret_cast<MessageHeader *>(shared_memory_vaddr_ + offset);
    hdr->return_origin = TEEC_ORIGIN_TEE;
    hdr->return_code = TEEC_SUCCESS;

    switch (hdr->command) {
      case Message::Command::kOpenSession: {
        hdr->session_id = ++cur_sid_;
        break;
      }
      case Message::Command::kInvokeCommand: {
        EXPECT_LE(hdr->session_id, cur_sid_);
        switch (hdr->app_function) {
          case kSleepCommand:
            hdr->return_code = handle_wq_message(rpc_handler, WaitQueueRpcMessage::Command::kSleep);
            break;
          case kWakeUpCommand:
            hdr->return_code =
                handle_wq_message(rpc_handler, WaitQueueRpcMessage::Command::kWakeUp);
            break;
          case kNopeCommand:
            // do nothing
            break;
          default:
            EXPECT_TRUE(false);
        }
        invoke_done_cnt_++;
        break;
      }
      case Message::Command::kCloseSession: {
        break;
      }
      default:
        hdr->return_code = TEEC_ERROR_NOT_IMPLEMENTED;
    }

    return CallResult{.return_code = kReturnOk};
  }

  uint32_t handle_wq_message(RpcHandler &rpc_handler, uint16_t cmd) {
    uint64_t message_paddr = 0;
    uint64_t message_mem_id = 0;
    uint32_t ret = TEEC_SUCCESS;

    AllocMemory(sizeof(MessageRaw), &message_paddr, &message_mem_id, rpc_handler);
    uint64_t offset = message_paddr - shared_memory_paddr_;

    MessageRaw *wq_msg = reinterpret_cast<MessageRaw *>(shared_memory_vaddr_ + offset);
    wq_msg->hdr.command = RpcMessage::Command::kWaitQueue;
    wq_msg->hdr.num_params = 1;

    wq_msg->params[0].attribute = MessageParam::kAttributeTypeValueInput;
    wq_msg->params[0].payload.value.wait_queue.key = sleep_key_;
    wq_msg->params[0].payload.value.wait_queue.command = cmd;

    RpcFunctionArgs args;
    RpcFunctionResult result;
    args.generic.status = kReturnRpcPrefix | kRpcFunctionIdExecuteCommand;
    args.execute_command.msg_mem_id_upper32 = message_mem_id >> 32;
    args.execute_command.msg_mem_id_lower32 = message_mem_id & 0xFFFFFFFF;

    zx_status_t status = rpc_handler(args, &result);
    if (status != ZX_OK) {
      ret = wq_msg->hdr.return_code;
    }

    FreeMemory(message_mem_id, rpc_handler);
    return ret;
  }

  void SetUp() override {
    cur_sid_ = kInitSessionId;
    sleep_key_ = 0;
    invoke_done_cnt_ = 0;
  }

  void TearDown() override {}

 protected:
  int cur_sid_{kInitSessionId};
  int sleep_key_{0};
  int invoke_done_cnt_{0};

  async::Loop clients_loop_;
};

TEST_F(OpteeClientTestWaitQueue, WakeUpBeforeSleep) {
  auto [client1_end, server1_end] = fidl::Endpoints<fuchsia_tee::Application>::Create();
  auto [client2_end, server2_end] = fidl::Endpoints<fuchsia_tee::Application>::Create();
  auto optee1_client = std::make_unique<OpteeClient>(
      this, fidl::ClientEnd<fuchsia_tee_manager::Provider>(), optee::Uuid{kOpteeOsUuid});
  auto optee2_client = std::make_unique<OpteeClient>(
      this, fidl::ClientEnd<fuchsia_tee_manager::Provider>(), optee::Uuid{kOpteeOsUuid});

  fidl::BindServer(loop_.dispatcher(), std::move(server1_end), optee1_client.get());
  fidl::BindServer(loop_.dispatcher(), std::move(server2_end), optee2_client.get());

  fidl::WireSharedClient fidl_client1(std::move(client1_end), clients_loop_.dispatcher());
  fidl::WireSyncClient<fuchsia_tee::Application> fidl_client2(std::move(client2_end));
  sync_completion_t completion;

  uint32_t sid1;
  uint32_t sid2;
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2> &result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              auto *resp = result.Unwrap();
              EXPECT_EQ(resp->session_id, cur_sid_);
              sid1 = resp->session_id;
              sync_completion_signal(&completion);
            });
  }
  sync_completion_wait(&completion, ZX_TIME_INFINITE);
  sync_completion_reset(&completion);

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client2->OpenSession2(std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().session_id, cur_sid_);
    sid2 = res.value().session_id;
  }

  sleep_key_ = 1;

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client2->InvokeCommand(sid2, kWakeUpCommand, std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  }

  EXPECT_EQ(invoke_done_cnt_, 1);
  EXPECT_EQ(this->WaitQueueSize(), 1);

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client2->InvokeCommand(sid2, kWakeUpCommand, std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  }

  EXPECT_EQ(invoke_done_cnt_, 2);
  EXPECT_EQ(this->WaitQueueSize(), 1);

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client2->InvokeCommand(sid2, kNopeCommand, std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  }

  EXPECT_EQ(invoke_done_cnt_, 3);
  EXPECT_EQ(this->WaitQueueSize(), 1);

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->InvokeCommand(sid1, kSleepCommand, parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::InvokeCommand> &result) {
              if (!result.ok()) {
                FAIL("InvokeCommand failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              auto *resp = result.Unwrap();
              EXPECT_EQ(resp->op_result.return_code(), TEEC_SUCCESS);
              sync_completion_signal(&completion);
            });
  }

  sync_completion_wait(&completion, ZX_TIME_INFINITE);
  EXPECT_EQ(invoke_done_cnt_, 4);
  EXPECT_EQ(this->WaitQueueSize(), 0);
}

TEST_F(OpteeClientTestWaitQueue, SleepWakeup) {
  auto [client1_end, server1_end] = fidl::Endpoints<fuchsia_tee::Application>::Create();
  auto [client2_end, server2_end] = fidl::Endpoints<fuchsia_tee::Application>::Create();
  auto optee1_client = std::make_unique<OpteeClient>(
      this, fidl::ClientEnd<fuchsia_tee_manager::Provider>(), optee::Uuid{kOpteeOsUuid});
  auto optee2_client = std::make_unique<OpteeClient>(
      this, fidl::ClientEnd<fuchsia_tee_manager::Provider>(), optee::Uuid{kOpteeOsUuid});

  fidl::BindServer(loop_.dispatcher(), std::move(server1_end), optee1_client.get());
  fidl::BindServer(loop_.dispatcher(), std::move(server2_end), optee2_client.get());

  fidl::WireSharedClient fidl_client1(std::move(client1_end), clients_loop_.dispatcher());
  fidl::WireSyncClient<fuchsia_tee::Application> fidl_client2(std::move(client2_end));
  sync_completion_t completion;
  zx_status_t status;

  uint32_t sid1;
  uint32_t sid2;
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2> &result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              auto *resp = result.Unwrap();
              EXPECT_EQ(resp->session_id, cur_sid_);
              sid1 = resp->session_id;
              sync_completion_signal(&completion);
            });
  }
  sync_completion_wait(&completion, ZX_TIME_INFINITE);

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client2->OpenSession2(std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().session_id, cur_sid_);
    sid2 = res.value().session_id;
  }

  sync_completion_reset(&completion);
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->InvokeCommand(sid1, kSleepCommand, parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::InvokeCommand> &result) {
              if (!result.ok()) {
                FAIL("InvokeCommand failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              auto *resp = result.Unwrap();
              EXPECT_EQ(resp->op_result.return_code(), TEEC_SUCCESS);
              sync_completion_signal(&completion);
            });
  }

  EXPECT_EQ(invoke_done_cnt_, 0);

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client2->InvokeCommand(sid2, kNopeCommand, std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  }

  EXPECT_EQ(invoke_done_cnt_, 1);
  EXPECT_FALSE(sync_completion_signaled(&completion));

  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    auto res = fidl_client2->InvokeCommand(sid2, kWakeUpCommand, std::move(parameter_set));
    EXPECT_OK(res.status());
    EXPECT_EQ(res.value().op_result.return_code(), TEEC_SUCCESS);
  }

  status = sync_completion_wait(&completion, ZX_TIME_INFINITE);
  EXPECT_OK(status);
  EXPECT_EQ(invoke_done_cnt_, 3);
  EXPECT_EQ(this->WaitQueueSize(), 0);
}

}  // namespace
}  // namespace optee
