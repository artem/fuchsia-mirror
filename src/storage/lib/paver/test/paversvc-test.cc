// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.paver/cpp/wire.h>
#include <lib/abr/data.h>
#include <lib/abr/util.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/cksum.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/string_view.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/sysconfig/sync-client.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/vmo.h>
#include <sparse_format.h>

#include <numeric>
// Clean up the unhelpful defines from sparse_format.h
#undef error

#include <zircon/hw/gpt.h>

#include <memory>

#include <fbl/algorithm.h>
#include <fbl/unique_fd.h>
#include <soc/aml-common/aml-guid.h>
#include <zxtest/zxtest.h>

#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/astro.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/fvm.h"
#include "src/storage/lib/paver/gpt.h"
#include "src/storage/lib/paver/luis.h"
#include "src/storage/lib/paver/nelson.h"
#include "src/storage/lib/paver/paver.h"
#include "src/storage/lib/paver/sherlock.h"
#include "src/storage/lib/paver/test/test-utils.h"
#include "src/storage/lib/paver/utils.h"
#include "src/storage/lib/paver/vim3.h"
#include "src/storage/lib/paver/violet.h"
#include "src/storage/lib/paver/x64.h"

namespace {

namespace partition = fuchsia_hardware_block_partition;

using device_watcher::RecursiveWaitForFile;
using driver_integration_test::IsolatedDevmgr;

constexpr std::string_view kFirmwareTypeBootloader;
constexpr std::string_view kFirmwareTypeBl2 = "bl2";
constexpr std::string_view kFirmwareTypeUnsupported = "unsupported_type";

// BL2 images must be exactly this size.
constexpr size_t kBl2ImageSize = 0x10000;
// Make sure we can use our page-based APIs to work with the BL2 image.
static_assert(kBl2ImageSize % kPageSize == 0);
constexpr size_t kBl2ImagePages = kBl2ImageSize / kPageSize;

constexpr uint32_t kBootloaderFirstBlock = 4;
constexpr uint32_t kBootloaderBlocks = 4;
constexpr uint32_t kBootloaderLastBlock = kBootloaderFirstBlock + kBootloaderBlocks - 1;
constexpr uint32_t kZirconAFirstBlock = kBootloaderLastBlock + 1;
constexpr uint32_t kZirconALastBlock = kZirconAFirstBlock + 1;
constexpr uint32_t kBl2FirstBlock = kNumBlocks - 1;
constexpr uint32_t kFvmFirstBlock = 18;

fuchsia_hardware_nand::wire::RamNandInfo NandInfo() {
  return {
      .nand_info =
          {
              .page_size = kPageSize,
              .pages_per_block = kPagesPerBlock,
              .num_blocks = kNumBlocks,
              .ecc_bits = 8,
              .oob_size = kOobSize,
              .nand_class = fuchsia_hardware_nand::wire::Class::kPartmap,
              .partition_guid = {},
          },
      .partition_map =
          {
              .device_guid = {},
              .partition_count = 8,
              .partitions =
                  {
                      fuchsia_hardware_nand::wire::Partition{
                          .type_guid = {},
                          .unique_guid = {},
                          .first_block = 0,
                          .last_block = 3,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name = {},
                          .hidden = true,
                          .bbt = true,
                      },
                      {
                          .type_guid = GUID_BOOTLOADER_VALUE,
                          .unique_guid = {},
                          .first_block = kBootloaderFirstBlock,
                          .last_block = kBootloaderLastBlock,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name = {'b', 'o', 'o', 't', 'l', 'o', 'a', 'd', 'e', 'r'},
                          .hidden = false,
                          .bbt = false,
                      },
                      {
                          .type_guid = GUID_ZIRCON_A_VALUE,
                          .unique_guid = {},
                          .first_block = kZirconAFirstBlock,
                          .last_block = kZirconALastBlock,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name = {'z', 'i', 'r', 'c', 'o', 'n', '-', 'a'},
                          .hidden = false,
                          .bbt = false,
                      },
                      {
                          .type_guid = GUID_ZIRCON_B_VALUE,
                          .unique_guid = {},
                          .first_block = 10,
                          .last_block = 11,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name = {'z', 'i', 'r', 'c', 'o', 'n', '-', 'b'},
                          .hidden = false,
                          .bbt = false,
                      },
                      {
                          .type_guid = GUID_ZIRCON_R_VALUE,
                          .unique_guid = {},
                          .first_block = 12,
                          .last_block = 13,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name = {'z', 'i', 'r', 'c', 'o', 'n', '-', 'r'},
                          .hidden = false,
                          .bbt = false,
                      },
                      {
                          .type_guid = GUID_SYS_CONFIG_VALUE,
                          .unique_guid = {},
                          .first_block = 14,
                          .last_block = 17,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name = {'s', 'y', 's', 'c', 'o', 'n', 'f', 'i', 'g'},
                          .hidden = false,
                          .bbt = false,
                      },
                      {
                          .type_guid = GUID_FVM_VALUE,
                          .unique_guid = {},
                          .first_block = kFvmFirstBlock,
                          .last_block = kBl2FirstBlock - 1,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name = {'f', 'v', 'm'},
                          .hidden = false,
                          .bbt = false,
                      },
                      {
                          .type_guid = GUID_BL2_VALUE,
                          .unique_guid = {},
                          .first_block = kBl2FirstBlock,
                          .last_block = kBl2FirstBlock,
                          .copy_count = 0,
                          .copy_byte_offset = 0,
                          .name =
                              {
                                  'b',
                                  'l',
                                  '2',
                              },
                          .hidden = false,
                          .bbt = false,
                      },
                  },
          },
      .export_nand_config = true,
      .export_partition_map = true,
  };
}

class FakeBootArgs : public fidl::WireServer<fuchsia_boot::Arguments> {
 public:
  void GetString(GetStringRequestView request, GetStringCompleter::Sync& completer) override {
    auto iter = string_args_.find(request->key.data());
    if (iter == string_args_.end()) {
      completer.Reply({});
      return;
    }
    completer.Reply(fidl::StringView::FromExternal(iter->second));
  }

  // Stubs
  void GetStrings(GetStringsRequestView request, GetStringsCompleter::Sync& completer) override {
    std::vector<fidl::StringView> response = {
        fidl::StringView::FromExternal(arg_response_),
        fidl::StringView(),
    };
    completer.Reply(fidl::VectorView<fidl::StringView>::FromExternal(response));
  }
  void GetBool(GetBoolRequestView request, GetBoolCompleter::Sync& completer) override {
    if (strncmp(request->key.data(), "astro.sysconfig.abr-wear-leveling",
                sizeof("astro.sysconfig.abr-wear-leveling")) == 0) {
      completer.Reply(astro_sysconfig_abr_wear_leveling_);
    } else {
      completer.Reply(request->defaultval);
    }
  }
  void GetBools(GetBoolsRequestView request, GetBoolsCompleter::Sync& completer) override {}
  void Collect(CollectRequestView request, CollectCompleter::Sync& completer) override {}

  void SetAstroSysConfigAbrWearLeveling(bool opt) { astro_sysconfig_abr_wear_leveling_ = opt; }

  void SetArgResponse(std::string arg_response) { arg_response_ = std::move(arg_response); }

  void AddStringArgs(std::string key, std::string value) {
    string_args_[std::move(key)] = std::move(value);
  }

 private:
  bool astro_sysconfig_abr_wear_leveling_ = false;
  std::string arg_response_ = "-a";

  std::unordered_map<std::string, std::string> string_args_;
};

class PaverServiceTest : public zxtest::Test {
 public:
  PaverServiceTest();

  ~PaverServiceTest() override;

 protected:
  static void CreatePayload(size_t num_pages, fuchsia_mem::wire::Buffer* out);

  static constexpr size_t kKilobyte = 1 << 10;

  static void ValidateWritten(const fuchsia_mem::wire::Buffer& buf, size_t num_pages) {
    ASSERT_GE(buf.size, num_pages * kPageSize);
    fzl::VmoMapper mapper;
    ASSERT_OK(mapper.Map(buf.vmo, 0,
                         fbl::round_up(num_pages * kPageSize, zx_system_get_page_size()),
                         ZX_VM_PERM_READ));
    const uint8_t* start = reinterpret_cast<uint8_t*>(mapper.start());
    for (size_t i = 0; i < num_pages * kPageSize; i++) {
      ASSERT_EQ(start[i], 0x4a, "i = %zu", i);
    }
  }

  std::unique_ptr<paver::Paver> paver_;
  fidl::WireSyncClient<fuchsia_paver::Paver> client_;
  async::Loop loop_;
  // The paver makes synchronous calls into /svc, so it must run in a separate loop to not
  // deadlock.
  async::Loop loop2_;
  FakeSvc<FakeBootArgs> fake_svc_;
};

PaverServiceTest::PaverServiceTest()
    : loop_(&kAsyncLoopConfigAttachToCurrentThread),
      loop2_(&kAsyncLoopConfigNoAttachToCurrentThread),
      fake_svc_(loop2_.dispatcher(), FakeBootArgs()) {
  auto [client, server] = fidl::Endpoints<fuchsia_paver::Paver>::Create();

  client_ = fidl::WireSyncClient(std::move(client));

  paver_ = std::make_unique<paver::Paver>();
  paver_->set_dispatcher(loop_.dispatcher());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::AstroPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::NelsonPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::SherlockPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::LuisPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::Vim3PartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::VioletPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::X64PartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::DefaultPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::AstroAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::NelsonAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::SherlockAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::LuisAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::Vim3AbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::VioletAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::X64AbrClientFactory>());

  fidl::BindServer(loop_.dispatcher(), std::move(server), paver_.get());
  loop_.StartThread("paver-svc-test-loop");
  loop2_.StartThread("paver-svc-test-loop-2");
}

PaverServiceTest::~PaverServiceTest() {
  loop_.Shutdown();
  loop2_.Shutdown();
  paver_.reset();
}

void PaverServiceTest::CreatePayload(size_t num_pages, fuchsia_mem::wire::Buffer* out) {
  zx::vmo vmo;
  fzl::VmoMapper mapper;
  const size_t size = kPageSize * num_pages;
  ASSERT_OK(mapper.CreateAndMap(size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &vmo));
  memset(mapper.start(), 0x4a, mapper.size());
  out->vmo = std::move(vmo);
  out->size = size;
}

class PaverServiceSkipBlockTest : public PaverServiceTest {
 public:
  // Initializes the RAM NAND device.
  void InitializeRamNand(fuchsia_hardware_nand::wire::RamNandInfo nand_info = NandInfo()) {
    ASSERT_NO_FATAL_FAILURE(SpawnIsolatedDevmgr(std::move(nand_info)));
    ASSERT_NO_FATAL_FAILURE(WaitForDevices());
  }

 protected:
  void SpawnIsolatedDevmgr(fuchsia_hardware_nand::wire::RamNandInfo nand_info) {
    ASSERT_EQ(device_.get(), nullptr);
    ASSERT_NO_FATAL_FAILURE(SkipBlockDevice::Create(std::move(nand_info), &device_));
    paver_->set_dispatcher(loop_.dispatcher());
    paver_->set_devfs_root(device_->devfs_root());
    paver_->set_svc_root(std::move(fake_svc_.svc_chan()));
  }

  void WaitForDevices() {
    ASSERT_OK(RecursiveWaitForFile(device_->devfs_root().get(),
                                   "sys/platform/00:00:2e/nand-ctl/ram-nand-0/sysconfig/skip-block")
                  .status_value());
    zx::result fvm_result = RecursiveWaitForFile(
        device_->devfs_root().get(), "sys/platform/00:00:2e/nand-ctl/ram-nand-0/fvm/ftl/block");
    ASSERT_OK(fvm_result.status_value());
    fvm_client_ = fidl::ClientEnd<fuchsia_hardware_block::Block>(std::move(fvm_result.value()));
  }

  void FindBootManager() {
    auto [local, remote] = fidl::Endpoints<fuchsia_paver::BootManager>::Create();

    auto result = client_->FindBootManager(std::move(remote));
    ASSERT_OK(result.status());
    boot_manager_ = fidl::WireSyncClient(std::move(local));
  }

  void FindDataSink() {
    auto [local, remote] = fidl::Endpoints<fuchsia_paver::DataSink>::Create();

    auto result = client_->FindDataSink(std::move(remote));
    ASSERT_OK(result.status());
    data_sink_ = fidl::WireSyncClient(std::move(local));
  }

  void FindSysconfig() {
    auto [local, remote] = fidl::Endpoints<fuchsia_paver::Sysconfig>::Create();

    auto result = client_->FindSysconfig(std::move(remote));
    ASSERT_OK(result.status());
    sysconfig_ = fidl::WireSyncClient(std::move(local));
  }

  void SetAbr(const AbrData& data) {
    auto* buf = reinterpret_cast<uint8_t*>(device_->mapper().start()) +
                (static_cast<size_t>(14) * kSkipBlockSize) + (static_cast<size_t>(60) * kKilobyte);
    *reinterpret_cast<AbrData*>(buf) = data;
  }

  AbrData GetAbr() {
    auto* buf = reinterpret_cast<uint8_t*>(device_->mapper().start()) +
                (static_cast<size_t>(14) * kSkipBlockSize) + (static_cast<size_t>(60) * kKilobyte);
    return *reinterpret_cast<AbrData*>(buf);
  }

  const uint8_t* SysconfigStart() {
    return reinterpret_cast<uint8_t*>(device_->mapper().start()) +
           (static_cast<size_t>(14) * kSkipBlockSize);
  }

  sysconfig_header GetSysconfigHeader() {
    const uint8_t* sysconfig_start = SysconfigStart();
    sysconfig_header ret;
    memcpy(&ret, sysconfig_start, sizeof(ret));
    return ret;
  }

  // Equivalence of GetAbr() in the context of abr wear-leveling.
  // Since there can be multiple pages in abr sub-partition that may have valid abr data,
  // argument |copy_index| is used to read a specific one.
  AbrData GetAbrInWearLeveling(const sysconfig_header& header, size_t copy_index) {
    auto* buf = SysconfigStart() + header.abr_metadata.offset + copy_index * 4 * kKilobyte;
    AbrData ret;
    memcpy(&ret, buf, sizeof(ret));
    return ret;
  }

  using PaverServiceTest::ValidateWritten;

  // Checks that the device mapper contains |expected| at each byte in the given
  // range. Uses ASSERT_EQ() per-byte to give a helpful message on failure.
  void AssertContents(size_t offset, size_t length, uint8_t expected) {
    const uint8_t* contents = static_cast<uint8_t*>(device_->mapper().start()) + offset;
    for (size_t i = 0; i < length; i++) {
      ASSERT_EQ(expected, contents[i], "i = %zu", i);
    }
  }

  void ValidateWritten(uint32_t block, size_t num_blocks) {
    AssertContents(static_cast<size_t>(block) * kSkipBlockSize, num_blocks * kSkipBlockSize, 0x4A);
  }

  void ValidateUnwritten(uint32_t block, size_t num_blocks) {
    AssertContents(static_cast<size_t>(block) * kSkipBlockSize, num_blocks * kSkipBlockSize, 0xFF);
  }

  void ValidateWrittenPages(uint32_t page, size_t num_pages) {
    AssertContents(static_cast<size_t>(page) * kPageSize, num_pages * kPageSize, 0x4A);
  }

  void ValidateUnwrittenPages(uint32_t page, size_t num_pages) {
    AssertContents(static_cast<size_t>(page) * kPageSize, num_pages * kPageSize, 0xFF);
  }

  void ValidateWrittenBytes(size_t offset, size_t num_bytes) {
    AssertContents(offset, num_bytes, 0x4A);
  }

  void ValidateUnwrittenBytes(size_t offset, size_t num_bytes) {
    AssertContents(offset, num_bytes, 0xFF);
  }

  void WriteData(uint32_t page, size_t num_pages, uint8_t data) {
    WriteDataBytes(page * kPageSize, num_pages * kPageSize, data);
  }

  void WriteDataBytes(uint32_t start, size_t num_bytes, uint8_t data) {
    memset(static_cast<uint8_t*>(device_->mapper().start()) + start, data, num_bytes);
  }

  void WriteDataBytes(uint32_t start, void* data, size_t num_bytes) {
    memcpy(static_cast<uint8_t*>(device_->mapper().start()) + start, data, num_bytes);
  }

  void TestSysconfigWriteBufferedClient(uint32_t offset_in_pages, uint32_t sysconfig_pages);

  void TestSysconfigWipeBufferedClient(uint32_t offset_in_pages, uint32_t sysconfig_pages);

  void TestQueryConfigurationLastSetActive(fuchsia_paver::wire::Configuration this_slot,
                                           fuchsia_paver::wire::Configuration other_slot);

  fidl::WireSyncClient<fuchsia_paver::BootManager> boot_manager_;
  fidl::WireSyncClient<fuchsia_paver::DataSink> data_sink_;
  fidl::WireSyncClient<fuchsia_paver::Sysconfig> sysconfig_;

  std::unique_ptr<SkipBlockDevice> device_;
  fidl::ClientEnd<fuchsia_hardware_block::Block> fvm_client_;
};

constexpr AbrData kAbrData = {
    .magic = {'\0', 'A', 'B', '0'},
    .version_major = kAbrMajorVersion,
    .version_minor = kAbrMinorVersion,
    .reserved1 = {},
    .slot_data =
        {
            {
                .priority = 0,
                .tries_remaining = 0,
                .successful_boot = 0,
                .reserved = {},
            },
            {
                .priority = 1,
                .tries_remaining = 0,
                .successful_boot = 1,
                .reserved = {},
            },
        },
    .one_shot_flags = kAbrDataOneShotFlagNone,
    .reserved2 = {},
    .crc32 = {},
};

void ComputeCrc(AbrData* data) {
  data->crc32 = htobe32(crc32(0, reinterpret_cast<const uint8_t*>(data), offsetof(AbrData, crc32)));
}

TEST_F(PaverServiceSkipBlockTest, InitializeAbr) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = {};
  memset(&abr_data, 0x3d, sizeof(abr_data));
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryActiveConfiguration();
  ASSERT_OK(result.status());
}

TEST_F(PaverServiceSkipBlockTest, InitializeAbrAlreadyValid) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryActiveConfiguration();
  ASSERT_OK(result.status());
}

TEST_F(PaverServiceSkipBlockTest, QueryActiveConfigurationInvalidAbr) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = {};
  memset(&abr_data, 0x3d, sizeof(abr_data));
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryActiveConfiguration();
  ASSERT_OK(result.status());
}

TEST_F(PaverServiceSkipBlockTest, QueryActiveConfigurationBothPriority0) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].priority = 0;
  abr_data.slot_data[1].priority = 0;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryActiveConfiguration();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_error());
  ASSERT_STATUS(result->error_value(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(PaverServiceSkipBlockTest, QueryActiveConfigurationSlotB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryActiveConfiguration();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kB);
}

TEST_F(PaverServiceSkipBlockTest, QueryActiveConfigurationSlotA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].priority = 2;
  abr_data.slot_data[0].successful_boot = 1;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryActiveConfiguration();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kA);
}

void PaverServiceSkipBlockTest::TestQueryConfigurationLastSetActive(
    fuchsia_paver::wire::Configuration this_slot, fuchsia_paver::wire::Configuration other_slot) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  // Set both slots to the active state.
  {
    auto result = boot_manager_->SetConfigurationActive(other_slot);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->SetConfigurationActive(this_slot);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  // Marking the slot successful shall not change the result.
  {
    auto result = boot_manager_->SetConfigurationHealthy(this_slot);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);

    auto get_result = boot_manager_->QueryConfigurationLastSetActive();
    ASSERT_OK(get_result.status());
    ASSERT_TRUE(get_result->is_ok());
    ASSERT_EQ(get_result->value()->configuration, this_slot);
  }

  // Marking the slot unbootable shall not change the result.
  {
    auto result = boot_manager_->SetConfigurationUnbootable(this_slot);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);

    auto get_result = boot_manager_->QueryConfigurationLastSetActive();
    ASSERT_OK(get_result.status());
    ASSERT_TRUE(get_result->is_ok());
    ASSERT_EQ(get_result->value()->configuration, this_slot);
  }

  // Marking the other slot successful shall not change the result.
  {
    auto result = boot_manager_->SetConfigurationHealthy(other_slot);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);

    auto get_result = boot_manager_->QueryConfigurationLastSetActive();
    ASSERT_OK(get_result.status());
    ASSERT_TRUE(get_result->is_ok());
    ASSERT_EQ(get_result->value()->configuration, this_slot);
  }

  // Marking the other slot unbootable shall not change the result.
  {
    auto result = boot_manager_->SetConfigurationUnbootable(other_slot);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);

    auto get_result = boot_manager_->QueryConfigurationLastSetActive();
    ASSERT_OK(get_result.status());
    ASSERT_TRUE(get_result->is_ok());
    ASSERT_EQ(get_result->value()->configuration, this_slot);
  }

  // Marking the other slot active does change the result.
  {
    auto result = boot_manager_->SetConfigurationActive(other_slot);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);

    auto get_result = boot_manager_->QueryConfigurationLastSetActive();
    ASSERT_OK(get_result.status());
    ASSERT_TRUE(get_result->is_ok());
    ASSERT_EQ(get_result->value()->configuration, other_slot);
  }
}

TEST_F(PaverServiceSkipBlockTest, QueryConfigurationLastSetActiveSlotA) {
  TestQueryConfigurationLastSetActive(fuchsia_paver::wire::Configuration::kA,
                                      fuchsia_paver::wire::Configuration::kB);
}

TEST_F(PaverServiceSkipBlockTest, QueryConfigurationLastSetActiveSlotB) {
  TestQueryConfigurationLastSetActive(fuchsia_paver::wire::Configuration::kB,
                                      fuchsia_paver::wire::Configuration::kA);
}

TEST_F(PaverServiceSkipBlockTest, QueryCurrentConfigurationSlotA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryCurrentConfiguration();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kA);
}

TEST_F(PaverServiceSkipBlockTest, QueryCurrentConfigurationSlotB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  fake_svc_.fake_boot_args().SetArgResponse("-b");

  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryCurrentConfiguration();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kB);
}

TEST_F(PaverServiceSkipBlockTest, QueryCurrentConfigurationSlotR) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  fake_svc_.fake_boot_args().SetArgResponse("-r");

  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryCurrentConfiguration();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kRecovery);
}

TEST_F(PaverServiceSkipBlockTest, QueryCurrentConfigurationSlotInvalid) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  fake_svc_.fake_boot_args().SetArgResponse("");

  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryCurrentConfiguration();
  ASSERT_STATUS(result, ZX_ERR_PEER_CLOSED);
}

TEST_F(PaverServiceSkipBlockTest, QueryConfigurationStatusHealthy) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  auto abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryConfigurationStatus(fuchsia_paver::wire::Configuration::kB);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->status, fuchsia_paver::wire::ConfigurationStatus::kHealthy);
}

TEST_F(PaverServiceSkipBlockTest, QueryConfigurationStatusPending) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  abr_data.slot_data[1].successful_boot = 0;
  abr_data.slot_data[1].tries_remaining = 1;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryConfigurationStatus(fuchsia_paver::wire::Configuration::kB);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->status, fuchsia_paver::wire::ConfigurationStatus::kPending);
}

TEST_F(PaverServiceSkipBlockTest, QueryConfigurationStatusUnbootable) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->QueryConfigurationStatus(fuchsia_paver::wire::Configuration::kA);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->status, fuchsia_paver::wire::ConfigurationStatus::kUnbootable);
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationActive) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  abr_data.slot_data[0].priority = kAbrMaxPriority;
  abr_data.slot_data[0].tries_remaining = kAbrMaxTriesRemaining;
  abr_data.slot_data[0].successful_boot = 0;
  ComputeCrc(&abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationActive(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationActiveRollover) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  abr_data.slot_data[1].priority = kAbrMaxPriority;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  abr_data.slot_data[1].priority = kAbrMaxPriority - 1;
  abr_data.slot_data[0].priority = kAbrMaxPriority;
  abr_data.slot_data[0].tries_remaining = kAbrMaxTriesRemaining;
  abr_data.slot_data[0].successful_boot = 0;
  ComputeCrc(&abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationActive(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }
  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationUnbootableSlotA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].priority = 2;
  abr_data.slot_data[0].tries_remaining = 3;
  abr_data.slot_data[0].successful_boot = 0;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  abr_data.slot_data[0].tries_remaining = 0;
  abr_data.slot_data[0].successful_boot = 0;
  ComputeCrc(&abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationUnbootable(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationUnbootableSlotB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  abr_data.slot_data[1].tries_remaining = 3;
  abr_data.slot_data[1].successful_boot = 0;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  abr_data.slot_data[1].tries_remaining = 0;
  abr_data.slot_data[1].successful_boot = 0;
  ComputeCrc(&abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationUnbootable(fuchsia_paver::wire::Configuration::kB);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationHealthySlotA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].priority = kAbrMaxPriority;
  abr_data.slot_data[0].tries_remaining = 0;
  abr_data.slot_data[0].successful_boot = 1;
  abr_data.slot_data[1].priority = 0;
  abr_data.slot_data[1].tries_remaining = 0;
  abr_data.slot_data[1].successful_boot = 0;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationHealthySlotB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ComputeCrc(&abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kB);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationHealthySlotR) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result =
      boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kRecovery);
  ASSERT_OK(result.status());
  ASSERT_EQ(result.value().status, ZX_ERR_INVALID_ARGS);
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationHealthyBothUnknown) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].priority = kAbrMaxPriority;
  abr_data.slot_data[0].tries_remaining = 3;
  abr_data.slot_data[0].successful_boot = 0;
  abr_data.slot_data[1].priority = kAbrMaxPriority - 1;
  abr_data.slot_data[1].tries_remaining = 3;
  abr_data.slot_data[1].successful_boot = 0;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  abr_data.slot_data[0].tries_remaining = 0;
  abr_data.slot_data[0].successful_boot = 1;
  abr_data.slot_data[1].tries_remaining = kAbrMaxTriesRemaining;
  ComputeCrc(&abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetConfigurationHealthyOtherHealthy) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].priority = kAbrMaxPriority - 1;
  abr_data.slot_data[0].tries_remaining = 0;
  abr_data.slot_data[0].successful_boot = 1;
  abr_data.slot_data[1].priority = kAbrMaxPriority;
  abr_data.slot_data[1].tries_remaining = 3;
  abr_data.slot_data[1].successful_boot = 0;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  abr_data.slot_data[0].tries_remaining = kAbrMaxTriesRemaining;
  abr_data.slot_data[0].successful_boot = 0;
  abr_data.slot_data[1].tries_remaining = 0;
  abr_data.slot_data[1].successful_boot = 1;
  ComputeCrc(&abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kB);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

TEST_F(PaverServiceSkipBlockTest, SetUnbootableConfigurationHealthy) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  auto result = boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kA);
  ASSERT_OK(result.status());
  ASSERT_EQ(result.value().status, ZX_ERR_INVALID_ARGS);
}

TEST_F(PaverServiceSkipBlockTest, BootManagerBuffered) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  AbrData abr_data = kAbrData;
  // Successful slot b, active slot a. Like what happen after a reboot following an OTA.
  abr_data.slot_data[0].tries_remaining = 3;
  abr_data.slot_data[0].successful_boot = 0;
  abr_data.slot_data[0].priority = 1;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->QueryActiveConfiguration();
    ASSERT_OK(result.status());
    ASSERT_TRUE(result->is_ok());
    ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kA);
  }

  {
    auto result = boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = boot_manager_->SetConfigurationUnbootable(fuchsia_paver::wire::Configuration::kB);
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  // haven't flushed yet, storage shall stay the same.
  auto abr = GetAbr();
  ASSERT_BYTES_EQ(&abr, &abr_data, sizeof(abr));

  {
    auto result = boot_manager_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  abr_data.slot_data[0].tries_remaining = 0;
  abr_data.slot_data[0].successful_boot = 1;
  abr_data.slot_data[1].tries_remaining = 0;
  abr_data.slot_data[1].successful_boot = 0;
  ComputeCrc(&abr_data);

  abr = GetAbr();
  ASSERT_BYTES_EQ(&abr, &abr_data, sizeof(abr));
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetKernelConfigA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(2) * kPagesPerBlock, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kA,
                                       fuchsia_paver::wire::Asset::kKernel, std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);
  ValidateWritten(8, 2);
  ValidateUnwritten(10, 4);
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetKernelConfigB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(2) * kPagesPerBlock, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kB,
                                       fuchsia_paver::wire::Asset::kKernel, std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);
  ValidateUnwritten(8, 2);
  ValidateWritten(10, 2);
  ValidateUnwritten(12, 2);
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetKernelConfigRecovery) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(2) * kPagesPerBlock, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kRecovery,
                                       fuchsia_paver::wire::Asset::kKernel, std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);
  ValidateUnwritten(8, 4);
  ValidateWritten(12, 2);
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetVbMetaConfigA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(32, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result =
      data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kA,
                             fuchsia_paver::wire::Asset::kVerifiedBootMetadata, std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  auto sync_result = data_sink_->Flush();
  ASSERT_OK(sync_result.status());
  ASSERT_OK(sync_result.value().status);

  ValidateWrittenPages(14 * kPagesPerBlock + 32, 32);
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetVbMetaConfigB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(32, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result =
      data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kB,
                             fuchsia_paver::wire::Asset::kVerifiedBootMetadata, std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  auto sync_result = data_sink_->Flush();
  ASSERT_OK(sync_result.status());
  ASSERT_OK(sync_result.value().status);

  ValidateWrittenPages(14 * kPagesPerBlock + 64, 32);
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetVbMetaConfigRecovery) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(32, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result =
      data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kRecovery,
                             fuchsia_paver::wire::Asset::kVerifiedBootMetadata, std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  auto sync_result = data_sink_->Flush();
  ASSERT_OK(sync_result.status());
  ASSERT_OK(sync_result.value().status);

  ValidateWrittenPages(14 * kPagesPerBlock + 96, 32);
}

TEST_F(PaverServiceSkipBlockTest, AbrWearLevelingLayoutNotUpdated) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  // Enable write-caching + abr metadata wear-leveling
  fake_svc_.fake_boot_args().SetAstroSysConfigAbrWearLeveling(true);

  // Active slot b
  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].tries_remaining = 3;
  abr_data.slot_data[0].successful_boot = 0;
  abr_data.slot_data[0].priority = 0;
  abr_data.slot_data[1].tries_remaining = 3;
  abr_data.slot_data[1].successful_boot = 0;
  abr_data.slot_data[1].priority = 1;
  ComputeCrc(&abr_data);
  SetAbr(abr_data);

  // Layout will not be updated as A/B state does not meet requirement.
  // (one successful slot + one unbootable slot)
  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->QueryActiveConfiguration();
    ASSERT_OK(result.status());
    ASSERT_TRUE(result->is_ok());
    ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kB);
  }

  {
    auto result = boot_manager_->SetConfigurationHealthy(fuchsia_paver::wire::Configuration::kB);
    ASSERT_OK(result.status());
  }

  {
    // The query result will come from the cache as flushed is not called.
    // Validate that it is correct.
    auto result = boot_manager_->QueryActiveConfiguration();
    ASSERT_OK(result.status());
    ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kB);
  }

  {
    // Mark the old slot A as unbootable.
    auto set_unbootable_result =
        boot_manager_->SetConfigurationUnbootable(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(set_unbootable_result.status());
  }

  // Haven't flushed yet. abr data in storage should stayed the same.
  auto actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));

  {
    auto result_sync = boot_manager_->Flush();
    ASSERT_OK(result_sync.status());
    ASSERT_OK(result_sync.value().status);
  }

  // Expected result: unbootable slot a, successful active slot b
  abr_data.slot_data[0].tries_remaining = 0;
  abr_data.slot_data[0].successful_boot = 0;
  abr_data.slot_data[0].priority = 0;
  abr_data.slot_data[1].tries_remaining = 0;
  abr_data.slot_data[1].successful_boot = 1;
  abr_data.slot_data[1].priority = 1;
  ComputeCrc(&abr_data);

  // Validate that new abr data is flushed to memory.
  // Since layout is not updated, Abr metadata is expected to be at the traditional position
  // (16th page).
  actual = GetAbr();
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));
}

AbrData GetAbrWearlevelingSupportingLayout() {
  // Unbootable slot a, successful active slot b
  AbrData abr_data = kAbrData;
  abr_data.slot_data[0].tries_remaining = 0;
  abr_data.slot_data[0].successful_boot = 0;
  abr_data.slot_data[0].priority = 0;
  abr_data.slot_data[1].tries_remaining = 0;
  abr_data.slot_data[1].successful_boot = 1;
  abr_data.slot_data[1].priority = 1;
  ComputeCrc(&abr_data);
  return abr_data;
}

TEST_F(PaverServiceSkipBlockTest, AbrWearLevelingLayoutUpdated) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  // Enable write-caching + abr metadata wear-leveling
  fake_svc_.fake_boot_args().SetAstroSysConfigAbrWearLeveling(true);

  // Unbootable slot a, successful active slot b
  auto abr_data = GetAbrWearlevelingSupportingLayout();
  SetAbr(abr_data);

  // Layout will be updated. Since A/B state is one successful + one unbootable
  ASSERT_NO_FATAL_FAILURE(FindBootManager());

  {
    auto result = boot_manager_->QueryActiveConfiguration();
    ASSERT_OK(result.status());
    ASSERT_TRUE(result->is_ok());
    ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kB);
  }

  {
    auto result = boot_manager_->SetConfigurationActive(fuchsia_paver::wire::Configuration::kA);
    ASSERT_OK(result.status());
  }

  {
    // The query result will come from the cache as we haven't flushed.
    // Validate that it is correct.
    auto result = boot_manager_->QueryActiveConfiguration();
    ASSERT_OK(result.status());
    ASSERT_EQ(result->value()->configuration, fuchsia_paver::wire::Configuration::kA);
  }

  // Haven't flushed yet. abr data in storage should stayed the same.
  // Since layout changed, use the updated layout to find abr.
  auto header = sysconfig::SyncClientAbrWearLeveling::GetAbrWearLevelingSupportedLayout();
  auto actual = GetAbrInWearLeveling(header, 0);
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));

  {
    auto result_sync = boot_manager_->Flush();
    ASSERT_OK(result_sync.status());
    ASSERT_OK(result_sync.value().status);
  }

  // Expected result: successful slot a, active slot b with max tries and priority.
  abr_data.slot_data[0].tries_remaining = kAbrMaxTriesRemaining;
  abr_data.slot_data[0].successful_boot = 0;
  abr_data.slot_data[0].priority = kAbrMaxPriority;
  abr_data.slot_data[1].tries_remaining = 0;
  abr_data.slot_data[1].successful_boot = 1;
  abr_data.slot_data[1].priority = 1;
  ComputeCrc(&abr_data);

  // Validate that new abr data is flushed to memory.
  // The first page (page 0) in the abr sub-partition is occupied by the initial abr data.
  // Thus, the new abr metadata is expected to be appended at the 2nd page (page 1).
  actual = GetAbrInWearLeveling(header, 1);
  ASSERT_BYTES_EQ(&abr_data, &actual, sizeof(abr_data));

  // Validate that header is updated.
  const sysconfig_header actual_header = GetSysconfigHeader();
  ASSERT_BYTES_EQ(&header, &actual_header, sizeof(sysconfig_header));
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetBuffered) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  fuchsia_paver::wire::Configuration configs[] = {fuchsia_paver::wire::Configuration::kA,
                                                  fuchsia_paver::wire::Configuration::kB,
                                                  fuchsia_paver::wire::Configuration::kRecovery};

  for (auto config : configs) {
    fuchsia_mem::wire::Buffer payload;
    CreatePayload(32, &payload);
    auto result = data_sink_->WriteAsset(config, fuchsia_paver::wire::Asset::kVerifiedBootMetadata,
                                         std::move(payload));
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }
  ValidateUnwrittenPages(14 * kPagesPerBlock + 32, 96);

  auto sync_result = data_sink_->Flush();
  ASSERT_OK(sync_result.status());
  ASSERT_OK(sync_result.value().status);
  ValidateWrittenPages(14 * kPagesPerBlock + 32, 96);
}

TEST_F(PaverServiceSkipBlockTest, WriteAssetTwice) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(2) * kPagesPerBlock, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  {
    auto result = data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kA,
                                         fuchsia_paver::wire::Asset::kKernel, std::move(payload));
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    CreatePayload(static_cast<size_t>(2) * kPagesPerBlock, &payload);
    ValidateWritten(8, 2);
    ValidateUnwritten(10, 4);
  }
  {
    auto result = data_sink_->WriteAsset(fuchsia_paver::wire::Configuration::kA,
                                         fuchsia_paver::wire::Asset::kKernel, std::move(payload));
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ValidateWritten(8, 2);
    ValidateUnwritten(10, 4);
  }
}

TEST_F(PaverServiceSkipBlockTest, ReadFirmwareConfigA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(kBootloaderFirstBlock * kPagesPerBlock,
            static_cast<size_t>(kBootloaderBlocks) * kPagesPerBlock, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadFirmware(fuchsia_paver::wire::Configuration::kA,
                                         fidl::StringView::FromExternal(kFirmwareTypeBootloader));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().is_ok());
  ValidateWritten(result.value().value()->firmware,
                  static_cast<size_t>(kBootloaderBlocks) * kPagesPerBlock);
}

TEST_F(PaverServiceSkipBlockTest, ReadFirmwareUnsupportedConfigBFallBackToA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(kBootloaderFirstBlock * kPagesPerBlock,
            static_cast<size_t>(kBootloaderBlocks) * kPagesPerBlock, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadFirmware(fuchsia_paver::wire::Configuration::kB,
                                         fidl::StringView::FromExternal(kFirmwareTypeBootloader));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().is_ok());
  ValidateWritten(result.value().value()->firmware,
                  static_cast<size_t>(kBootloaderBlocks) * kPagesPerBlock);
}

TEST_F(PaverServiceSkipBlockTest, ReadFirmwareUnsupportedConfigR) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadFirmware(fuchsia_paver::wire::Configuration::kRecovery,
                                         fidl::StringView::FromExternal(kFirmwareTypeBootloader));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().is_error());
}

TEST_F(PaverServiceSkipBlockTest, ReadFirmwareUnsupportedType) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadFirmware(fuchsia_paver::wire::Configuration::kA,
                                         fidl::StringView::FromExternal(kFirmwareTypeUnsupported));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().is_error());
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareConfigASupported) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(4) * kPagesPerBlock, &payload);
  auto result = data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kA,
                                          fidl::StringView::FromExternal(kFirmwareTypeBootloader),
                                          std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_status());
  ASSERT_OK(result.value().result.status());
  ValidateWritten(kBootloaderFirstBlock, 4);
  WriteData(kBootloaderFirstBlock, static_cast<size_t>(4) * kPagesPerBlock, 0xff);
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareUnsupportedConfigBFallBackToA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(4) * kPagesPerBlock, &payload);
  auto result = data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kB,
                                          fidl::StringView::FromExternal(kFirmwareTypeBootloader),
                                          std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_status());
  ASSERT_OK(result.value().result.status());
  ValidateWritten(kBootloaderFirstBlock, 4);
  WriteData(kBootloaderFirstBlock, static_cast<size_t>(4) * kPagesPerBlock, 0xff);
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareUnsupportedConfigR) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(4) * kPagesPerBlock, &payload);
  auto result = data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kRecovery,
                                          fidl::StringView::FromExternal(kFirmwareTypeBootloader),
                                          std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_unsupported());
  ASSERT_TRUE(result.value().result.unsupported());
  ValidateUnwritten(kBootloaderFirstBlock, 4);
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareBl2ConfigASupported) {
  // BL2 special handling: we should always leave the first 4096 bytes intact.
  constexpr size_t kBl2StartByte{static_cast<size_t>(kBl2FirstBlock) * kPageSize * kPagesPerBlock};
  constexpr size_t kBl2SkipLength{4096};

  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  ASSERT_NO_FATAL_FAILURE(FindDataSink());

  WriteDataBytes(kBl2StartByte, kBl2SkipLength, 0xC6);
  fuchsia_mem::wire::Buffer payload;
  CreatePayload(kBl2ImagePages, &payload);
  auto result = data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kA,
                                          fidl::StringView::FromExternal(kFirmwareTypeBl2),
                                          std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_status());
  ASSERT_OK(result.value().result.status());
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareBl2UnsupportedConfigBFallBackToA) {
  // BL2 special handling: we should always leave the first 4096 bytes intact.
  constexpr size_t kBl2StartByte{static_cast<size_t>(kBl2FirstBlock) * kPageSize * kPagesPerBlock};
  constexpr size_t kBl2SkipLength{4096};

  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  WriteDataBytes(kBl2StartByte, kBl2SkipLength, 0xC6);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  fuchsia_mem::wire::Buffer payload;
  CreatePayload(kBl2ImagePages, &payload);
  auto result = data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kB,
                                          fidl::StringView::FromExternal(kFirmwareTypeBl2),
                                          std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_status());
  ASSERT_OK(result.value().result.status());
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareBl2UnsupportedConfigR) {
  // BL2 special handling: we should always leave the first 4096 bytes intact.
  constexpr size_t kBl2StartByte{static_cast<size_t>(kBl2FirstBlock) * kPageSize * kPagesPerBlock};
  constexpr size_t kBl2SkipLength{4096};

  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());
  WriteDataBytes(kBl2StartByte, kBl2SkipLength, 0xC6);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  fuchsia_mem::wire::Buffer payload;
  CreatePayload(kBl2ImagePages, &payload);
  auto result = data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kRecovery,
                                          fidl::StringView::FromExternal(kFirmwareTypeBl2),
                                          std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_unsupported());
  ASSERT_TRUE(result.value().result.unsupported());
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareUnsupportedType) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  constexpr fuchsia_paver::wire::Configuration kAllConfigs[] = {
      fuchsia_paver::wire::Configuration::kA,
      fuchsia_paver::wire::Configuration::kB,
      fuchsia_paver::wire::Configuration::kRecovery,
  };

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  for (auto config : kAllConfigs) {
    fuchsia_mem::wire::Buffer payload;
    CreatePayload(static_cast<size_t>(4) * kPagesPerBlock, &payload);
    auto result = data_sink_->WriteFirmware(
        config, fidl::StringView::FromExternal(kFirmwareTypeUnsupported), std::move(payload));
    ASSERT_OK(result.status());
    ASSERT_TRUE(result.value().result.is_unsupported());
    ASSERT_TRUE(result.value().result.unsupported());
    ValidateUnwritten(kBootloaderFirstBlock, 4);
    ValidateUnwritten(kBl2FirstBlock, 1);
  }
}

TEST_F(PaverServiceSkipBlockTest, WriteFirmwareError) {
  // Make a RAM NAND device without a visible "bootloader" partition so that
  // the partitioner initializes properly but then fails when trying to find it.
  fuchsia_hardware_nand::wire::RamNandInfo info = NandInfo();
  info.partition_map.partitions[1].hidden = true;
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand(std::move(info)));

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(4) * kPagesPerBlock, &payload);
  auto result = data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kA,
                                          fidl::StringView::FromExternal(kFirmwareTypeBootloader),
                                          std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_status());
  ASSERT_NOT_OK(result.value().result.status());
  ValidateUnwritten(kBootloaderFirstBlock, 4);
}

TEST_F(PaverServiceSkipBlockTest, ReadAssetKernelConfigA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(kZirconAFirstBlock * kPagesPerBlock, static_cast<size_t>(2) * kPagesPerBlock, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadAsset(fuchsia_paver::wire::Configuration::kA,
                                      fuchsia_paver::wire::Asset::kKernel);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ValidateWritten(result->value()->asset, static_cast<size_t>(2) * kPagesPerBlock);
}

TEST_F(PaverServiceSkipBlockTest, ReadAssetKernelConfigB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(10 * kPagesPerBlock, static_cast<size_t>(2) * kPagesPerBlock, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadAsset(fuchsia_paver::wire::Configuration::kB,
                                      fuchsia_paver::wire::Asset::kKernel);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ValidateWritten(result->value()->asset, static_cast<size_t>(2) * kPagesPerBlock);
}

TEST_F(PaverServiceSkipBlockTest, ReadAssetKernelConfigRecovery) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(12 * kPagesPerBlock, static_cast<size_t>(2) * kPagesPerBlock, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadAsset(fuchsia_paver::wire::Configuration::kRecovery,
                                      fuchsia_paver::wire::Asset::kKernel);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ValidateWritten(result->value()->asset, static_cast<size_t>(2) * kPagesPerBlock);
}

TEST_F(PaverServiceSkipBlockTest, ReadAssetVbMetaConfigA) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(14 * kPagesPerBlock + 32, 32, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadAsset(fuchsia_paver::wire::Configuration::kA,
                                      fuchsia_paver::wire::Asset::kVerifiedBootMetadata);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ValidateWritten(result->value()->asset, 32);
}

TEST_F(PaverServiceSkipBlockTest, ReadAssetVbMetaConfigB) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(14 * kPagesPerBlock + 64, 32, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadAsset(fuchsia_paver::wire::Configuration::kB,
                                      fuchsia_paver::wire::Asset::kVerifiedBootMetadata);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ValidateWritten(result->value()->asset, 32);
}

TEST_F(PaverServiceSkipBlockTest, ReadAssetVbMetaConfigRecovery) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  WriteData(14 * kPagesPerBlock + 96, 32, 0x4a);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadAsset(fuchsia_paver::wire::Configuration::kRecovery,
                                      fuchsia_paver::wire::Asset::kVerifiedBootMetadata);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ValidateWritten(result->value()->asset, 32);
}

TEST_F(PaverServiceSkipBlockTest, ReadAssetZbi) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  zbi_header_t container;
  // Currently our ZBI checker only validates the container header so the data can be anything.
  uint8_t data[8] = {10, 20, 30, 40, 50, 60, 70, 80};
  container.type = ZBI_TYPE_CONTAINER;
  container.extra = ZBI_CONTAINER_MAGIC;
  container.magic = ZBI_ITEM_MAGIC;
  container.flags = ZBI_FLAGS_VERSION;
  container.crc32 = ZBI_ITEM_NO_CRC32;
  container.length = sizeof(data);  // Contents size only, does not include header size.

  constexpr uint32_t kZirconAStartByte = kZirconAFirstBlock * kPagesPerBlock * kPageSize;
  WriteDataBytes(kZirconAStartByte, &container, sizeof(container));
  WriteDataBytes(kZirconAStartByte + sizeof(container), &data, sizeof(data));

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result = data_sink_->ReadAsset(fuchsia_paver::wire::Configuration::kA,
                                      fuchsia_paver::wire::Asset::kKernel);
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->asset.size, sizeof(container) + sizeof(data));

  fzl::VmoMapper mapper;
  ASSERT_OK(
      mapper.Map(result->value()->asset.vmo, 0, result->value()->asset.size, ZX_VM_PERM_READ));
  const uint8_t* read_data = static_cast<const uint8_t*>(mapper.start());
  ASSERT_EQ(0, memcmp(read_data, &container, sizeof(container)));
  ASSERT_EQ(0, memcmp(read_data + sizeof(container), &data, sizeof(data)));
}

TEST_F(PaverServiceSkipBlockTest, WriteBootloader) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(4) * kPagesPerBlock, &payload);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result =
      data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kA, "", std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().result.status());
  ValidateWritten(4, 4);
}

// We prefill the bootloader partition with the expected data, leaving the last block as 0xFF.
// Normally the last page would be overwritten with 0s, but because the actual payload is identical,
// we don't actually pave the image, so the extra page stays as 0xFF.
TEST_F(PaverServiceSkipBlockTest, WriteBootloaderNotAligned) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  fuchsia_mem::wire::Buffer payload;
  CreatePayload(static_cast<size_t>(4) * kPagesPerBlock - 1, &payload);

  WriteData(4 * kPagesPerBlock, static_cast<size_t>(4) * kPagesPerBlock - 1, 0x4a);
  WriteData(8 * kPagesPerBlock - 1, 1, 0xff);

  ASSERT_NO_FATAL_FAILURE(FindDataSink());
  auto result =
      data_sink_->WriteFirmware(fuchsia_paver::wire::Configuration::kA, "", std::move(payload));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().result.status());
  ValidateWrittenPages(4 * kPagesPerBlock, static_cast<size_t>(4) * kPagesPerBlock - 1);
  ValidateUnwrittenPages(8 * kPagesPerBlock - 1, 1);
}

TEST_F(PaverServiceSkipBlockTest, WriteVolumes) {
  // TODO(https://fxbug.dev/42109028): Figure out a way to test this.
}

void PaverServiceSkipBlockTest::TestSysconfigWriteBufferedClient(uint32_t offset_in_pages,
                                                                 uint32_t sysconfig_pages) {
  {
    auto result = sysconfig_->GetPartitionSize();
    ASSERT_OK(result.status());
    ASSERT_TRUE(result->is_ok());
    ASSERT_EQ(result->value()->size, sysconfig_pages * kPageSize);
  }

  {
    fuchsia_mem::wire::Buffer payload;
    CreatePayload(sysconfig_pages, &payload);
    auto result = sysconfig_->Write(std::move(payload));
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    // Without flushing, data in the storage should remain unchanged.
    ASSERT_NO_FATAL_FAILURE(
        ValidateUnwrittenPages(14 * kPagesPerBlock + offset_in_pages, sysconfig_pages));
  }

  {
    auto result = sysconfig_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_NO_FATAL_FAILURE(
        ValidateWrittenPages(14 * kPagesPerBlock + offset_in_pages, sysconfig_pages));
  }

  {
    // Validate read.
    auto result = sysconfig_->Read();
    ASSERT_OK(result.status());
    ASSERT_TRUE(result->is_ok());
    ASSERT_NO_FATAL_FAILURE(ValidateWritten(result->value()->data, sysconfig_pages));
  }
}

TEST_F(PaverServiceSkipBlockTest, SysconfigWriteWithBufferredClientLayoutNotUpdated) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  // Enable write-caching + abr metadata wear-leveling
  fake_svc_.fake_boot_args().SetAstroSysConfigAbrWearLeveling(true);

  ASSERT_NO_FATAL_FAILURE(FindSysconfig());

  ASSERT_NO_FATAL_FAILURE(TestSysconfigWriteBufferedClient(0, 15 * 2));
}

TEST_F(PaverServiceSkipBlockTest, SysconfigWriteWithBufferredClientLayoutUpdated) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  // Enable write-caching + abr metadata wear-leveling
  fake_svc_.fake_boot_args().SetAstroSysConfigAbrWearLeveling(true);

  auto abr_data = GetAbrWearlevelingSupportingLayout();
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindSysconfig());

  ASSERT_NO_FATAL_FAILURE(TestSysconfigWriteBufferedClient(2, 5 * 2));
}

void PaverServiceSkipBlockTest::TestSysconfigWipeBufferedClient(uint32_t offset_in_pages,
                                                                uint32_t sysconfig_pages) {
  {
    auto result = sysconfig_->Wipe();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    // Without flushing, data in the storage should remain unchanged.
    ASSERT_NO_FATAL_FAILURE(
        ValidateUnwrittenPages(14 * kPagesPerBlock + offset_in_pages, sysconfig_pages));
  }

  {
    auto result = sysconfig_->Flush();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_NO_FATAL_FAILURE(AssertContents(
        static_cast<size_t>(14) * kSkipBlockSize + offset_in_pages * static_cast<size_t>(kPageSize),
        sysconfig_pages * static_cast<size_t>(kPageSize), 0));
  }
}

TEST_F(PaverServiceSkipBlockTest, SysconfigWipeWithBufferredClientLayoutNotUpdated) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  // Enable write-caching + abr metadata wear-leveling
  fake_svc_.fake_boot_args().SetAstroSysConfigAbrWearLeveling(true);

  ASSERT_NO_FATAL_FAILURE(FindSysconfig());

  ASSERT_NO_FATAL_FAILURE(TestSysconfigWipeBufferedClient(0, 15 * 2));
}

TEST_F(PaverServiceSkipBlockTest, SysconfigWipeWithBufferredClientLayoutUpdated) {
  ASSERT_NO_FATAL_FAILURE(InitializeRamNand());

  // Enable write-caching + abr metadata wear-leveling
  fake_svc_.fake_boot_args().SetAstroSysConfigAbrWearLeveling(true);

  auto abr_data = GetAbrWearlevelingSupportingLayout();
  SetAbr(abr_data);

  ASSERT_NO_FATAL_FAILURE(FindSysconfig());

  ASSERT_NO_FATAL_FAILURE(TestSysconfigWipeBufferedClient(2, 5 * 2));
}

constexpr uint8_t kEmptyType[GPT_GUID_LEN] = GUID_EMPTY_VALUE;

#if defined(__x86_64__)
class PaverServiceBlockTest : public PaverServiceTest {
 public:
  PaverServiceBlockTest() { ASSERT_NO_FATAL_FAILURE(SpawnIsolatedDevmgr()); }

 protected:
  void SpawnIsolatedDevmgr() {
    driver_integration_test::IsolatedDevmgr::Args args;
    args.disable_block_watcher = false;

    ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr_));

    // Forward the block watcher FIDL interface from the devmgr.
    fake_svc_.ForwardServiceTo(fidl::DiscoverableProtocolName<fuchsia_fshost::BlockWatcher>,
                               devmgr_.fshost_svc_dir());

    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform/00:00:2d/ramctl")
                  .status_value());
    paver_->set_devfs_root(devmgr_.devfs_root().duplicate());
    paver_->set_svc_root(std::move(fake_svc_.svc_chan()));
  }

  void UseBlockDevice(DeviceAndController block_device) {
    auto [local, remote] = fidl::Endpoints<fuchsia_paver::DynamicDataSink>::Create();

    auto result = client_->UseBlockDevice(
        fidl::ClientEnd<fuchsia_hardware_block::Block>(std::move(block_device.device)),
        std::move(block_device.controller), std::move(remote));
    ASSERT_OK(result.status());
    data_sink_ = fidl::WireSyncClient(std::move(local));
  }

  IsolatedDevmgr devmgr_;
  fidl::WireSyncClient<fuchsia_paver::DynamicDataSink> data_sink_;
};

TEST_F(PaverServiceBlockTest, DISABLED_InitializePartitionTables) {
  std::unique_ptr<BlockDevice> gpt_dev;
  // 32GiB disk.
  constexpr uint64_t block_count = (32LU << 30) / kBlockSize;
  ASSERT_NO_FATAL_FAILURE(
      BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, block_count, &gpt_dev));

  zx::result connections = GetNewConnections(gpt_dev->block_controller_interface());
  ASSERT_OK(connections);
  ASSERT_NO_FATAL_FAILURE(UseBlockDevice(std::move(connections.value())));

  auto result = data_sink_->InitializePartitionTables();
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);
}

TEST_F(PaverServiceBlockTest, DISABLED_InitializePartitionTablesMultipleDevices) {
  std::unique_ptr<BlockDevice> gpt_dev1, gpt_dev2;
  // 32GiB disk.
  constexpr uint64_t block_count = (32LU << 30) / kBlockSize;
  ASSERT_NO_FATAL_FAILURE(
      BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, block_count, &gpt_dev1));
  ASSERT_NO_FATAL_FAILURE(
      BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, block_count, &gpt_dev2));

  zx::result connections = GetNewConnections(gpt_dev1->block_controller_interface());
  ASSERT_OK(connections);
  ASSERT_NO_FATAL_FAILURE(UseBlockDevice(std::move(connections.value())));

  auto result = data_sink_->InitializePartitionTables();
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);
}

TEST_F(PaverServiceBlockTest, DISABLED_WipePartitionTables) {
  std::unique_ptr<BlockDevice> gpt_dev;
  // 32GiB disk.
  constexpr uint64_t block_count = (32LU << 30) / kBlockSize;
  ASSERT_NO_FATAL_FAILURE(
      BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, block_count, &gpt_dev));

  zx::result connections = GetNewConnections(gpt_dev->block_controller_interface());
  ASSERT_OK(connections);
  ASSERT_NO_FATAL_FAILURE(UseBlockDevice(std::move(connections.value())));
  auto result = data_sink_->InitializePartitionTables();
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  auto wipe_result = data_sink_->WipePartitionTables();
  ASSERT_OK(wipe_result.status());
  ASSERT_OK(wipe_result.value().status);
}

#endif

class PaverServiceGptDeviceTest : public PaverServiceTest {
 protected:
  void SpawnIsolatedDevmgr(const char* board_name) {
    driver_integration_test::IsolatedDevmgr::Args args;
    args.disable_block_watcher = false;

    args.board_name = board_name;
    ASSERT_OK(driver_integration_test::IsolatedDevmgr::Create(&args, &devmgr_));

    // Forward the block watcher FIDL interface from the devmgr.
    fake_svc_.ForwardServiceTo(fidl::DiscoverableProtocolName<fuchsia_fshost::BlockWatcher>,
                               devmgr_.fshost_svc_dir());

    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform/00:00:2d/ramctl")
                  .status_value());
    ASSERT_OK(RecursiveWaitForFile(devmgr_.devfs_root().get(), "sys/platform").status_value());
    paver_->set_dispatcher(loop_.dispatcher());
    paver_->set_devfs_root(devmgr_.devfs_root().duplicate());
    fidl::ClientEnd<fuchsia_io::Directory> svc_root = GetSvcRoot();
    paver_->set_svc_root(std::move(svc_root));
  }

  void InitializeGptDevice(const char* board_name, uint64_t block_count, uint32_t block_size) {
    SpawnIsolatedDevmgr(board_name);
    block_count_ = block_count;
    block_size_ = block_size;
    ASSERT_NO_FATAL_FAILURE(
        BlockDevice::Create(devmgr_.devfs_root(), kEmptyType, block_count, block_size, &gpt_dev_));
  }

  fidl::ClientEnd<fuchsia_io::Directory> GetSvcRoot() {
    return component::MaybeClone(fake_svc_.svc_chan());
  }

  struct PartitionDescription {
    const char* name;
    const uint8_t* type;
    uint64_t start;
    uint64_t length;
  };

  void InitializeStartingGPTPartitions(const std::vector<PartitionDescription>& init_partitions) {
    InitializeStartingGPTPartitions(gpt_dev_.get(), init_partitions);
  }

  void InitializeStartingGPTPartitions(const BlockDevice* gpt_dev,
                                       const std::vector<PartitionDescription>& init_partitions) {
    // Pause the block watcher while we write partitions to the disk.
    // This is to avoid the block watcher seeing an intermediate state of the partition table
    // and incorrectly treating it as an MBR.
    // The watcher is automatically resumed when this goes out of scope.
    auto pauser = paver::BlockWatcherPauser::Create(GetSvcRoot());
    ASSERT_OK(pauser);

    zx::result new_connection_result = GetNewConnections(gpt_dev->block_controller_interface());
    ASSERT_OK(new_connection_result);
    DeviceAndController& new_connection = new_connection_result.value();

    fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> volume(std::move(new_connection.device));
    zx::result remote_device = block_client::RemoteBlockDevice::Create(
        std::move(volume), std::move(new_connection.controller));
    ASSERT_OK(remote_device);
    zx::result gpt_result = gpt::GptDevice::Create(std::move(remote_device.value()),
                                                   gpt_dev->block_size(), gpt_dev->block_count());
    ASSERT_OK(gpt_result);
    gpt::GptDevice& gpt = *gpt_result.value();
    ASSERT_OK(gpt.Sync());

    for (const auto& part : init_partitions) {
      ASSERT_OK(gpt.AddPartition(part.name, part.type, GetRandomGuid(), part.start, part.length, 0),
                "%s", part.name);
    }

    ASSERT_OK(gpt.Sync());

    fidl::WireResult result =
        fidl::WireCall(gpt_dev->block_controller_interface())->Rebind(fidl::StringView("gpt.cm"));
    ASSERT_TRUE(result.ok(), "%s", result.FormatDescription().c_str());
    ASSERT_TRUE(result->is_ok(), "%s", zx_status_get_string(result->error_value()));
  }

  uint8_t* GetRandomGuid() {
    static uint8_t random_guid[GPT_GUID_LEN];
    zx_cprng_draw(random_guid, GPT_GUID_LEN);
    return random_guid;
  }

  driver_integration_test::IsolatedDevmgr devmgr_;
  std::unique_ptr<BlockDevice> gpt_dev_;
  uint64_t block_count_;
  uint64_t block_size_;
};

class PaverServiceLuisTest : public PaverServiceGptDeviceTest {
 public:
  static constexpr size_t kDurableBootStart = 0x10400;
  static constexpr size_t kDurableBootSize = 0x10000;
  static constexpr size_t kFvmBlockStart = 0x20400;
  static constexpr size_t kFvmBlockSize = 0x10000;

  void SetUp() override { ASSERT_NO_FATAL_FAILURE(InitializeGptDevice("luis", 0x748034, 512)); }

  void InitializeLuisGPTPartitions() {
    constexpr uint8_t kDummyType[GPT_GUID_LEN] = {0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47,
                                                  0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4};
    const std::vector<PartitionDescription> kLuisStartingPartitions = {
        {GPT_DURABLE_BOOT_NAME, kDummyType, kDurableBootStart, kDurableBootSize},
        {GPT_FVM_NAME, kDummyType, kFvmBlockStart, kFvmBlockSize},
    };
    ASSERT_NO_FATAL_FAILURE(InitializeStartingGPTPartitions(kLuisStartingPartitions));
  }
};

TEST_F(PaverServiceLuisTest, CreateAbr) {
  ASSERT_NO_FATAL_FAILURE(InitializeLuisGPTPartitions());
  std::shared_ptr<paver::Context> context;
  fidl::ClientEnd<fuchsia_io::Directory> svc_root = GetSvcRoot();
  EXPECT_OK(abr::ClientFactory::Create(devmgr_.devfs_root().duplicate(), svc_root, context));
}

TEST_F(PaverServiceLuisTest, SysconfigNotSupportedAndFailWithPeerClosed) {
  ASSERT_NO_FATAL_FAILURE(InitializeLuisGPTPartitions());
  auto [local, remote] = fidl::Endpoints<fuchsia_paver::Sysconfig>::Create();
  auto result = client_->FindSysconfig(std::move(remote));
  ASSERT_OK(result.status());

  fidl::WireSyncClient sysconfig(std::move(local));
  auto wipe_result = sysconfig->Wipe();
  ASSERT_EQ(wipe_result.status(), ZX_ERR_PEER_CLOSED);
}

TEST_F(PaverServiceLuisTest, FindGPTDevicesIgnoreFvmPartitions) {
  // Initialize the primary block solely as FVM and allocate sub-partitions.
  fvm::SparseImage header = {};
  header.slice_size = 1 << 20;
  zx::result fvm = FvmPartitionFormat(devmgr_.devfs_root(), gpt_dev_->block_interface(),
                                      gpt_dev_->block_controller_interface(), header,
                                      paver::BindOption::Reformat);
  ASSERT_OK(fvm);

  auto [volume, volume_server] =
      fidl::Endpoints<fuchsia_hardware_block_volume::VolumeManager>::Create();
  ASSERT_OK(fidl::WireCall(fvm.value())->ConnectToDeviceFidl(volume_server.TakeChannel()).status());
  zx::result status = paver::AllocateEmptyPartitions(devmgr_.devfs_root(), volume);
  ASSERT_OK(status);

  // Check that FVM created sub-partitions are not considered as candidates.
  zx::result gpt_devices = paver::GptDevicePartitioner::FindGptDevices(devmgr_.devfs_root());
  ASSERT_OK(gpt_devices);
  ASSERT_EQ(gpt_devices.value().size(), 1);
  ASSERT_EQ(gpt_devices.value()[0].topological_path,
            std::string("/dev/sys/platform/00:00:2d/ramctl/ramdisk-0/block"));
}

TEST_F(PaverServiceLuisTest, WriteOpaqueVolume) {
  // TODO(b/217597389): Consdier also adding an e2e test for this interface.
  ASSERT_NO_FATAL_FAILURE(InitializeLuisGPTPartitions());
  auto [local, remote] = fidl::Endpoints<fuchsia_paver::DynamicDataSink>::Create();

  {
    zx::result connections = GetNewConnections(gpt_dev_->block_controller_interface());
    ASSERT_OK(connections);
    ASSERT_OK(client_->UseBlockDevice(
        fidl::ClientEnd<fuchsia_hardware_block::Block>(std::move(connections->device)),
        std::move(connections->controller), std::move(remote)));
  }
  fidl::WireSyncClient data_sink{std::move(local)};

  // Create a payload
  constexpr size_t kPayloadSize = 2048;
  std::vector<uint8_t> payload(kPayloadSize, 0x4a);

  fuchsia_mem::wire::Buffer payload_wire_buffer;
  zx::vmo payload_vmo;
  fzl::VmoMapper payload_vmo_mapper;
  ASSERT_OK(payload_vmo_mapper.CreateAndMap(kPayloadSize, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                                            nullptr, &payload_vmo));
  memcpy(payload_vmo_mapper.start(), payload.data(), kPayloadSize);
  payload_wire_buffer.vmo = std::move(payload_vmo);
  payload_wire_buffer.size = kPayloadSize;

  // Write the payload as opaque volume
  auto result = data_sink->WriteOpaqueVolume(std::move(payload_wire_buffer));
  ASSERT_OK(result.status());

  // Create a block partition client to read the written content directly.
  zx::result block_client =
      paver::BlockPartitionClient::Create(gpt_dev_->block_controller_interface());
  ASSERT_OK(block_client);

  // Read the partition directly from block and verify.
  zx::vmo block_read_vmo;
  fzl::VmoMapper block_read_vmo_mapper;
  ASSERT_OK(
      block_read_vmo_mapper.CreateAndMap(kPayloadSize, ZX_VM_PERM_READ, nullptr, &block_read_vmo));
  ASSERT_OK(block_client->Read(block_read_vmo, kPayloadSize, kFvmBlockStart, 0));

  // Verify the written data against the payload
  ASSERT_BYTES_EQ(block_read_vmo_mapper.start(), payload.data(), kPayloadSize);
}

struct SparseImageResult {
  std::vector<uint8_t> sparse;
  std::vector<uint32_t> raw_data;
  // image_length can be > raw_data.size(), simulating an image with sparse padding at the end.
  size_t image_length;
};

class Chunk {
 public:
  enum class ChunkType {
    kUnknown = 0,
    kRaw = CHUNK_TYPE_RAW,
    kFill = CHUNK_TYPE_FILL,
    kDontCare = CHUNK_TYPE_DONT_CARE,
    kCrc32 = CHUNK_TYPE_CRC32,
  };

  constexpr Chunk(ChunkType type, uint32_t payload, size_t output_blocks, size_t block_size)
      : type_(type),
        payload_(payload),
        output_blocks_(output_blocks),
        block_size_bytes_(block_size) {}

  constexpr chunk_header_t GenerateHeader() const {
    return chunk_header_t{
        .chunk_type = static_cast<uint16_t>(type_),
        .reserved1 = 0,
        .chunk_sz = static_cast<uint32_t>(output_blocks_),
        .total_sz = static_cast<uint32_t>(SizeInImage()),
    };
  }

  constexpr size_t SizeInImage() const {
    switch (type_) {
      case ChunkType::kRaw:
        return sizeof(chunk_header_t) + output_blocks_ * block_size_bytes_;
      case ChunkType::kCrc32:
      case ChunkType::kFill:
        return sizeof(chunk_header_t) + sizeof(payload_);
      case ChunkType::kUnknown:
      case ChunkType::kDontCare:
        return sizeof(chunk_header_t);
    }
  }

  constexpr size_t OutputSize() const {
    switch (type_) {
      case ChunkType::kRaw:
      case ChunkType::kFill:
      case ChunkType::kDontCare:
        return output_blocks_ * block_size_bytes_;
      case ChunkType::kUnknown:
      case ChunkType::kCrc32:
        return 0;
    }
  }

  constexpr size_t OutputBlocks() const { return output_blocks_; }

  void AppendImageBytes(std::vector<uint8_t>& sparse_image) const {
    chunk_header_t hdr = GenerateHeader();
    const uint8_t* hdr_bytes = reinterpret_cast<const uint8_t*>(&hdr);
    sparse_image.insert(sparse_image.end(), hdr_bytes, hdr_bytes + sizeof(hdr));

    uint32_t tmp = payload_;
    // Make the payload an ascending counter for the raw case to disambiguate with fill.
    uint32_t increment = type_ == ChunkType::kRaw ? 1 : 0;
    for (size_t i = 0; i < (SizeInImage() - sizeof(hdr)) / sizeof(tmp); i++, tmp += increment) {
      const uint8_t* tmp_bytes = reinterpret_cast<const uint8_t*>(&tmp);
      sparse_image.insert(sparse_image.end(), tmp_bytes, tmp_bytes + sizeof(tmp));
    }
  }

  void AppendExpectedBytes(std::vector<uint32_t>& image) const {
    // Make the payload an ascending counter for the raw case to disambiguate with fill.
    uint32_t increment = type_ == ChunkType::kRaw ? 1 : 0;
    uint32_t tmp = payload_;
    switch (type_) {
      case ChunkType::kRaw:
      case ChunkType::kFill:
        for (size_t i = 0; i < output_blocks_ * block_size_bytes_ / sizeof(uint32_t);
             i++, tmp += increment) {
          image.push_back(tmp);
        }
        break;
      case ChunkType::kDontCare:
        for (size_t i = 0; i < output_blocks_ * block_size_bytes_ / sizeof(uint32_t); i++) {
          // A DONT_CARE chunk still has an impact on the output image
          image.push_back(0);
        }
        break;
      case ChunkType::kUnknown:
      case ChunkType::kCrc32:
        break;
    }
  }

 private:
  ChunkType type_;
  uint32_t payload_;
  size_t output_blocks_;
  size_t block_size_bytes_;
};

SparseImageResult CreateSparseImage() {
  constexpr size_t kBlockSize = 512;
  std::vector<uint32_t> raw;
  std::vector<uint8_t> sparse;

  constexpr Chunk chunks[] = {
      Chunk(Chunk::ChunkType::kRaw, 0x55555555, 1, kBlockSize),
      Chunk(Chunk::ChunkType::kDontCare, 0, 2, kBlockSize),
      Chunk(Chunk::ChunkType::kFill, 0xCAFED00D, 3, kBlockSize),
  };
  size_t total_blocks =
      std::reduce(std::cbegin(chunks), std::cend(chunks), 0,
                  [](size_t sum, const Chunk& c) { return sum + c.OutputBlocks(); });
  size_t image_length =
      std::reduce(std::cbegin(chunks), std::cend(chunks), 0,
                  [](size_t sum, const Chunk& c) { return sum + c.OutputSize(); });
  sparse_header_t header = {
      .magic = SPARSE_HEADER_MAGIC,
      .major_version = 1,
      .file_hdr_sz = sizeof(sparse_header_t),
      .chunk_hdr_sz = sizeof(chunk_header_t),
      .blk_sz = kBlockSize,
      .total_blks = static_cast<uint32_t>(total_blocks),
      .total_chunks = static_cast<uint32_t>(std::size(chunks)),
      .image_checksum = 0xDEADBEEF  // We don't do crc validation as of 2023-07-05
  };
  const uint8_t* header_bytes = reinterpret_cast<const uint8_t*>(&header);
  sparse.insert(sparse.end(), header_bytes, header_bytes + sizeof(header));
  for (const Chunk& chunk : chunks) {
    chunk.AppendImageBytes(sparse);
    chunk.AppendExpectedBytes(raw);
  }

  return SparseImageResult{
      .sparse = std::move(sparse),
      .raw_data = std::move(raw),
      .image_length = image_length,
  };
}

TEST_F(PaverServiceLuisTest, WriteSparseVolume) {
  ASSERT_NO_FATAL_FAILURE(InitializeLuisGPTPartitions());
  auto [local, remote] = fidl::Endpoints<fuchsia_paver::DynamicDataSink>::Create();

  {
    zx::result connections = GetNewConnections(gpt_dev_->block_controller_interface());
    ASSERT_OK(connections);
    ASSERT_OK(client_->UseBlockDevice(
        fidl::ClientEnd<fuchsia_hardware_block::Block>(std::move(connections->device)),
        std::move(connections->controller), std::move(remote)));
  }
  fidl::WireSyncClient data_sink{std::move(local)};

  SparseImageResult image = CreateSparseImage();

  fuchsia_mem::wire::Buffer payload_wire_buffer;
  zx::vmo payload_vmo;
  fzl::VmoMapper payload_vmo_mapper;
  ASSERT_OK(payload_vmo_mapper.CreateAndMap(image.sparse.size(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                                            nullptr, &payload_vmo));
  std::copy(image.sparse.cbegin(), image.sparse.cend(),
            static_cast<uint8_t*>(payload_vmo_mapper.start()));
  payload_wire_buffer.vmo = std::move(payload_vmo);
  payload_wire_buffer.size = image.sparse.size();

  auto result = data_sink->WriteSparseVolume(std::move(payload_wire_buffer));
  ASSERT_OK(result.status());

  // Create a block partition client to read the written content directly.
  zx::result block_client =
      paver::BlockPartitionClient::Create(gpt_dev_->block_controller_interface());
  ASSERT_OK(block_client);

  // Read the partition directly from block and verify.  Read `image.image_length` bytes so we know
  // the image was paved to the desired length, although we only verify the bytes up to the size of
  // `image.raw_data`.
  zx::vmo block_read_vmo;
  fzl::VmoMapper block_read_vmo_mapper;
  ASSERT_OK(block_read_vmo_mapper.CreateAndMap(image.image_length, ZX_VM_PERM_READ, nullptr,
                                               &block_read_vmo));
  ASSERT_OK(block_client->Read(block_read_vmo, image.image_length, kFvmBlockStart, 0));

  // Verify the written data against the unsparsed payload
  cpp20::span<const uint8_t> raw_as_bytes = {
      reinterpret_cast<const uint8_t*>(image.raw_data.data()),
      image.raw_data.size() * sizeof(uint32_t)};
  ASSERT_BYTES_EQ(block_read_vmo_mapper.start(), raw_as_bytes.data(), raw_as_bytes.size());
}

TEST_F(PaverServiceLuisTest, OneShotRecovery) {
  // TODO(b/255567130): There's an discussion whether use one-shot-recovery to implement
  // RebootToRecovery in power-manager. If the approach is taken, paver e2e test will
  // cover this.
  ASSERT_NO_FATAL_FAILURE(InitializeLuisGPTPartitions());

  auto [local, remote] = fidl::Endpoints<fuchsia_paver::BootManager>::Create();

  // Required by FindBootManager().
  fake_svc_.fake_boot_args().SetArgResponse("_a");

  auto result = client_->FindBootManager(std::move(remote));
  ASSERT_OK(result.status());
  auto boot_manager = fidl::WireSyncClient(std::move(local));

  auto set_one_shot_recovery_result = boot_manager->SetOneShotRecovery();
  ASSERT_OK(set_one_shot_recovery_result.status());

  // Read the abr data directly from block and verify.
  zx::vmo block_read_vmo;
  fzl::VmoMapper block_read_vmo_mapper;
  ASSERT_OK(block_read_vmo_mapper.CreateAndMap(kDurableBootSize * kBlockSize, ZX_VM_PERM_READ,
                                               nullptr, &block_read_vmo));
  gpt_dev_->Read(block_read_vmo, kDurableBootSize, kDurableBootStart);

  AbrData disk_abr_data;
  memcpy(&disk_abr_data, block_read_vmo_mapper.start(), sizeof(disk_abr_data));
  ASSERT_TRUE(AbrIsOneShotRecoveryBoot(&disk_abr_data));
}

}  // namespace
