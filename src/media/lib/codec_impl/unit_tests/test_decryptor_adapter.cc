// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/media/cpp/fidl.h>
#include <fuchsia/media/drm/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/executor.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/sys/cpp/component_context.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <random>
#include <unordered_map>
#include <variant>
#include <vector>

#include <gtest/gtest.h>

#include "lib/media/codec_impl/codec_impl.h"
#include "lib/media/codec_impl/decryptor_adapter.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace {

constexpr uint64_t kBufferConstraintsVersionOrdinal = 1;
constexpr uint64_t kBufferLifetimeOrdinal = 1;
constexpr uint64_t kStreamLifetimeOrdinal = 1;
constexpr uint32_t kInputPacketSize = 8 * 1024;

auto CreateDecryptorParams(bool is_secure) {
  fuchsia::media::drm::DecryptorParams params;
  params.mutable_input_details()->set_format_details_version_ordinal(0);
  if (is_secure) {
    params.set_require_secure_mode(is_secure);
  }
  return params;
}

auto CreateStreamBufferPartialSettings(
    const fuchsia::media::StreamBufferConstraints& constraints,
    fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token) {
  fuchsia::media::StreamBufferPartialSettings settings;
  settings.set_buffer_lifetime_ordinal(kBufferLifetimeOrdinal)
      .set_buffer_constraints_version_ordinal(kBufferConstraintsVersionOrdinal)
      .set_sysmem_token(std::move(token));
  return settings;
}

auto CreateBufferCollectionConstraints(uint32_t cpu_usage) {
  fuchsia::sysmem::BufferCollectionConstraints collection_constraints;

  collection_constraints.usage.cpu = cpu_usage;
  collection_constraints.min_buffer_count_for_camping = 1;
  collection_constraints.has_buffer_memory_constraints = true;
  collection_constraints.buffer_memory_constraints.min_size_bytes = kInputPacketSize;

  // Secure buffers not allowed for test keys
  EXPECT_FALSE(collection_constraints.buffer_memory_constraints.secure_required);

  return collection_constraints;
}

auto CreateInputFormatDetails(const std::string& scheme, const std::vector<uint8_t>& key_id,
                              const std::vector<uint8_t>& init_vector) {
  constexpr uint64_t kFormatDetailsVersionOrdinal = 0;

  fuchsia::media::FormatDetails details;
  details.set_format_details_version_ordinal(kFormatDetailsVersionOrdinal);
  details.mutable_domain()
      ->crypto()
      .encrypted()
      .set_scheme(scheme)
      .set_key_id(key_id)
      .set_init_vector(init_vector);

  return details;
}

class FakeDecryptorAdapter : public DecryptorAdapter {
 public:
  explicit FakeDecryptorAdapter(std::mutex& lock, CodecAdapterEvents* codec_adapter_events,
                                inspect::Node node)
      : DecryptorAdapter(lock, codec_adapter_events, std::move(node)) {}

  void set_has_keys(bool has_keys) { has_keys_ = has_keys; }
  bool has_keys() const { return has_keys_; }

  void set_use_mapped_output(bool use_mapped_output) { use_mapped_output_ = use_mapped_output; }
  bool use_mapped_output() const { return use_mapped_output_; }

  bool IsCoreCodecMappedBufferUseful(CodecPort port) override {
    if (!use_mapped_output_ && port == kOutputPort) {
      return false;
    } else {
      return DecryptorAdapter::IsCoreCodecMappedBufferUseful(port);
    }
  }

 private:
  bool has_keys_ = false;
  bool use_mapped_output_ = true;
};

class ClearTextDecryptorAdapter : public FakeDecryptorAdapter {
 public:
  explicit ClearTextDecryptorAdapter(std::mutex& lock, CodecAdapterEvents* codec_adapter_events,
                                     inspect::Node node)
      : FakeDecryptorAdapter(lock, codec_adapter_events, std::move(node)) {}

  std::optional<fuchsia::media::StreamError> Decrypt(const EncryptionParams& params,
                                                     const InputBuffer& input,
                                                     const OutputBuffer& output,
                                                     CodecPacket* output_packet) override {
    if (!std::holds_alternative<ClearOutputBuffer>(output)) {
      fprintf(stderr, "!std::holds_alternative<ClearOutputBuffer>(output)\n");
      return fuchsia::media::StreamError::DECRYPTOR_UNKNOWN;
    }
    auto& clear_output = std::get<ClearOutputBuffer>(output);

    if (input.data_length > clear_output.data_length) {
      fprintf(stderr, "input.data_length > clear_output.data_length -- input: %u output: %u\n",
              input.data_length, clear_output.data_length);
      return fuchsia::media::StreamError::DECRYPTOR_UNKNOWN;
    }

    if (!has_keys()) {
      fprintf(stderr, "!has_keys()\n");
      return fuchsia::media::StreamError::DECRYPTOR_NO_KEY;
    }

    std::memcpy(clear_output.data, input.data, input.data_length);

    return std::nullopt;
  }
};

class FakeSecureDecryptorAdapter : public FakeDecryptorAdapter {
 public:
  explicit FakeSecureDecryptorAdapter(std::mutex& lock, CodecAdapterEvents* codec_adapter_events,
                                      inspect::Node node)
      : FakeDecryptorAdapter(lock, codec_adapter_events, std::move(node)) {}

  std::optional<fuchsia::media::StreamError> Decrypt(const EncryptionParams& params,
                                                     const InputBuffer& input,
                                                     const OutputBuffer& output,
                                                     CodecPacket* output_packet) override {
    // We should not get here, so just return an error.
    return fuchsia::media::StreamError::DECRYPTOR_UNKNOWN;
  }
};

}  // namespace

template <typename DecryptorAdapterT>
class DecryptorAdapterTest : public gtest::RealLoopFixture {
 protected:
  DecryptorAdapterTest()
      : context_(sys::ComponentContext::Create()), random_device_(), prng_(random_device_()) {
    EXPECT_EQ(ZX_OK, context_->svc()->Connect(allocator_.NewRequest()));
    allocator_->SetDebugClientInfo(fsl::GetCurrentProcessName(), fsl::GetCurrentProcessKoid());

    PopulateInputData();

    allocator_.set_error_handler([this](zx_status_t s) { sysmem_error_ = s; });
    decryptor_.set_error_handler([this](zx_status_t s) { decryptor_error_ = s; });
    input_collection_.set_error_handler([this](zx_status_t s) { input_collection_error_ = s; });
    output_collection_.set_error_handler([this](zx_status_t s) { output_collection_error_ = s; });

    decryptor_.events().OnStreamFailed =
        fit::bind_member(this, &DecryptorAdapterTest::OnStreamFailed);
    decryptor_.events().OnInputConstraints =
        fit::bind_member(this, &DecryptorAdapterTest::OnInputConstraints);
    decryptor_.events().OnOutputConstraints =
        fit::bind_member(this, &DecryptorAdapterTest::OnOutputConstraints);
    decryptor_.events().OnOutputFormat =
        fit::bind_member(this, &DecryptorAdapterTest::OnOutputFormat);
    decryptor_.events().OnOutputPacket =
        fit::bind_member(this, &DecryptorAdapterTest::OnOutputPacket);
    decryptor_.events().OnFreeInputPacket =
        fit::bind_member(this, &DecryptorAdapterTest::OnFreeInputPacket);
    decryptor_.events().OnOutputEndOfStream =
        fit::bind_member(this, &DecryptorAdapterTest::OnOutputEndOfStream);
  }

  ~DecryptorAdapterTest() {
    // Cleanly terminate BufferCollection view to avoid errors as the test halts.
    if (input_collection_.is_bound()) {
      input_collection_->Close();
    }
    if (output_collection_.is_bound()) {
      output_collection_->Close();
    }
  }

  void ConnectDecryptor(bool is_secure) {
    fidl::InterfaceHandle<fuchsia::sysmem::Allocator> allocator;

    ASSERT_EQ(ZX_OK, context_->svc()->Connect(allocator.NewRequest()));
    codec_impl_ =
        std::make_unique<CodecImpl>(std::move(allocator), nullptr, dispatcher(), thrd_current(),
                                    CreateDecryptorParams(is_secure), decryptor_.NewRequest());
    auto adapter = std::make_unique<DecryptorAdapterT>(
        codec_impl_->lock(), codec_impl_.get(), inspector_.GetRoot().CreateChild("decryptor"));
    // Grab a non-owning reference to the adapter for test manipulation.
    decryptor_adapter_ = adapter.get();
    codec_impl_->SetCoreCodecAdapter(std::move(adapter));

    codec_impl_->BindAsync([this]() { codec_impl_ = nullptr; });
  }

  void OnStreamFailed(uint64_t stream_lifetime_ordinal, fuchsia::media::StreamError error) {
    stream_error_ = std::move(error);
    for (const auto& [packet_index, buffer_index] : used_packets_) {
      failed_packets_.insert(packet_index);
    }
  }

  void OnInputConstraints(fuchsia::media::StreamBufferConstraints ic) {
    auto settings = BindBufferCollection(
        input_collection_, fuchsia::sysmem::cpuUsageWrite | fuchsia::sysmem::cpuUsageWriteOften,
        ic);
    input_collection_->WaitForBuffersAllocated(
        [this](zx_status_t status, fuchsia::sysmem::BufferCollectionInfo_2 info) {
          ASSERT_EQ(status, ZX_OK);
          input_buffer_info_ = std::move(info);
        });

    input_collection_->Sync([this, settings = std::move(settings)]() mutable {
      decryptor_->SetInputBufferPartialSettings(std::move(settings));
    });

    input_constraints_ = std::move(ic);
  }

  void OnOutputConstraints(fuchsia::media::StreamOutputConstraints oc) {
    auto settings = BindBufferCollection(
        output_collection_, fuchsia::sysmem::cpuUsageRead | fuchsia::sysmem::cpuUsageReadOften,
        oc.buffer_constraints());
    output_collection_->WaitForBuffersAllocated(
        [this](zx_status_t status, fuchsia::sysmem::BufferCollectionInfo_2 info) {
          if (status == ZX_OK) {
            output_buffer_info_ = std::move(info);
          }
        });

    output_collection_->Sync([this, settings = std::move(settings)]() mutable {
      decryptor_->SetOutputBufferPartialSettings(std::move(settings));
      decryptor_->CompleteOutputBufferPartialSettings(kBufferLifetimeOrdinal);
    });

    output_constraints_ = std::move(oc);
  }

  void OnOutputFormat(fuchsia::media::StreamOutputFormat of) { output_format_ = std::move(of); }

  void OnOutputPacket(fuchsia::media::Packet packet, bool error_before, bool error_during) {
    EXPECT_FALSE(error_before);
    EXPECT_FALSE(error_during);
    auto header = fidl::Clone(packet.header());
    output_data_.emplace_back(ExtractPayloadData(std::move(packet)));
    decryptor_->RecycleOutputPacket(std::move(header));
  }

  void OnFreeInputPacket(fuchsia::media::PacketHeader header) {
    ASSERT_TRUE(header.has_packet_index());
    FreePacket(header.packet_index());
    if (stream_error_) {
      freed_failed_packets_.insert(header.packet_index());
      return;
    }
    successful_packet_count_++;
    if (end_of_stream_set_) {
      return;
    }
    PumpInput();
  }

  void OnOutputEndOfStream(uint64_t stream_lifetime_ordinal, bool error_before) {
    end_of_stream_reached_ = true;
  }

  void PopulateInputData() {
    constexpr size_t kNumInputPackets = 50;
    std::uniform_int_distribution<int> dist(0, std::numeric_limits<uint8_t>::max());

    input_data_.reserve(kNumInputPackets);
    for (size_t i = 0; i < kNumInputPackets; i++) {
      std::vector<uint8_t> v(kInputPacketSize);
      std::generate(v.begin(), v.end(),
                    [this, &dist]() { return static_cast<uint8_t>(dist(prng_)); });
      input_data_.emplace_back(std::move(v));
    }
    input_iter_ = input_data_.begin();
  }

  fuchsia::media::StreamBufferPartialSettings BindBufferCollection(
      fuchsia::sysmem::BufferCollectionPtr& collection, uint32_t cpu_usage,
      const fuchsia::media::StreamBufferConstraints& constraints) {
    fuchsia::sysmem::BufferCollectionTokenPtr client_token;
    allocator_->AllocateSharedCollection(client_token.NewRequest());

    fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> decryptor_token;
    client_token->Duplicate(std::numeric_limits<uint32_t>::max(), decryptor_token.NewRequest());

    allocator_->BindSharedCollection(client_token.Unbind(), collection.NewRequest());
    collection->SetConstraints(true, CreateBufferCollectionConstraints(cpu_usage));

    return CreateStreamBufferPartialSettings(constraints, std::move(decryptor_token));
  }

  fuchsia::media::Packet CreateInputPacket(const std::vector<uint8_t>& data) {
    fuchsia::media::Packet packet;
    static uint64_t timestamp_ish = 42;
    uint32_t packet_index;
    uint32_t buffer_index;
    AllocatePacket(&packet_index, &buffer_index);

    auto& vmo = input_buffer_info_->buffers[buffer_index].vmo;
    uint64_t offset = input_buffer_info_->buffers[buffer_index].vmo_usable_start;
    size_t size = data.size();

    // Since test code, no particular reason to bother with mapping.
    auto status = vmo.write(data.data(), offset, size);

    EXPECT_EQ(status, ZX_OK);

    packet.mutable_header()
        ->set_buffer_lifetime_ordinal(kBufferLifetimeOrdinal)
        .set_packet_index(packet_index);
    packet.set_buffer_index(buffer_index)
        .set_stream_lifetime_ordinal(kStreamLifetimeOrdinal)
        .set_start_offset(0)
        .set_valid_length_bytes(size)
        .set_timestamp_ish(timestamp_ish++)
        .set_start_access_unit(true);

    return packet;
  }

  std::vector<uint8_t> ExtractPayloadData(fuchsia::media::Packet packet) {
    std::vector<uint8_t> data;

    uint32_t buffer_index = packet.buffer_index();
    uint32_t offset = packet.start_offset();
    uint32_t size = packet.valid_length_bytes();

    EXPECT_TRUE(buffer_index < output_buffer_info_->buffer_count);

    const auto& buffer = output_buffer_info_->buffers[buffer_index];

    data.resize(size);
    auto status = buffer.vmo.read(data.data(), offset, size);
    EXPECT_EQ(status, ZX_OK);

    return data;
  }

  bool HasFreePackets() { return !free_packets_.empty(); }

  void ConfigureInputPackets() {
    ASSERT_TRUE(input_buffer_info_);

    auto buffer_count = input_buffer_info_->buffer_count;
    std::vector<uint32_t> buffers;
    std::vector<uint32_t> packets;
    for (uint32_t i = 0; i < buffer_count; i++) {
      buffers.emplace_back(i);
      packets.emplace_back(i);
    }
    // Shuffle the packet indexes so that they don't align with the buffer indexes
    std::shuffle(packets.begin(), packets.end(), prng_);

    for (uint32_t i = 0; i < buffer_count; i++) {
      free_packets_.emplace(packets[i], buffers[i]);
    }
  }

  void AllocatePacket(uint32_t* packet_index, uint32_t* buffer_index) {
    ASSERT_TRUE(HasFreePackets());
    ASSERT_TRUE(packet_index);
    ASSERT_TRUE(buffer_index);

    auto node = free_packets_.extract(free_packets_.begin());
    *packet_index = node.key();
    *buffer_index = node.mapped();
    used_packets_.insert(std::move(node));
  }

  void FreePacket(uint32_t packet_index) {
    free_packets_.insert(used_packets_.extract(packet_index));
  }

  void PumpInput() {
    while (input_iter_ != input_data_.end() && HasFreePackets()) {
      decryptor_->QueueInputPacket(CreateInputPacket(*input_iter_));
      input_iter_++;
    }
    if (input_iter_ == input_data_.end() && !end_of_stream_set_) {
      decryptor_->QueueInputEndOfStream(kStreamLifetimeOrdinal);
      end_of_stream_set_ = true;
    }
  }

  void AssertNoChannelErrors() {
    ASSERT_FALSE(decryptor_error_) << "Decryptor error = " << *decryptor_error_;
    ASSERT_FALSE(sysmem_error_) << "Sysmem error = " << *sysmem_error_;
    ASSERT_FALSE(input_collection_error_)
        << "Input BufferCollection error = " << *input_collection_error_;
    ASSERT_FALSE(output_collection_error_)
        << "Output BufferCollection error = " << *output_collection_error_;
  }

  bool HasChannelErrors() {
    return decryptor_error_.has_value() || sysmem_error_.has_value() ||
           input_collection_error_.has_value() || output_collection_error_.has_value();
  }

  std::unique_ptr<sys::ComponentContext> context_;
  inspect::Inspector inspector_;
  fuchsia::media::StreamProcessorPtr decryptor_;
  fuchsia::sysmem::AllocatorPtr allocator_;
  std::unique_ptr<CodecImpl> codec_impl_;
  DecryptorAdapterT* decryptor_adapter_;

  using DataSet = std::vector<std::vector<uint8_t>>;
  DataSet input_data_;
  DataSet output_data_;

  std::optional<fuchsia::media::StreamBufferConstraints> input_constraints_;
  std::optional<fuchsia::media::StreamOutputConstraints> output_constraints_;
  std::optional<fuchsia::media::StreamOutputFormat> output_format_;
  bool end_of_stream_set_ = false;
  bool end_of_stream_reached_ = false;
  DataSet::const_iterator input_iter_;

  fuchsia::sysmem::BufferCollectionPtr input_collection_;
  fuchsia::sysmem::BufferCollectionPtr output_collection_;

  std::optional<fuchsia::sysmem::BufferCollectionInfo_2> input_buffer_info_;
  std::optional<fuchsia::sysmem::BufferCollectionInfo_2> output_buffer_info_;

  std::optional<fuchsia::media::StreamError> stream_error_;
  std::optional<zx_status_t> sysmem_error_;
  std::optional<zx_status_t> decryptor_error_;
  std::optional<zx_status_t> input_collection_error_;
  std::optional<zx_status_t> output_collection_error_;

  using PacketMap = std::unordered_map<uint32_t /*packet_index*/, uint32_t /*buffer_index*/>;

  PacketMap free_packets_;
  PacketMap used_packets_;

  unsigned int successful_packet_count_ = 0;
  std::set<uint32_t> failed_packets_;
  std::set<uint32_t> freed_failed_packets_;

  std::random_device random_device_;
  std::mt19937 prng_;
};

class ClearDecryptorAdapterTest : public DecryptorAdapterTest<ClearTextDecryptorAdapter> {};

TEST_F(ClearDecryptorAdapterTest, ClearTextDecrypt) {
  ConnectDecryptor(false);
  decryptor_adapter_->set_has_keys(true);

  RunLoopUntil([this]() { return input_buffer_info_.has_value(); });

  AssertNoChannelErrors();
  ASSERT_TRUE(input_buffer_info_);

  ConfigureInputPackets();

  decryptor_->QueueInputFormatDetails(kStreamLifetimeOrdinal,
                                      CreateInputFormatDetails("clear", {}, {}));

  PumpInput();

  RunLoopUntil([this]() {
    return end_of_stream_reached_ && free_packets_.size() == input_buffer_info_->buffer_count;
  });

  AssertNoChannelErrors();

  EXPECT_TRUE(input_constraints_);
  EXPECT_TRUE(output_constraints_);
  EXPECT_TRUE(output_format_);

  ASSERT_TRUE(end_of_stream_reached_);
  // ClearText decryptor just copies data across
  EXPECT_EQ(output_data_, input_data_);

  // Check that all input packets were returned
  EXPECT_EQ(free_packets_.size(), input_buffer_info_->buffer_count);
}

TEST_F(ClearDecryptorAdapterTest, NoKeys) {
  ConnectDecryptor(false);
  decryptor_adapter_->set_has_keys(false);
  decryptor_->EnableOnStreamFailed();

  RunLoopUntil([this]() { return input_buffer_info_.has_value(); });

  AssertNoChannelErrors();
  ASSERT_TRUE(input_buffer_info_);

  ConfigureInputPackets();

  decryptor_->QueueInputFormatDetails(kStreamLifetimeOrdinal,
                                      CreateInputFormatDetails("clear", {}, {}));

  PumpInput();

  RunLoopUntil([this]() {
    return stream_error_.has_value() && free_packets_.size() == input_buffer_info_->buffer_count;
  });

  AssertNoChannelErrors();

  ASSERT_TRUE(stream_error_.has_value());
  EXPECT_EQ(*stream_error_, fuchsia::media::StreamError::DECRYPTOR_NO_KEY);

  // Check that all input packets were returned
  EXPECT_EQ(free_packets_.size(), input_buffer_info_->buffer_count);

  // Check that no input packets were successfully completed.
  EXPECT_EQ(successful_packet_count_, 0u);

  // Check that all of the packets that were with the server at the time of the stream failure have
  // been returned.
  EXPECT_GT(failed_packets_.size(), 0u);
  EXPECT_GT(freed_failed_packets_.size(), 0u);
  EXPECT_EQ(failed_packets_, freed_failed_packets_);
}

TEST_F(ClearDecryptorAdapterTest, UnmappedOutputBuffers) {
  ConnectDecryptor(false);
  decryptor_adapter_->set_has_keys(true);
  decryptor_adapter_->set_use_mapped_output(false);

  RunLoopUntil([this]() { return input_buffer_info_.has_value(); });

  AssertNoChannelErrors();
  ASSERT_TRUE(input_buffer_info_);

  ConfigureInputPackets();

  decryptor_->QueueInputFormatDetails(kStreamLifetimeOrdinal,
                                      CreateInputFormatDetails("clear", {}, {}));

  PumpInput();

  RunLoopUntil([this]() { return HasChannelErrors(); });

  // The decryptor should have failed (since unmapped buffers are unsupported),
  // and nothing else should have failed.
  EXPECT_TRUE(decryptor_error_.has_value());
  decryptor_error_ = std::nullopt;
  AssertNoChannelErrors();
}

TEST_F(ClearDecryptorAdapterTest, DecryptorClosesBuffersCleanly) {
  ConnectDecryptor(false);
  decryptor_adapter_->set_has_keys(true);

  RunLoopUntil([this]() { return input_buffer_info_.has_value(); });
  AssertNoChannelErrors();
  ASSERT_TRUE(input_buffer_info_);

  ConfigureInputPackets();

  decryptor_->QueueInputFormatDetails(kStreamLifetimeOrdinal,
                                      CreateInputFormatDetails("clear", {}, {}));

  // Queue a single input packet.
  ASSERT_TRUE(HasFreePackets());
  decryptor_->QueueInputPacket(CreateInputPacket(*input_iter_++));

  // Wait until the output collection has been allocated.
  RunLoopUntil([this]() { return output_buffer_info_.has_value(); });
  AssertNoChannelErrors();
  ASSERT_TRUE(output_buffer_info_);

  // Wait until receive first output packet.
  RunLoopUntil([this]() { return !output_data_.empty(); });
  AssertNoChannelErrors();

  // Drop the decryptor_. This should not cause any buffer collection failures.
  decryptor_ = nullptr;
  AssertNoChannelErrors();

  // If the below fail, the collections have failed.
  std::optional<zx_status_t> input_status;
  std::optional<zx_status_t> output_status;
  EXPECT_TRUE(input_collection_.is_bound());
  EXPECT_TRUE(output_collection_.is_bound());
  input_collection_->CheckBuffersAllocated([&input_status](zx_status_t s) { input_status = s; });
  output_collection_->CheckBuffersAllocated([&output_status](zx_status_t s) { output_status = s; });
  RunLoopUntil([&]() { return input_status.has_value() && output_status.has_value(); });
  EXPECT_EQ(*input_status, ZX_OK);
  EXPECT_EQ(*output_status, ZX_OK);
  AssertNoChannelErrors();
  EXPECT_FALSE(stream_error_.has_value());

  // Buffers are A-OK after dropping decryptor_.

  // ClearText decryptor just copies data across, and only one input packet was sent.
  EXPECT_EQ(output_data_[0], input_data_[0]);
}

TEST_F(ClearDecryptorAdapterTest, InspectValues) {
  ConnectDecryptor(false);
  decryptor_adapter_->set_has_keys(true);
  RunLoopUntil([this]() { return input_buffer_info_.has_value(); });
  AssertNoChannelErrors();
  ASSERT_TRUE(input_buffer_info_);

  ConfigureInputPackets();

  const std::string kScheme = "clear";
  const std::vector<uint8_t> kKeyId = {0xde, 0xad, 0xbe, 0xef};
  decryptor_->QueueInputFormatDetails(kStreamLifetimeOrdinal,
                                      CreateInputFormatDetails(kScheme, kKeyId, {}));

  // Queue a single input packet to trigger output buffer allocation.
  ASSERT_TRUE(HasFreePackets());
  decryptor_->QueueInputPacket(CreateInputPacket(*input_iter_++));

  // Wait until output packet received to ensure the input format details have definitely been
  // processed
  RunLoopUntil([this]() { return !output_data_.empty(); });
  AssertNoChannelErrors();

  async::Executor executor(dispatcher());
  fpromise::result<inspect::Hierarchy> hierarchy;
  executor.schedule_task(inspect::ReadFromInspector(inspector_)
                             .then([&](fpromise::result<inspect::Hierarchy>& result) {
                               hierarchy = std::move(result);
                             }));
  RunLoopUntil([&]() { return !hierarchy.is_pending(); });
  ASSERT_TRUE(hierarchy.is_ok());

  auto* decryptor_hierarchy = hierarchy.value().GetByPath({"decryptor"});
  ASSERT_TRUE(decryptor_hierarchy);

  auto* secure_property =
      decryptor_hierarchy->node().get_property<inspect::BoolPropertyValue>("secure_mode");
  ASSERT_TRUE(secure_property);
  EXPECT_FALSE(secure_property->value());

  auto* scheme_property =
      decryptor_hierarchy->node().get_property<inspect::StringPropertyValue>("scheme");
  ASSERT_TRUE(scheme_property);
  EXPECT_EQ(scheme_property->value(), kScheme);

  auto* key_id_property =
      decryptor_hierarchy->node().get_property<inspect::ByteVectorPropertyValue>("key_id");
  ASSERT_TRUE(key_id_property);
  EXPECT_EQ(key_id_property->value(), kKeyId);

  auto port_validator = [&](const std::string& port) {
    auto* port_hierarchy = decryptor_hierarchy->GetByPath({port});
    ASSERT_TRUE(port_hierarchy);
    auto* buffer_count_property =
        port_hierarchy->node().get_property<inspect::UintPropertyValue>("buffer_count");
    ASSERT_TRUE(buffer_count_property);
    EXPECT_GT(buffer_count_property->value(), 0u);
    auto* packet_count_property =
        port_hierarchy->node().get_property<inspect::UintPropertyValue>("packet_count");
    ASSERT_TRUE(packet_count_property);
    EXPECT_GT(packet_count_property->value(), 0u);
  };

  port_validator("input_port");
  port_validator("output_port");
}

class SecureDecryptorAdapterTest : public DecryptorAdapterTest<FakeSecureDecryptorAdapter> {};

TEST_F(SecureDecryptorAdapterTest, FailsToAcquireSecureBuffers) {
  ConnectDecryptor(true);

  RunLoopUntil([this]() { return input_buffer_info_.has_value(); });

  AssertNoChannelErrors();
  ASSERT_TRUE(input_buffer_info_);

  ConfigureInputPackets();

  decryptor_->QueueInputFormatDetails(kStreamLifetimeOrdinal,
                                      CreateInputFormatDetails("secure", {}, {}));

  PumpInput();

  // TODO(https://fxbug.dev/42086427): Once there is a Sysmem fake that allows us to control behavior, we could
  // force it to give us back "secure" buffers that aren't really secure and go through more of the
  // flow.
  RunLoopUntil(
      [this]() { return decryptor_error_.has_value() && output_collection_error_.has_value(); });

  EXPECT_TRUE(decryptor_error_.has_value());
  EXPECT_TRUE(output_collection_error_.has_value());

  EXPECT_TRUE(input_constraints_);
  EXPECT_TRUE(output_constraints_);
  EXPECT_FALSE(output_format_);

  EXPECT_FALSE(end_of_stream_reached_);
}
