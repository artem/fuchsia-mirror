// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/gatt/server.h"

#include <lib/async/cpp/task.h>

#include <fbl/macros.h>

#include "src/connectivity/bluetooth/core/bt-host/att/att.h"
#include "src/connectivity/bluetooth/core/bt-host/att/attribute.h"
#include "src/connectivity/bluetooth/core/bt-host/att/database.h"
#include "src/connectivity/bluetooth/core/bt-host/common/test_helpers.h"
#include "src/connectivity/bluetooth/core/bt-host/common/uuid.h"
#include "src/connectivity/bluetooth/core/bt-host/gatt/gatt_defs.h"
#include "src/connectivity/bluetooth/core/bt-host/gatt/local_service_manager.h"
#include "src/connectivity/bluetooth/core/bt-host/l2cap/fake_channel_test.h"

namespace bt::gatt {
namespace {

constexpr UUID kTestSvcType(uint16_t{0xDEAD});
constexpr UUID kTestChrcType(uint16_t{0xFEED});
constexpr IdType kTestChrcId{0xFADE};
constexpr PeerId kTestPeerId(1);
constexpr UUID kTestType16(uint16_t{0xBEEF});
constexpr UUID kTestType128({0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15});

const auto kTestValue1 = CreateStaticByteBuffer('f', 'o', 'o');
const auto kTestValue2 = CreateStaticByteBuffer('b', 'a', 'r');
const auto kTestValue3 = CreateStaticByteBuffer('b', 'a', 'z');
const auto kTestValue4 = CreateStaticByteBuffer('l', 'o', 'l');

const auto kTestValueLong = CreateStaticByteBuffer('l', 'o', 'n', 'g');

inline att::AccessRequirements AllowedNoSecurity() {
  return att::AccessRequirements(/*encryption=*/false, /*authentication=*/false,
                                 /*authorization=*/false);
}

class ServerTest : public l2cap::testing::FakeChannelTest {
 public:
  ServerTest() = default;
  ~ServerTest() override = default;

 protected:
  void SetUp() override {
    local_services_ = std::make_unique<LocalServiceManager>();

    ChannelOptions options(l2cap::kATTChannelId);
    auto fake_chan = CreateFakeChannel(options);
    att_ = att::Bearer::Create(std::move(fake_chan));
    server_ = gatt::Server::Create(kTestPeerId, local_services_->GetWeakPtr(), att_);
  }

  void TearDown() override {
    server_ = nullptr;
    att_ = nullptr;
    local_services_ = nullptr;
  }

  // Registers a service with UUID |svc_type| containing a single characteristic of UUID |chrc_type|
  // and represented by |chrc_id|. The characteristic supports the indicate and notify properties,
  // but has not configured them via the CCC. Returns the ID of the registered service.
  IdType RegisterSvcWithSingleChrc(UUID svc_type, IdType chrc_id, UUID chrc_type) {
    ServicePtr svc = std::make_unique<Service>(/*primary=*/true, svc_type);
    CharacteristicPtr chr = std::make_unique<Characteristic>(
        chrc_id, chrc_type, Property::kIndicate | Property::kNotify,
        /*extended_properties=*/0u,
        /*read_permission=*/att::AccessRequirements(),
        /*write_permissions=*/att::AccessRequirements(),
        /*update_permisisons=*/AllowedNoSecurity());
    svc->AddCharacteristic(std::move(chr));
    return local_services_->RegisterService(std::move(svc), NopReadHandler, NopWriteHandler,
                                            NopCCCallback);
  }

  struct SvcIdAndChrcHandle {
    IdType svc_id;
    att::Handle chrc_val_handle;
  };
  // RegisterSvcWithSingleChrc, but the CCC is configured to |ccc_val| for kTestPeerId.
  // Returns the ID of the service alongside the handle of the registered characteristic value.
  SvcIdAndChrcHandle RegisterSvcWithConfiguredChrc(UUID svc_type, IdType chrc_id, UUID chrc_type,
                                                   uint16_t ccc_val = kCCCIndicationBit |
                                                                      kCCCNotificationBit) {
    IdType svc_id = RegisterSvcWithSingleChrc(svc_type, chrc_id, chrc_type);
    std::vector<att::Handle> chrc_val_handle = SetCCCs(kTestPeerId, chrc_type, ccc_val);
    EXPECT_EQ(1u, chrc_val_handle.size());
    return SvcIdAndChrcHandle{.svc_id = svc_id, .chrc_val_handle = chrc_val_handle[0]};
  }
  Server* server() const { return server_.get(); }

  fbl::RefPtr<att::Database> db() const { return local_services_->database(); }

  // TODO(armansito): Consider introducing a FakeBearer for testing (fxbug.dev/642).
  att::Bearer* att() const { return att_.get(); }

 private:
  enum CCCSearchState {
    kSearching,
    kChrcDeclarationFound,
    kCorrectChrcUuidFound,
  };
  // Sets |local_services_|' CCCs for |peer_id| to |ccc_val| for all characteristics of |chrc_type|,
  // and returns the handles of the characteristic values for which the CCC was modified.
  std::vector<att::Handle> SetCCCs(PeerId peer_id, bt::UUID chrc_type, uint16_t ccc_val) {
    std::vector<att::Handle> modified_attrs;
    CCCSearchState state = kSearching;
    for (auto& grouping : db()->groupings()) {
      att::Handle matching_chrc_value_handle = att::kInvalidHandle;
      for (auto& attr : grouping.attributes()) {
        if (attr.type() == types::kCharacteristicDeclaration) {
          EXPECT_NE(state, kChrcDeclarationFound)
              << "unexpectedly found two consecutive characteristic declarations";
          state = kChrcDeclarationFound;
        } else if (state == kChrcDeclarationFound && attr.type() == chrc_type) {
          state = kCorrectChrcUuidFound;
          matching_chrc_value_handle = attr.handle();
        } else if (state == kChrcDeclarationFound) {
          state = kSearching;
        } else if (state == kCorrectChrcUuidFound &&
                   attr.type() == types::kClientCharacteristicConfig) {
          ZX_ASSERT(matching_chrc_value_handle != att::kInvalidHandle);
          DynamicByteBuffer new_ccc(sizeof(ccc_val));
          new_ccc.WriteObj(ccc_val);
          auto write_status = att::ErrorCode::kReadNotPermitted;
          EXPECT_TRUE(attr.WriteAsync(peer_id, /*offset=*/0, new_ccc,
                                      [&](att::ErrorCode code) { write_status = code; }));
          // Not strictly necessary with the current WriteAsync implementation, but running the loop
          // here makes this more future-proof.
          test_loop().RunUntilIdle();
          EXPECT_EQ(att::ErrorCode::kNoError, write_status);
          modified_attrs.push_back(matching_chrc_value_handle);
        }
      }
    }
    EXPECT_NE(0u, modified_attrs.size()) << "Couldn't find CCC attribute!";
    return modified_attrs;
  }

  std::unique_ptr<LocalServiceManager> local_services_;
  fbl::RefPtr<att::Bearer> att_;
  std::unique_ptr<Server> server_;

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(ServerTest);
};

TEST_F(ServerTest, ExchangeMTURequestInvalidPDU) {
  // Just opcode
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x02);
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x02,        // request: exchange MTU
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, ExchangeMTURequestValueTooSmall) {
  const uint16_t kServerMTU = att::kLEMaxMTU;
  constexpr uint16_t kClientMTU = 1;

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
    0x02,             // opcode: exchange MTU
    kClientMTU, 0x00  // client rx mtu: |kClientMTU|
  );

  const auto kExpected = CreateStaticByteBuffer(
    0x03,       // opcode: exchange MTU response
    0xF7, 0x00  // server rx mtu: |kServerMTU|
  );
  // clang-format on

  ASSERT_EQ(kServerMTU, att()->preferred_mtu());

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));

  // Should default to kLEMinMTU since kClientMTU is too small.
  EXPECT_EQ(att::kLEMinMTU, att()->mtu());
}

TEST_F(ServerTest, ExchangeMTURequest) {
  constexpr uint16_t kServerMTU = att::kLEMaxMTU;
  constexpr uint16_t kClientMTU = 0x64;

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
    0x02,             // opcode: exchange MTU
    kClientMTU, 0x00  // client rx mtu: |kClientMTU|
  );

  const auto kExpected = CreateStaticByteBuffer(
    0x03,       // opcode: exchange MTU response
    0xF7, 0x00  // server rx mtu: |kServerMTU|
  );
  // clang-format on

  ASSERT_EQ(kServerMTU, att()->preferred_mtu());

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));

  EXPECT_EQ(kClientMTU, att()->mtu());
}

TEST_F(ServerTest, FindInformationInvalidPDU) {
  // Just opcode
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x04);
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x04,        // request: find information
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, FindInformationInvalidHandle) {
  // Start handle is 0
  // clang-format off
  const auto kInvalidStartHandle = CreateStaticByteBuffer(
      0x04,        // opcode: find information
      0x00, 0x00,  // start: 0x0000
      0xFF, 0xFF   // end: 0xFFFF
  );

  const auto kExpected1 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x04,        // request: find information
      0x00, 0x00,  // handle: 0x0000 (start handle in request)
      0x01         // error: Invalid handle
  );

  // End handle is smaller than start handle
  const auto kInvalidEndHandle = CreateStaticByteBuffer(
      0x04,        // opcode: find information
      0x02, 0x00,  // start: 0x0002
      0x01, 0x00   // end: 0x0001
  );

  const auto kExpected2 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x04,        // request: find information
      0x02, 0x00,  // handle: 0x0002 (start handle in request)
      0x01         // error: Invalid handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidStartHandle, kExpected1));
  EXPECT_TRUE(ReceiveAndExpect(kInvalidEndHandle, kExpected2));
}

TEST_F(ServerTest, FindInformationAttributeNotFound) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x04,        // opcode: find information request
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF   // end: 0xFFFF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x04,        // request: find information
      0x01, 0x00,  // handle: 0x0001 (start handle in request)
      0x0A         // error: Attribute not found
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindInformation16) {
  auto* grp = db()->NewGrouping(types::kPrimaryService, 2, kTestValue1);
  grp->AddAttribute(kTestType16);
  grp->AddAttribute(kTestType16);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x04,        // opcode: find information request
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF   // end: 0xFFFF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x05,        // opcode: find information response
      0x01,        // format: 16-bit
      0x01, 0x00,  // handle: 0x0001
      0x00, 0x28,  // uuid: primary service group type
      0x02, 0x00,  // handle: 0x0002
      0xEF, 0xBE,  // uuid: 0xBEEF
      0x03, 0x00,  // handle: 0x0003
      0xEF, 0xBE   // uuid: 0xBEEF
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindInformation128) {
  auto* grp = db()->NewGrouping(kTestType128, 0, kTestValue1);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x04,        // opcode: find information request
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF   // end: 0xFFFF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x05,        // opcode: find information response
      0x02,        // format: 128-bit
      0x01, 0x00,  // handle: 0x0001

      // uuid: 0F0E0D0C-0B0A-0908-0706-050403020100
      0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
      0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F);
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindByTypeValueSuccess) {
  // handle: 1 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // handle: 2 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue2)->set_active(true);

  // handle: 3 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x06,          // opcode: find by type value request
      0x01, 0x00,    // start: 0x0001
      0xFF, 0xFF,    // end: 0xFFFF
      0x00, 0x28,    // uuid: primary service group type
      'f', 'o', 'o'  // value: foo
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x07,        // opcode: find by type value response
      0x01, 0x00,  // handle: 0x0001
      0x01, 0x00,  // group handle: 0x0001
      0x03, 0x00,  // handle: 0x0003
      0x03, 0x00   // group handle: 0x0003
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindByTypeValueFail) {
  // handle: 1 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x06,          // opcode: find by type value request
      0x01, 0x00,    // start: 0x0001
      0xFF, 0xFF,    // end: 0xFFFF
      0x00, 0x28,    // uuid: primary service group type
      'n', 'o'       // value: no
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,          // Error
      0x06,          // opcode: find by type value
      0x00, 0x00,    // group handle: 0x0000
      0x0a           // Attribute Not Found
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindByTypeValueEmptyDB) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x06,          // opcode: find by type value request
      0x01, 0x00,    // start: 0x0001
      0xFF, 0xFF,    // end: 0xFFFF
      0x00, 0x28,    // uuid: primary service group type
      'n', 'o'       // value: no
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,          // Error
      0x06,          // opcode: find by type value
      0x00, 0x00,    // group handle: 0x0000
      0x0a           // Attribute Not Found
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindByTypeValueInvalidHandle) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x06,          // opcode: find by type value request
      0x02, 0x00,    // start: 0x0002
      0x01, 0x00,    // end: 0x0001
      0x00, 0x28,    // uuid: primary service group type
      'n', 'o'       // value: no
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,          // Error
      0x06,          // opcode: find by type value
      0x00, 0x00,    // group handle: 0x0000
      0x01           // Invalid Handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindByTypeValueInvalidPDUError) {
  // handle: 1 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x06);

  const auto kExpected = CreateStaticByteBuffer(
      0x01,          // Error
      0x06,          // opcode: find by type value
      0x00, 0x00,    // group handle: 0x0000
      0x04           // Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, FindByTypeValueZeroLengthValueError) {
  // handle: 1 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x06,          // opcode: find by type value request
      0x01, 0x00,    // start: 0x0001
      0xFF, 0xFF,    // end: 0xFFFF
      0x00, 0x28     // uuid: primary service group type
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,          // Error
      0x06,          // opcode: find by type value
      0x00, 0x00,    // group handle: 0x0000
      0x0a           // Attribute Not Found
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindByTypeValueOutsideRangeError) {
  // handle: 1 (active)
  auto* grp = db()->NewGrouping(kTestType16, 2, kTestValue2);

  // handle: 2 - value: "long"
  grp->AddAttribute(kTestType16, AllowedNoSecurity())->SetValue(kTestValue2);

  // handle: 3 - value: "foo"
  grp->AddAttribute(kTestType16, AllowedNoSecurity())->SetValue(kTestValue1);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x06,          // opcode: find by type value request
      0x01, 0x00,    // start: 0x0001
      0x02, 0x00,    // end: 0xFFFF
      0x00, 0x28,    // uuid: primary service group type
      'f', 'o', 'o'  // value: foo
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,          // Error
      0x06,          // opcode: find by type value
      0x00, 0x00,    // group handle: 0x0000
      0x0a           // Attribute Not Found
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindInfomationInactive) {
  // handle: 1 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // handle: 2, 3, 4 (inactive)
  auto* grp = db()->NewGrouping(types::kPrimaryService, 2, kTestValue1);
  grp->AddAttribute(kTestType16);
  grp->AddAttribute(kTestType16);

  // handle: 5 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x04,        // opcode: find information request
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF   // end: 0xFFFF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x05,        // opcode: find information response
      0x01,        // format: 16-bit
      0x01, 0x00,  // handle: 0x0001
      0x00, 0x28,  // uuid: primary service group type
      0x05, 0x00,  // handle: 0x0005
      0x00, 0x28  // uuid: primary service group type
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, FindInfomationRange) {
  auto* grp = db()->NewGrouping(types::kPrimaryService, 2, kTestValue1);
  grp->AddAttribute(kTestType16);
  grp->AddAttribute(kTestType16);
  grp->set_active(true);

  // handle: 5 (active)
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x04,        // opcode: find information request
      0x02, 0x00,  // start: 0x0002
      0x02, 0x00   // end: 0x0002
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x05,        // opcode: find information response
      0x01,        // format: 16-bit
      0x02, 0x00,  // handle: 0x0001
      0xEF, 0xBE   // uuid: 0xBEEF
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByGroupTypeInvalidPDU) {
  // Just opcode
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x10);
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x10,        // request: read by group type
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, ReadByGroupTypeUnsupportedGroupType) {
  // 16-bit UUID
  // clang-format off
  const auto kUsing16BitType = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0x01, 0x00   // group type: 1 (unsupported)
  );

  // 128-bit UUID
  const auto kUsing128BitType = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF

      // group type: 00112233-4455-6677-8899-AABBCCDDEEFF (unsupported)
      0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA, 0x99, 0x88,
      0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00);

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x10,        // request: read by group type
      0x01, 0x00,  // handle: 0x0001 (start handle in request)
      0x10         // error: Unsupported Group Type
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kUsing16BitType, kExpected));
  EXPECT_TRUE(ReceiveAndExpect(kUsing128BitType, kExpected));
}

TEST_F(ServerTest, ReadByGroupTypeInvalidHandle) {
  // Start handle is 0
  // clang-format off
  const auto kInvalidStartHandle = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x00, 0x00,  // start: 0x0000
      0xFF, 0xFF,  // end: 0xFFFF
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected1 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x10,        // request: read by group type
      0x00, 0x00,  // handle: 0x0000 (start handle in request)
      0x01         // error: Invalid handle
  );

  // End handle is smaller than start handle
  const auto kInvalidEndHandle = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x02, 0x00,  // start: 0x0002
      0x01, 0x00,  // end: 0x0001
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected2 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x10,        // request: read by group type
      0x02, 0x00,  // handle: 0x0002 (start handle in request)
      0x01         // error: Invalid handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidStartHandle, kExpected1));
  EXPECT_TRUE(ReceiveAndExpect(kInvalidEndHandle, kExpected2));
}

TEST_F(ServerTest, ReadByGroupTypeAttributeNotFound) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x10,        // request: read by group type
      0x01, 0x00,  // handle: 0x0001 (start handle in request)
      0x0A         // error: Attribute not found
  );
  // clang-format on

  // Database is empty.
  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));

  // Group type does not match.
  db()->NewGrouping(types::kSecondaryService, 0, kTestValue1)->set_active(true);
  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByGroupTypeSingle) {
  const auto kTestValue = CreateStaticByteBuffer('t', 'e', 's', 't');

  // Start: 1, end: 2
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  grp->AddAttribute(UUID(), att::AccessRequirements(), att::AccessRequirements());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x11,               // opcode: read by group type response
      0x08,               // length: 8 (strlen("test") + 4)
      0x01, 0x00,         // start: 0x0001
      0x02, 0x00,         // end: 0x0002
      't', 'e', 's', 't'  // value: "test"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByGroupTypeSingle128) {
  const auto kTestValue = CreateStaticByteBuffer('t', 'e', 's', 't');

  // Start: 1, end: 2
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  grp->AddAttribute(UUID(), att::AccessRequirements(), att::AccessRequirements());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF

      // group type: 00002800-0000-1000-8000-00805F9B34FB (primary service)
      0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80,
      0x00, 0x10, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00);

  const auto kExpected = CreateStaticByteBuffer(
      0x11,               // opcode: read by group type response
      0x08,               // length: 8 (strlen("test") + 4)
      0x01, 0x00,         // start: 0x0001
      0x02, 0x00,         // end: 0x0002
      't', 'e', 's', 't'  // value: "test"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByGroupTypeSingleTruncated) {
  const auto kTestValue = CreateStaticByteBuffer('t', 'e', 's', 't');

  // Start: 1, end: 1
  auto* grp = db()->NewGrouping(types::kPrimaryService, 0, kTestValue);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x11,        // opcode: read by group type response
      0x06,        // length: 6 (strlen("te") + 4)
      0x01, 0x00,  // start: 0x0001
      0x01, 0x00,  // end: 0x0001
      't', 'e'     // value: "te"
  );
  // clang-format on

  // Force the MTU to exactly fit |kExpected| which partially contains
  // |kTestValue|.
  att()->set_mtu(kExpected.size());

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByGroupTypeMultipleSameValueSize) {
  // Start: 1, end: 1
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // Start: 2, end: 2
  auto* grp2 = db()->NewGrouping(types::kPrimaryService, 0, kTestValue2);
  grp2->set_active(true);

  // Start: 3, end: 3
  db()->NewGrouping(types::kSecondaryService, 0, kTestValue3)->set_active(true);

  // Start: 4, end: 4
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue3)->set_active(true);

  // Start: 5, end: 5
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue4)->set_active(true);

  // clang-format off
  const auto kRequest1 = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected1 = CreateStaticByteBuffer(
      0x11,           // opcode: read by group type response
      0x07,           // length: 7 (strlen("foo") + 4)
      0x01, 0x00,     // start: 0x0001
      0x01, 0x00,     // end: 0x0001
      'f', 'o', 'o',  // value: "foo"
      0x02, 0x00,     // start: 0x0002
      0x02, 0x00,     // end: 0x0002
      'b', 'a', 'r',  // value: "bar"
      0x04, 0x00,     // start: 0x0004
      0x04, 0x00,     // end: 0x0004
      'b', 'a', 'z'   // value: "baz"
  );
  // clang-format on

  // Set the MTU to be one byte too short to include the 5th attribute group.
  // The 3rd group is omitted as its group type does not match.
  att()->set_mtu(kExpected1.size() + 6);

  EXPECT_TRUE(ReceiveAndExpect(kRequest1, kExpected1));

  // Search a narrower range. Only two groups should be returned even with room
  // in MTU.
  // clang-format off
  const auto kRequest2 = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x02, 0x00,  // start: 0x0002
      0x04, 0x00,  // end: 0x0004
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected2 = CreateStaticByteBuffer(
      0x11,           // opcode: read by group type response
      0x07,           // length: 7 (strlen("foo") + 4)
      0x02, 0x00,     // start: 0x0002
      0x02, 0x00,     // end: 0x0002
      'b', 'a', 'r',  // value: "bar"
      0x04, 0x00,     // start: 0x0004
      0x04, 0x00,     // end: 0x0004
      'b', 'a', 'z'   // value: "baz"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest2, kExpected2));

  // Make the second group inactive. It should get omitted.
  // clang-format off
  const auto kExpected3 = CreateStaticByteBuffer(
      0x11,           // opcode: read by group type response
      0x07,           // length: 7 (strlen("foo") + 4)
      0x04, 0x00,     // start: 0x0004
      0x04, 0x00,     // end: 0x0004
      'b', 'a', 'z'   // value: "baz"
  );
  // clang-format on

  grp2->set_active(false);
  EXPECT_TRUE(ReceiveAndExpect(kRequest2, kExpected3));
}

TEST_F(ServerTest, ReadByGroupTypeMultipleVaryingLengths) {
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // Matching type but value of different size. The results will stop here.
  db()->NewGrouping(types::kPrimaryService, 0, kTestValueLong)->set_active(true);

  // Matching type and matching value length. This won't be included as the
  // request will terminate at the second attribute.
  db()->NewGrouping(types::kPrimaryService, 0, kTestValue1)->set_active(true);

  // clang-format off
  const auto kRequest2 = CreateStaticByteBuffer(
      0x10,        // opcode: read by group type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected2 = CreateStaticByteBuffer(
      0x11,               // opcode: read by group type response
      0x08,               // length: 8 (strlen("long") + 4)
      0x01, 0x00,         // start: 0x0001
      0x01, 0x00,         // end: 0x0001
      'l', 'o', 'n', 'g'  // value: "bar"
  );
  // clang-format on
}

TEST_F(ServerTest, ReadByTypeInvalidPDU) {
  // Just opcode
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x08);
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x08,        // request: read by type
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, ReadByTypeInvalidHandle) {
  // Start handle is 0
  // clang-format off
  const auto kInvalidStartHandle = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x00, 0x00,  // start: 0x0000
      0xFF, 0xFF,  // end: 0xFFFF
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected1 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x08,        // request: read by type
      0x00, 0x00,  // handle: 0x0000 (start handle in request)
      0x01         // error: Invalid handle
  );

  // End handle is smaller than start handle
  const auto kInvalidEndHandle = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x02, 0x00,  // start: 0x0002
      0x01, 0x00,  // end: 0x0001
      0x00, 0x28   // group type: 0x2800 (primary service)
  );

  const auto kExpected2 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x08,        // request: read by type
      0x02, 0x00,  // handle: 0x0002 (start handle in request)
      0x01         // error: Invalid handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidStartHandle, kExpected1));
  EXPECT_TRUE(ReceiveAndExpect(kInvalidEndHandle, kExpected2));
}

TEST_F(ServerTest, ReadByTypeAttributeNotFound) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x08,        // request: read by type
      0x01, 0x00,  // handle: 0x0001 (start handle in request)
      0x0A         // error: Attribute not found
  );
  // clang-format on

  // Database is empty.
  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));

  // Attribute type does not match.
  db()->NewGrouping(types::kSecondaryService, 0, kTestValue1)->set_active(true);
  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByTypeDynamicValueNoHandler) {
  const auto kTestValue = CreateStaticByteBuffer('t', 'e', 's', 't');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x08,        // request: read by type
      0x02, 0x00,  // handle: 0x0002 (the attribute causing the error)
      0x02         // error: Read not permitted
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByTypeDynamicValue) {
  auto* grp = db()->NewGrouping(types::kPrimaryService, 2, kTestValue1);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity());
  attr->set_read_handler(
      [attr](PeerId peer_id, auto handle, uint16_t offset, const auto& result_cb) {
        EXPECT_EQ(attr->handle(), handle);
        EXPECT_EQ(0u, offset);
        result_cb(att::ErrorCode::kNoError, CreateStaticByteBuffer('f', 'o', 'r', 'k'));
      });

  // Add a second dynamic attribute, which should be omitted.
  attr = grp->AddAttribute(kTestType16, AllowedNoSecurity());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x09,               // opcode: read by type response
      0x06,               // length: 6 (strlen("fork") + 2)
      0x02, 0x00,         // handle: 0x0002
      'f', 'o', 'r', 'k'  // value: "fork"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));

  // Assign a static value to the second attribute. It should still be omitted
  // as the first attribute is dynamic.
  attr->SetValue(kTestValue1);
  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByTypeDynamicValueError) {
  const auto kTestValue = CreateStaticByteBuffer('t', 'e', 's', 't');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  attr->set_read_handler([](PeerId peer_id, auto handle, uint16_t offset, const auto& result_cb) {
    result_cb(att::ErrorCode::kUnlikelyError, BufferView());
  });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x08,        // request: read by type
      0x02, 0x00,  // handle: 0x0002 (the attribute causing the error)
      0x0E         // error: Unlikely error
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByTypeSingle) {
  const auto kTestValue1 = CreateStaticByteBuffer('f', 'o', 'o');
  const auto kTestValue2 = CreateStaticByteBuffer('t', 'e', 's', 't');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue1);
  grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements())
      ->SetValue(kTestValue2);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x09,               // opcode: read by type response
      0x06,               // length: 6 (strlen("test") + 2)
      0x02, 0x00,         // handle: 0x0002
      't', 'e', 's', 't'  // value: "test"
  );

  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByTypeSingle128) {
  const auto kTestValue1 = CreateStaticByteBuffer('f', 'o', 'o');
  const auto kTestValue2 = CreateStaticByteBuffer('t', 'e', 's', 't');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue1);
  grp->AddAttribute(kTestType128, AllowedNoSecurity(), att::AccessRequirements())
      ->SetValue(kTestValue2);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF

      // type: 0F0E0D0C-0B0A-0908-0706-050403020100
      0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
      0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F);

  const auto kExpected = CreateStaticByteBuffer(
      0x09,               // opcode: read by type response
      0x06,               // length: 6 (strlen("test") + 2)
      0x02, 0x00,         // handle: 0x0002
      't', 'e', 's', 't'  // value: "test"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByTypeSingleTruncated) {
  const auto kVeryLongValue =
      CreateStaticByteBuffer('t', 'e', 's', 't', 'i', 'n', 'g', ' ', 'i', 's', ' ', 'f', 'u', 'n');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue1);
  grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements())
      ->SetValue(kVeryLongValue);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x09,          // opcode: read by type response
      0x05,          // length: 5 (strlen("tes") + 2)
      0x02, 0x00,    // handle: 0x0002
      't', 'e', 's'  // value: "tes"
  );
  // clang-format on

  // Force the MTU to exactly fit |kExpected| which partially contains
  // |kTestValue2| (the packet is crafted so that both |kRequest| and
  // |kExpected| fit within the MTU).
  att()->set_mtu(kExpected.size());

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

// When there are more than one matching attributes, the list should end at the
// first attribute that causes an error.
TEST_F(ServerTest, ReadByTypeMultipleExcludeFirstError) {
  // handle 1: readable
  auto* grp = db()->NewGrouping(kTestType16, 1, kTestValue1);

  // handle 2: not readable.
  grp->AddAttribute(kTestType16)->SetValue(kTestValue1);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x09,          // opcode: read by type response
      0x05,          // length: 5 (strlen("foo") + 2)
      0x01, 0x00,    // handle: 0x0001
      'f', 'o', 'o'  // value: "foo"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadByTypeMultipleSameValueSize) {
  // handle: 1, value: foo
  auto* grp = db()->NewGrouping(types::kPrimaryService, 2, kTestValue1);

  // handle: 2, value: foo
  grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements())
      ->SetValue(kTestValue1);

  // handle: 3, value: bar
  grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements())
      ->SetValue(kTestValue2);
  grp->set_active(true);

  // handle: 4, value: foo (new grouping)
  grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue1);

  // handle: 5, value: baz
  grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements())
      ->SetValue(kTestValue3);
  grp->set_active(true);

  // clang-format off
  const auto kRequest1 = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected1 = CreateStaticByteBuffer(
      0x09,           // opcode: read by type response
      0x05,           // length: 5 (strlen("foo") + 2)
      0x02, 0x00,     // handle: 0x0002
      'f', 'o', 'o',  // value: "foo"
      0x03, 0x00,     // handle: 0x0003
      'b', 'a', 'r',  // value: "bar"
      0x05, 0x00,     // handle: 0x0005
      'b', 'a', 'z'   // value: "baz"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest1, kExpected1));

  // Set the MTU 1 byte too short for |kExpected1|.
  att()->set_mtu(kExpected1.size() - 1);

  // clang-format off
  const auto kExpected2 = CreateStaticByteBuffer(
      0x09,           // opcode: read by type response
      0x05,           // length: 5 (strlen("foo") + 2)
      0x02, 0x00,     // handle: 0x0002
      'f', 'o', 'o',  // value: "foo"
      0x03, 0x00,     // handle: 0x0003
      'b', 'a', 'r'   // value: "bar"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest1, kExpected2));

  // Try a different range.
  // clang-format off
  const auto kRequest2 = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x03, 0x00,  // start: 0x0003
      0x05, 0x00,  // end: 0x0005
      0xEF, 0xBE   // type: 0xBEEF
  );

  const auto kExpected3 = CreateStaticByteBuffer(
      0x09,           // opcode: read by type response
      0x05,           // length: 5 (strlen("bar") + 2)
      0x03, 0x00,     // handle: 0x0003
      'b', 'a', 'r',  // value: "bar"
      0x05, 0x00,     // handle: 0x0005
      'b', 'a', 'z'   // value: "baz"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest2, kExpected3));

  // Make the second group inactive.
  grp->set_active(false);

  // clang-format off
  const auto kExpected4 = CreateStaticByteBuffer(
      0x09,           // opcode: read by type response
      0x05,           // length: 5 (strlen("bar") + 2)
      0x03, 0x00,     // handle: 0x0003
      'b', 'a', 'r'   // value: "bar"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest2, kExpected4));
}

// A response packet should only include consecutive attributes with the same
// value size.
TEST_F(ServerTest, ReadByTypeMultipleVaryingLengths) {
  // handle: 1 - value: "foo"
  auto* grp = db()->NewGrouping(kTestType16, 2, kTestValue1);

  // handle: 2 - value: "long"
  grp->AddAttribute(kTestType16, AllowedNoSecurity())->SetValue(kTestValueLong);

  // handle: 3 - value: "foo"
  grp->AddAttribute(kTestType16, AllowedNoSecurity())->SetValue(kTestValue1);
  grp->set_active(true);

  // Even though we have 3 attributes with a matching type, the requests below
  // will always return one attribute at a time as their values have different
  // sizes.

  // clang-format off
  const auto kRequest1 = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );
  const auto kExpected1 = CreateStaticByteBuffer(
      0x09,          // opcode: read by type response
      0x05,          // length: 5 (strlen("foo") + 2)
      0x01, 0x00,    // handle: 0x0001
      'f', 'o', 'o'  // value: "foo"
  );
  const auto kRequest2 = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x02, 0x00,  // start: 0x0002
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );
  const auto kExpected2 = CreateStaticByteBuffer(
      0x09,               // opcode: read by type response
      0x06,               // length: 6 (strlen("long") + 2)
      0x02, 0x00,         // handle: 0x0002
      'l', 'o', 'n', 'g'  // value: "long"
  );
  const auto kRequest3 = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x03, 0x00,  // start: 0x0003
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );
  const auto kExpected3 = CreateStaticByteBuffer(
      0x09,          // opcode: read by type response
      0x05,          // length: 5 (strlen("foo") + 2)
      0x03, 0x00,    // handle: 0x0003
      'f', 'o', 'o'  // value: "foo"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest1, kExpected1));
  EXPECT_TRUE(ReceiveAndExpect(kRequest2, kExpected2));
  EXPECT_TRUE(ReceiveAndExpect(kRequest3, kExpected3));
}

// When there are more than one matching attributes, the list should end at the
// first attribute with a dynamic value.
TEST_F(ServerTest, ReadByTypeMultipleExcludeFirstDynamic) {
  // handle: 1 - value: "foo"
  auto* grp = db()->NewGrouping(kTestType16, 1, kTestValue1);

  // handle: 2 - value: dynamic
  grp->AddAttribute(kTestType16, AllowedNoSecurity());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x08,        // opcode: read by type
      0x01, 0x00,  // start: 0x0001
      0xFF, 0xFF,  // end: 0xFFFF
      0xEF, 0xBE   // type: 0xBEEF
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x09,          // opcode: read by type response
      0x05,          // length: 5 (strlen("foo") + 2)
      0x01, 0x00,    // handle: 0x0001
      'f', 'o', 'o'  // value: "foo"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, WriteRequestInvalidPDU) {
  // Just opcode
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x12);
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x12,        // request: write request
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, WriteRequestInvalidHandle) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x12,        // opcode: write request
      0x01, 0x00,  // handle: 0x0001

      // value: "test"
      't', 'e', 's', 't');

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x12,        // request: write request
      0x01, 0x00,  // handle: 0x0001
      0x01         // error: invalid handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, WriteRequestSecurity) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);

  // Requires encryption
  grp->AddAttribute(kTestType16, att::AccessRequirements(),
                    att::AccessRequirements(/*encryption=*/true, /*authentication=*/false,
                                            /*authorization=*/false));
  grp->set_active(true);

  // We send two write requests:
  //   1. 0x0001: not writable
  //   2. 0x0002: writable but requires encryption
  //
  // clang-format off
  const auto kRequest1 = CreateStaticByteBuffer(
      0x12,        // opcode: write request
      0x01, 0x00,  // handle: 0x0001

      // value: "test"
      't', 'e', 's', 't');

  const auto kExpected1 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x12,        // request: write request
      0x01, 0x00,  // handle: 0x0001
      0x03         // error: write not permitted
  );
  const auto kRequest2 = CreateStaticByteBuffer(
      0x12,        // opcode: write request
      0x02, 0x00,  // handle: 0x0002

      // value: "test"
      't', 'e', 's', 't');

  const auto kExpected2 = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x12,        // request: write request
      0x02, 0x00,  // handle: 0x0002
      0x05         // error: insufficient authentication
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest1, kExpected1));
  EXPECT_TRUE(ReceiveAndExpect(kRequest2, kExpected2));
}

TEST_F(ServerTest, WriteRequestNoHandler) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);

  grp->AddAttribute(kTestType16, att::AccessRequirements(), AllowedNoSecurity());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x12,        // opcode: write request
      0x02, 0x00,  // handle: 0x0002

      // value: "test"
      't', 'e', 's', 't');

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x12,        // request: write request
      0x02, 0x00,  // handle: 0x0002
      0x03         // error: write not permitted
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, WriteRequestError) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, att::AccessRequirements(), AllowedNoSecurity());

  attr->set_write_handler([&](PeerId peer_id, att::Handle handle, uint16_t offset,
                              const auto& value, const auto& result_cb) {
    EXPECT_EQ(kTestPeerId, peer_id);
    EXPECT_EQ(attr->handle(), handle);
    EXPECT_EQ(0u, offset);
    EXPECT_TRUE(ContainersEqual(CreateStaticByteBuffer('t', 'e', 's', 't'), value));

    result_cb(att::ErrorCode::kUnlikelyError);
  });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x12,        // opcode: write request
      0x02, 0x00,  // handle: 0x0002

      // value: "test"
      't', 'e', 's', 't');

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x12,        // request: write request
      0x02, 0x00,  // handle: 0x0002
      0x0E         // error: unlikely error
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, WriteRequestSuccess) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, att::AccessRequirements(), AllowedNoSecurity());

  attr->set_write_handler([&](PeerId peer_id, att::Handle handle, uint16_t offset,
                              const auto& value, const auto& result_cb) {
    EXPECT_EQ(kTestPeerId, peer_id);
    EXPECT_EQ(attr->handle(), handle);
    EXPECT_EQ(0u, offset);
    EXPECT_TRUE(ContainersEqual(CreateStaticByteBuffer('t', 'e', 's', 't'), value));

    result_cb(att::ErrorCode::kNoError);
  });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x12,        // opcode: write request
      0x02, 0x00,  // handle: 0x0002

      // value: "test"
      't', 'e', 's', 't');
  // clang-format on

  // opcode: write response
  const auto kExpected = CreateStaticByteBuffer(0x13);

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

// TODO(bwb): Add test cases for the error conditions involved in a Write
// Command (fxbug.dev/675)

TEST_F(ServerTest, WriteCommandSuccess) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, att::AccessRequirements(), AllowedNoSecurity());

  attr->set_write_handler([&](PeerId peer_id, att::Handle handle, uint16_t offset,
                              const auto& value, const auto& result_cb) {
    EXPECT_EQ(kTestPeerId, peer_id);
    EXPECT_EQ(attr->handle(), handle);
    EXPECT_EQ(0u, offset);
    EXPECT_TRUE(ContainersEqual(CreateStaticByteBuffer('t', 'e', 's', 't'), value));
  });
  grp->set_active(true);

  // clang-format off
  const auto kCmd = CreateStaticByteBuffer(
      0x52,        // opcode: write command
      0x02, 0x00,  // handle: 0x0002
      't', 'e', 's', 't');
  // clang-format on

  fake_chan()->Receive(kCmd);
  RunLoopUntilIdle();
}

TEST_F(ServerTest, ReadRequestInvalidPDU) {
  // Just opcode
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x0A);
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0A,        // request: read request
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, ReadRequestInvalidHandle) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0A,       // opcode: read request
      0x01, 0x00  // handle: 0x0001
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0A,        // request: read request
      0x01, 0x00,  // handle: 0x0001
      0x01         // error: invalid handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadRequestSecurity) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);

  // Requires encryption
  grp->AddAttribute(kTestType16,
                    att::AccessRequirements(/*encryption=*/true, /*authentication=*/false,
                                            /*authorization=*/false),
                    att::AccessRequirements());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0A,       // opcode: read request
      0x02, 0x00  // handle: 0x0002
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0A,        // request: read request
      0x02, 0x00,  // handle: 0x0002
      0x05         // error: insufficient authentication
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadRequestCached) {
  const auto kDeclValue = CreateStaticByteBuffer('d', 'e', 'c', 'l');
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kDeclValue);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  attr->SetValue(kTestValue);
  grp->set_active(true);

  // clang-format off
  const auto kRequest1 = CreateStaticByteBuffer(
      0x0A,       // opcode: read request
      0x01, 0x00  // handle: 0x0001
  );
  const auto kExpected1 = CreateStaticByteBuffer(
      0x0B,               // opcode: read response
      'd', 'e', 'c', 'l'  // value: kDeclValue
  );
  const auto kRequest2 = CreateStaticByteBuffer(
      0x0A,       // opcode: read request
      0x02, 0x00  // handle: 0x0002
  );
  const auto kExpected2 = CreateStaticByteBuffer(
      0x0B,          // opcode: read response
      'f', 'o', 'o'  // value: kTestValue
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest1, kExpected1));
  EXPECT_TRUE(ReceiveAndExpect(kRequest2, kExpected2));
}

TEST_F(ServerTest, ReadRequestNoHandler) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);

  grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0A,       // opcode: read request
      0x02, 0x00  // handle: 0x0002
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0A,        // request: read request
      0x02, 0x00,  // handle: 0x0002
      0x02         // error: read not permitted
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadRequestError) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  attr->set_read_handler(
      [&](PeerId peer_id, att::Handle handle, uint16_t offset, const auto& result_cb) {
        EXPECT_EQ(kTestPeerId, peer_id);
        EXPECT_EQ(attr->handle(), handle);
        EXPECT_EQ(0u, offset);

        result_cb(att::ErrorCode::kUnlikelyError, BufferView());
      });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0A,       // opcode: read request
      0x02, 0x00  // handle: 0x0002
  );

  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0A,        // request: read request
      0x02, 0x00,  // handle: 0x0002
      0x0E         // error: unlikely error
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadBlobRequestInvalidPDU) {
  // Just opcode
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(0x0C);
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0C,        // request: read blob request
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, ReadBlobRequestDynamicSuccess) {
  const auto kDeclValue = CreateStaticByteBuffer('d', 'e', 'c', 'l');
  const auto kTestValue = CreateStaticByteBuffer(
      'A', ' ', 'V', 'e', 'r', 'y', ' ', 'L', 'o', 'n', 'g', ' ', 'D', 'e', 'v', 'i', 'c', 'e', ' ',
      'N', 'a', 'm', 'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L', 'o', 'n', 'g', ' ', 'A',
      't', 't', 'r', 'i', 'b', 'u', 't', 'e');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());

  attr->set_read_handler(
      [&](PeerId peer_id, att::Handle handle, uint16_t offset, const auto& result_cb) {
        EXPECT_EQ(kTestPeerId, peer_id);
        EXPECT_EQ(attr->handle(), handle);
        EXPECT_EQ(22u, offset);
        result_cb(att::ErrorCode::kNoError,
                  CreateStaticByteBuffer('e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L', 'o',
                                         'n', 'g', ' ', 'A', 't', 't', 'r', 'i', 'b', 'u'));
      });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0C,       // opcode: read blob request
      0x02, 0x00, // handle: 0x0002
      0x16, 0x00  // offset: 0x0016
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x0D,          // opcode: read blob response
      // Read Request response
      'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L',
      'o', 'n', 'g', ' ', 'A', 't', 't', 'r', 'i', 'b', 'u'
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadBlobDynamicRequestError) {
  const auto kTestValue = CreateStaticByteBuffer(
      'A', ' ', 'V', 'e', 'r', 'y', ' ', 'L', 'o', 'n', 'g', ' ', 'D', 'e', 'v', 'i', 'c', 'e', ' ',
      'N', 'a', 'm', 'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L', 'o', 'n', 'g', ' ', 'A',
      't', 't', 'r', 'i', 'b', 'u', 't', 'e');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  attr->set_read_handler(
      [&](PeerId peer_id, att::Handle handle, uint16_t offset, const auto& result_cb) {
        EXPECT_EQ(kTestPeerId, peer_id);
        EXPECT_EQ(attr->handle(), handle);

        result_cb(att::ErrorCode::kUnlikelyError, BufferView());
      });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0C,       // opcode: read blob request
      0x02, 0x00, // handle: 0x0002
      0x16, 0x00  // offset: 0x0016
      );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0C,        // request: read by type
      0x02, 0x00,  // handle: 0x0002 (the attribute causing the error)
      0x0E         // error: Unlikely error
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadBlobRequestStaticSuccess) {
  const auto kTestValue = CreateStaticByteBuffer(
      'A', ' ', 'V', 'e', 'r', 'y', ' ', 'L', 'o', 'n', 'g', ' ', 'D', 'e', 'v', 'i', 'c', 'e', ' ',
      'N', 'a', 'm', 'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L', 'o', 'n', 'g', ' ', 'A',
      't', 't', 'r', 'i', 'b', 'u', 't', 'e');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 0, kTestValue);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0C,       // opcode: read blob request
      0x01, 0x00, // handle: 0x0002
      0x16, 0x00  // offset: 0x0016
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x0D,          // opcode: read blob response
      // Read Request response
      'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L',
      'o', 'n', 'g', ' ', 'A', 't', 't', 'r', 'i', 'b', 'u'
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadBlobRequestStaticOverflowError) {
  const auto kTestValue = CreateStaticByteBuffer('s', 'h', 'o', 'r', 't', 'e', 'r');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 0, kTestValue);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0C,       // opcode: read blob request
      0x01, 0x00, // handle: 0x0001
      0x16, 0x10  // offset: 0x1016
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,       // Error
      0x0C,       // opcode
      0x01, 0x00, // handle: 0x0001
      0x07        // InvalidOffset
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadBlobRequestInvalidHandleError) {
  const auto kTestValue = CreateStaticByteBuffer(
      'A', ' ', 'V', 'e', 'r', 'y', ' ', 'L', 'o', 'n', 'g', ' ', 'D', 'e', 'v', 'i', 'c', 'e', ' ',
      'N', 'a', 'm', 'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L', 'o', 'n', 'g', ' ', 'A',
      't', 't', 'r', 'i', 'b', 'u', 't', 'e');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 0, kTestValue);
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0C,       // opcode: read blob request
      0x02, 0x30, // handle: 0x0002
      0x16, 0x00  // offset: 0x0016
      );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0C,        // request: read blob request
      0x02, 0x30,  // handle: 0x0001
      0x01         // error: invalid handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadBlobRequestNotPermitedError) {
  const auto kTestValue = CreateStaticByteBuffer(
      'A', ' ', 'V', 'e', 'r', 'y', ' ', 'L', 'o', 'n', 'g', ' ', 'D', 'e', 'v', 'i', 'c', 'e', ' ',
      'N', 'a', 'm', 'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L', 'o', 'n', 'g', ' ', 'A',
      't', 't', 'r', 'i', 'b', 'u', 't', 'e');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr =
      grp->AddAttribute(kTestType16, att::AccessRequirements(),
                        att::AccessRequirements(/*encryption=*/true, /*authentication=*/false,
                                                /*authorization=*/false));
  attr->set_read_handler(
      [&](PeerId peer_id, att::Handle handle, uint16_t offset, const auto& result_cb) {
        EXPECT_EQ(kTestPeerId, peer_id);
        EXPECT_EQ(attr->handle(), handle);

        result_cb(att::ErrorCode::kUnlikelyError, BufferView());
      });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0C,       // opcode: read blob request
      0x02, 0x00, // handle: 0x0002
      0x16, 0x00  // offset: 0x0016
      );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0C,        // request: read by type
      0x02, 0x00,  // handle: 0x0002 (the attribute causing the error)
      0x02         // error: Not Permitted
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadBlobRequestInvalidOffsetError) {
  const auto kTestValue = CreateStaticByteBuffer(
      'A', ' ', 'V', 'e', 'r', 'y', ' ', 'L', 'o', 'n', 'g', ' ', 'D', 'e', 'v', 'i', 'c', 'e', ' ',
      'N', 'a', 'm', 'e', ' ', 'U', 's', 'i', 'n', 'g', ' ', 'A', ' ', 'L', 'o', 'n', 'g', ' ', 'A',
      't', 't', 'r', 'i', 'b', 'u', 't', 'e');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  attr->set_read_handler(
      [&](PeerId peer_id, att::Handle handle, uint16_t offset, const auto& result_cb) {
        EXPECT_EQ(kTestPeerId, peer_id);
        EXPECT_EQ(attr->handle(), handle);

        result_cb(att::ErrorCode::kInvalidOffset, BufferView());
      });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0C,       // opcode: read blob request
      0x02, 0x00, // handle: 0x0002
      0x16, 0x40  // offset: 0x4016
      );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x0C,        // request: read by type
      0x02, 0x00,  // handle: 0x0002 (the attribute causing the error)
      0x07         // error: Invalid Offset Error
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, ReadRequestSuccess) {
  const auto kDeclValue = CreateStaticByteBuffer('d', 'e', 'c', 'l');
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);
  auto* attr = grp->AddAttribute(kTestType16, AllowedNoSecurity(), att::AccessRequirements());
  attr->set_read_handler(
      [&](PeerId peer_id, att::Handle handle, uint16_t offset, const auto& result_cb) {
        EXPECT_EQ(kTestPeerId, peer_id);
        EXPECT_EQ(attr->handle(), handle);
        EXPECT_EQ(0u, offset);

        result_cb(att::ErrorCode::kNoError, kTestValue);
      });
  grp->set_active(true);

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x0A,       // opcode: read request
      0x02, 0x00  // handle: 0x0002
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x0B,          // opcode: read response
      'f', 'o', 'o'  // value: kTestValue
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kExpected));
}

TEST_F(ServerTest, PrepareWriteRequestInvalidPDU) {
  // Payload is one byte too short.
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(
      0x16,        // opcode: prepare write request
      0x01, 0x00,  // handle: 0x0001
      0x01         // offset (should be 2 bytes).
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x16,        // request: prepare write request
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, PrepareWriteRequestInvalidHandle) {
  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x16,              // opcode: prepare write request
      0x01, 0x00,         // handle: 0x0001
      0x00, 0x00,         // offset: 0
      't', 'e', 's', 't'  // value: "test"
  );
  const auto kResponse = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x16,        // request: prepare write request
      0x01, 0x00,  // handle: 0x0001
      0x01         // error: invalid handle
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kResponse));
}

TEST_F(ServerTest, PrepareWriteRequestSucceeds) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);

  // No security requirement
  auto* attr =
      grp->AddAttribute(kTestType16, att::AccessRequirements(),
                        att::AccessRequirements(/*encryption=*/false, /*authentication=*/false,
                                                /*authorization=*/false));
  grp->set_active(true);

  int write_count = 0;
  attr->set_write_handler(
      [&](PeerId, att::Handle, uint16_t, const auto&, const auto&) { write_count++; });

  ASSERT_EQ(0x0002, attr->handle());

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x16,              // opcode: prepare write request
      0x02, 0x00,         // handle: 0x0002
      0x00, 0x00,         // offset: 0
      't', 'e', 's', 't'  // value: "test"
  );
  const auto kResponse = CreateStaticByteBuffer(
      0x17,              // opcode: prepare write response
      0x02, 0x00,         // handle: 0x0002
      0x00, 0x00,         // offset: 0
      't', 'e', 's', 't'  // value: "test"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kRequest, kResponse));

  // The attribute should not have been written yet.
  EXPECT_EQ(0, write_count);
}

TEST_F(ServerTest, PrepareWriteRequestPrepareQueueFull) {
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue);

  // No security requirement
  const auto* attr =
      grp->AddAttribute(kTestType16, att::AccessRequirements(),
                        att::AccessRequirements(/*encryption=*/false, /*authentication=*/false,
                                                /*authorization=*/false));
  grp->set_active(true);

  ASSERT_EQ(0x0002, attr->handle());

  // clang-format off
  const auto kRequest = CreateStaticByteBuffer(
      0x16,              // opcode: prepare write request
      0x02, 0x00,         // handle: 0x0002
      0x00, 0x00,         // offset: 0
      't', 'e', 's', 't'  // value: "test"
  );
  const auto kSuccessResponse = CreateStaticByteBuffer(
      0x17,              // opcode: prepare write response
      0x02, 0x00,         // handle: 0x0002
      0x00, 0x00,         // offset: 0
      't', 'e', 's', 't'  // value: "test"
  );
  const auto kErrorResponse = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x16,        // request: prepare write request
      0x02, 0x00,  // handle: 0x0002
      0x09         // error: prepare queue full
  );
  // clang-format on

  // Write requests should succeed until capacity is filled.
  for (unsigned i = 0; i < att::kPrepareQueueMaxCapacity; i++) {
    ASSERT_TRUE(ReceiveAndExpect(kRequest, kSuccessResponse))
        << "Unexpected failure at attempt: " << i;
  }

  // The next request should fail with a capacity error.
  EXPECT_TRUE(ReceiveAndExpect(kRequest, kErrorResponse));
}

TEST_F(ServerTest, ExecuteWriteMalformedPayload) {
  // Payload is one byte too short.
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(
      0x18  // opcode: execute write request
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x18,        // request: execute write request
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

TEST_F(ServerTest, ExecuteWriteInvalidFlag) {
  // Payload is one byte too short.
  // clang-format off
  const auto kInvalidPDU = CreateStaticByteBuffer(
      0x18,  // opcode: execute write request
      0xFF   // flag: invalid
  );
  const auto kExpected = CreateStaticByteBuffer(
      0x01,        // opcode: error response
      0x18,        // request: execute write request
      0x00, 0x00,  // handle: 0
      0x04         // error: Invalid PDU
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kInvalidPDU, kExpected));
}

// Tests that an "execute write request" without any prepared writes returns
// success without writing to any attributes.
TEST_F(ServerTest, ExecuteWriteQueueEmpty) {
  // clang-format off
  const auto kExecute = CreateStaticByteBuffer(
    0x18,  // opcode: execute write request
    0x01   // flag: "write pending"
  );
  const auto kExecuteResponse = CreateStaticByteBuffer(
    0x19  // opcode: execute write response
  );
  // clang-format on

  // |buffer| should contain the partial writes.
  EXPECT_TRUE(ReceiveAndExpect(kExecute, kExecuteResponse));
}

TEST_F(ServerTest, ExecuteWriteSuccess) {
  auto buffer = CreateStaticByteBuffer('x', 'x', 'x', 'x', 'x', 'x');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue1);
  auto* attr = grp->AddAttribute(kTestType16, att::AccessRequirements(), AllowedNoSecurity());
  attr->set_write_handler([&](const auto& peer_id, att::Handle handle, uint16_t offset,
                              const auto& value, const auto& result_cb) {
    EXPECT_EQ(kTestPeerId, peer_id);
    EXPECT_EQ(attr->handle(), handle);

    // Write the contents into |buffer|.
    buffer.Write(value, offset);
    result_cb(att::ErrorCode::kNoError);
  });
  grp->set_active(true);

  // Prepare two partial writes of the string "hello!".
  // clang-format off
  const auto kPrepare1 = CreateStaticByteBuffer(
    0x016,              // opcode: prepare write request
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    'h', 'e', 'l', 'l'  // value: "hell"
  );
  const auto kPrepareResponse1 = CreateStaticByteBuffer(
    0x017,              // opcode: prepare write response
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    'h', 'e', 'l', 'l'  // value: "hell"
  );
  const auto kPrepare2 = CreateStaticByteBuffer(
    0x016,              // opcode: prepare write request
    0x02, 0x00,         // handle: 0x0002
    0x04, 0x00,         // offset: 4
    'o', '!'            // value: "o!"
  );
  const auto kPrepareResponse2 = CreateStaticByteBuffer(
    0x017,              // opcode: prepare write response
    0x02, 0x00,         // handle: 0x0002
    0x04, 0x00,         // offset: 4
    'o', '!'            // value: "o!"
  );

  // Add an overlapping write that partial overwrites data from previous
  // payloads.
  const auto kPrepare3 = CreateStaticByteBuffer(
    0x016,              // opcode: prepare write request
    0x02, 0x00,         // handle: 0x0002
    0x02, 0x00,         // offset: 2
    'r', 'p', '?'       // value: "rp?"
  );
  const auto kPrepareResponse3 = CreateStaticByteBuffer(
    0x017,              // opcode: prepare write response
    0x02, 0x00,         // handle: 0x0002
    0x02, 0x00,         // offset: 2
    'r', 'p', '?'       // value: "rp?"
  );

  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kPrepare1, kPrepareResponse1));
  EXPECT_TRUE(ReceiveAndExpect(kPrepare2, kPrepareResponse2));
  EXPECT_TRUE(ReceiveAndExpect(kPrepare3, kPrepareResponse3));

  // The writes should not be committed yet.
  EXPECT_EQ("xxxxxx", buffer.AsString());

  // clang-format off
  const auto kExecute = CreateStaticByteBuffer(
    0x18,  // opcode: execute write request
    0x01   // flag: "write pending"
  );
  const auto kExecuteResponse = CreateStaticByteBuffer(
    0x19  // opcode: execute write response
  );
  // clang-format on

  // |buffer| should contain the partial writes.
  EXPECT_TRUE(ReceiveAndExpect(kExecute, kExecuteResponse));
  EXPECT_EQ("herp?!", buffer.AsString());
}

// Tests that the rest of the queue is dropped if a prepared write fails.
TEST_F(ServerTest, ExecuteWriteError) {
  auto buffer = CreateStaticByteBuffer('x', 'x', 'x', 'x', 'x', 'x');

  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue1);
  auto* attr = grp->AddAttribute(kTestType16, att::AccessRequirements(), AllowedNoSecurity());
  attr->set_write_handler([&](const auto& peer_id, att::Handle handle, uint16_t offset,
                              const auto& value, const auto& result_cb) {
    EXPECT_EQ(kTestPeerId, peer_id);
    EXPECT_EQ(attr->handle(), handle);

    // Make the write to non-zero offsets fail (this corresponds to the second
    // partial write we prepare below.
    if (offset) {
      result_cb(att::ErrorCode::kUnlikelyError);
    } else {
      buffer.Write(value);
      result_cb(att::ErrorCode::kNoError);
    }
  });
  grp->set_active(true);

  // Prepare two partial writes of the string "hello!".
  // clang-format off
  const auto kPrepare1 = CreateStaticByteBuffer(
    0x016,              // opcode: prepare write request
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    'h', 'e', 'l', 'l'  // value: "hell"
  );
  const auto kPrepareResponse1 = CreateStaticByteBuffer(
    0x017,              // opcode: prepare write response
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    'h', 'e', 'l', 'l'  // value: "hell"
  );
  const auto kPrepare2 = CreateStaticByteBuffer(
    0x016,              // opcode: prepare write request
    0x02, 0x00,         // handle: 0x0002
    0x04, 0x00,         // offset: 4
    'o', '!'            // value: "o!"
  );
  const auto kPrepareResponse2 = CreateStaticByteBuffer(
    0x017,              // opcode: prepare write response
    0x02, 0x00,         // handle: 0x0002
    0x04, 0x00,         // offset: 4
    'o', '!'            // value: "o!"
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kPrepare1, kPrepareResponse1));
  EXPECT_TRUE(ReceiveAndExpect(kPrepare2, kPrepareResponse2));

  // The writes should not be committed yet.
  EXPECT_EQ("xxxxxx", buffer.AsString());

  // clang-format off
  const auto kExecute = CreateStaticByteBuffer(
    0x18,  // opcode: execute write request
    0x01   // flag: "write pending"
  );
  const auto kExecuteResponse = CreateStaticByteBuffer(
    0x01,        // opcode: error response
    0x18,        // request: execute write request
    0x02, 0x00,  // handle: 2 (the attribute in error)
    0x0E         // error: Unlikely Error (returned by callback above).
  );
  // clang-format on

  // Only the first partial write should have gone through as the second one
  // is expected to fail.
  EXPECT_TRUE(ReceiveAndExpect(kExecute, kExecuteResponse));
  EXPECT_EQ("hellxx", buffer.AsString());
}

TEST_F(ServerTest, ExecuteWriteAbort) {
  auto* grp = db()->NewGrouping(types::kPrimaryService, 1, kTestValue1);
  // |attr| has handle "2".
  auto* attr = grp->AddAttribute(kTestType16, att::AccessRequirements(), AllowedNoSecurity());

  int write_count = 0;
  attr->set_write_handler([&](const auto& peer_id, att::Handle handle, uint16_t offset,
                              const auto& value, const auto& result_cb) {
    write_count++;

    EXPECT_EQ(kTestPeerId, peer_id);
    EXPECT_EQ(attr->handle(), handle);
    EXPECT_EQ(0u, offset);
    EXPECT_TRUE(ContainersEqual(CreateStaticByteBuffer('l', 'o', 'l'), value));
    result_cb(att::ErrorCode::kNoError);
  });
  grp->set_active(true);

  // clang-format off
  const auto kPrepareToAbort = CreateStaticByteBuffer(
    0x016,              // opcode: prepare write request
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    't', 'e', 's', 't'  // value: "test"
  );
  const auto kPrepareToAbortResponse = CreateStaticByteBuffer(
    0x017,              // opcode: prepare write response
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    't', 'e', 's', 't'  // value: "test"
  );
  // clang-format on

  // Prepare writes. These should get committed right away.
  EXPECT_TRUE(ReceiveAndExpect(kPrepareToAbort, kPrepareToAbortResponse));
  EXPECT_TRUE(ReceiveAndExpect(kPrepareToAbort, kPrepareToAbortResponse));
  EXPECT_TRUE(ReceiveAndExpect(kPrepareToAbort, kPrepareToAbortResponse));
  EXPECT_TRUE(ReceiveAndExpect(kPrepareToAbort, kPrepareToAbortResponse));
  EXPECT_EQ(0, write_count);

  // Abort the writes. They should get dropped.
  // clang-format off
  const auto kAbort = CreateStaticByteBuffer(
    0x18,  // opcode: execute write request
    0x00   // flag: "cancel all"
  );
  const auto kAbortResponse = CreateStaticByteBuffer(
    0x19  // opcode: execute write response
  );
  // clang-format on
  EXPECT_TRUE(ReceiveAndExpect(kAbort, kAbortResponse));
  EXPECT_EQ(0, write_count);

  // Prepare and commit a new write request. This one should take effect without
  // involving the previously aborted writes.
  // clang-format off
  const auto kPrepareToCommit = CreateStaticByteBuffer(
    0x016,              // opcode: prepare write request
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    'l', 'o', 'l'       // value: "lol"
  );
  const auto kPrepareToCommitResponse = CreateStaticByteBuffer(
    0x017,              // opcode: prepare write response
    0x02, 0x00,         // handle: 0x0002
    0x00, 0x00,         // offset: 0
    'l', 'o', 'l'       // value: "lol"
  );
  const auto kCommit = CreateStaticByteBuffer(
    0x18,  // opcode: execute write request
    0x01   // flag: "write pending"
  );
  const auto kCommitResponse = CreateStaticByteBuffer(
    0x19  // opcode: execute write response
  );
  // clang-format on

  EXPECT_TRUE(ReceiveAndExpect(kPrepareToCommit, kPrepareToCommitResponse));
  EXPECT_TRUE(ReceiveAndExpect(kCommit, kCommitResponse));
  EXPECT_EQ(1, write_count);
}

TEST_F(ServerTest, TrySendNotificationNoCccConfig) {
  IdType svc_id = RegisterSvcWithSingleChrc(kTestSvcType, kTestChrcId, kTestChrcType);
  const BufferView kTestValue;

  bool sent = false;
  auto send_cb = [&](auto) { sent = true; };
  fake_chan()->SetSendCallback(std::move(send_cb), dispatcher());

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(svc_id, kTestChrcId, kTestValue, /*indicate_cb=*/nullptr);
  });
  RunLoopUntilIdle();
  EXPECT_FALSE(sent);
}

TEST_F(ServerTest, TrySendNotificationConfiguredForIndicationsOnly) {
  SvcIdAndChrcHandle registered =
      RegisterSvcWithConfiguredChrc(kTestSvcType, kTestChrcId, kTestChrcType, kCCCIndicationBit);
  const BufferView kTestValue;

  bool sent = false;
  auto send_cb = [&](auto) { sent = true; };
  fake_chan()->SetSendCallback(std::move(send_cb), dispatcher());

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(registered.svc_id, kTestChrcId, kTestValue, /*indicate_cb=*/nullptr);
  });
  RunLoopUntilIdle();
  EXPECT_FALSE(sent);
}

TEST_F(ServerTest, SendNotificationEmpty) {
  SvcIdAndChrcHandle registered =
      RegisterSvcWithConfiguredChrc(kTestSvcType, kTestChrcId, kTestChrcType);
  const BufferView kTestValue;

  // clang-format off
  const StaticByteBuffer kExpected{
    att::kNotification,  // Opcode
    // Handle of the characteristic value being notified
    LowerBits(registered.chrc_val_handle), UpperBits(registered.chrc_val_handle)
  };
  // clang-format on

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(registered.svc_id, kTestChrcId, kTestValue, /*indicate_cb=*/nullptr);
  });

  EXPECT_TRUE(Expect(kExpected));
}

TEST_F(ServerTest, SendNotification) {
  SvcIdAndChrcHandle registered =
      RegisterSvcWithConfiguredChrc(kTestSvcType, kTestChrcId, kTestChrcType);
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');

  // clang-format off
  const StaticByteBuffer kExpected{
    att::kNotification,  // Opcode
    // Handle of the characteristic value being notified
    LowerBits(registered.chrc_val_handle), UpperBits(registered.chrc_val_handle),
    kTestValue[0], kTestValue[1], kTestValue[2]
  };
  // clang-format on

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(registered.svc_id, kTestChrcId, kTestValue.view(),
                         /*indicate_cb=*/nullptr);
  });

  EXPECT_TRUE(Expect(kExpected));
}

TEST_F(ServerTest, TrySendIndicationNoCccConfig) {
  IdType svc_id = RegisterSvcWithSingleChrc(kTestSvcType, kTestChrcId, kTestChrcType);
  const BufferView kTestValue;

  bool sent = false;
  auto send_cb = [&](auto) { sent = true; };
  fake_chan()->SetSendCallback(std::move(send_cb), dispatcher());

  att::Result<> indicate_res = fitx::ok();
  auto indicate_cb = [&](att::Result<> res) { indicate_res = res; };

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(svc_id, kTestChrcId, kTestValue, std::move(indicate_cb));
  });
  RunLoopUntilIdle();
  EXPECT_FALSE(sent);
  EXPECT_EQ(fitx::failed(), indicate_res);
}

TEST_F(ServerTest, TrySendIndicationConfiguredForNotificationsOnly) {
  SvcIdAndChrcHandle registered =
      RegisterSvcWithConfiguredChrc(kTestSvcType, kTestChrcId, kTestChrcType, kCCCNotificationBit);
  const BufferView kTestValue;

  bool sent = false;
  auto send_cb = [&](auto) { sent = true; };
  fake_chan()->SetSendCallback(std::move(send_cb), dispatcher());

  att::Result<> indicate_res = fitx::ok();
  auto indicate_cb = [&](att::Result<> res) { indicate_res = res; };

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(registered.svc_id, kTestChrcId, kTestValue, std::move(indicate_cb));
  });
  RunLoopUntilIdle();
  EXPECT_FALSE(sent);
  EXPECT_EQ(fitx::failed(), indicate_res);
}

TEST_F(ServerTest, SendIndicationEmpty) {
  SvcIdAndChrcHandle registered =
      RegisterSvcWithConfiguredChrc(kTestSvcType, kTestChrcId, kTestChrcType);
  const BufferView kTestValue;

  att::Result<> indicate_res = ToResult(HostError::kFailed);
  auto indicate_cb = [&](att::Result<> res) { indicate_res = res; };

  // clang-format off
  const StaticByteBuffer kExpected{
    att::kIndication,  // Opcode
    // Handle of the characteristic value being notified
    LowerBits(registered.chrc_val_handle), UpperBits(registered.chrc_val_handle)
  };
  // clang-format on

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(registered.svc_id, kTestChrcId, kTestValue, std::move(indicate_cb));
  });

  EXPECT_TRUE(Expect(kExpected));
  const StaticByteBuffer kIndicationConfirmation{att::kConfirmation};
  fake_chan()->Receive(kIndicationConfirmation);
  EXPECT_EQ(fitx::ok(), indicate_res);
}

TEST_F(ServerTest, SendIndication) {
  SvcIdAndChrcHandle registered =
      RegisterSvcWithConfiguredChrc(kTestSvcType, kTestChrcId, kTestChrcType);
  const auto kTestValue = CreateStaticByteBuffer('f', 'o', 'o');

  att::Result<> indicate_res = ToResult(HostError::kFailed);
  auto indicate_cb = [&](att::Result<> res) { indicate_res = res; };

  // clang-format off
  const StaticByteBuffer kExpected{
    att::kIndication,  // Opcode
    // Handle of the characteristic value being notified
    LowerBits(registered.chrc_val_handle), UpperBits(registered.chrc_val_handle),
    kTestValue[0], kTestValue[1], kTestValue[2]
  };
  // clang-format on

  async::PostTask(dispatcher(), [=] {
    server()->SendUpdate(registered.svc_id, kTestChrcId, kTestValue.view(), std::move(indicate_cb));
  });

  EXPECT_TRUE(Expect(kExpected));
  const StaticByteBuffer kIndicationConfirmation{att::kConfirmation};
  fake_chan()->Receive(kIndicationConfirmation);
  EXPECT_EQ(fitx::ok(), indicate_res);
}

class ServerTestSecurity : public ServerTest {
 protected:
  void InitializeAttributesForReading() {
    auto* grp = db()->NewGrouping(types::kPrimaryService, 4, kTestValue1);

    const att::AccessRequirements encryption(/*encryption=*/true, /*authentication=*/false,
                                             /*authorization=*/false);
    const att::AccessRequirements authentication(/*encryption=*/false, /*authentication=*/true,
                                                 /*authorization=*/false);
    const att::AccessRequirements authorization(/*encryption=*/false, /*authentication=*/false,
                                                /*authorization=*/true);

    not_permitted_attr_ = grp->AddAttribute(kTestType16);
    encryption_required_attr_ = grp->AddAttribute(kTestType16, encryption);
    authentication_required_attr_ = grp->AddAttribute(kTestType16, authentication);
    authorization_required_attr_ = grp->AddAttribute(kTestType16, authorization);

    // Assigns all tests attributes a static value. Intended to be used by read
    // requests. (Note: assigning a static value makes an attribute
    // non-writable). All attributes are assigned kTestValue1 as their static
    // value.
    not_permitted_attr_->SetValue(kTestValue1);
    encryption_required_attr_->SetValue(kTestValue1);
    authentication_required_attr_->SetValue(kTestValue1);
    authorization_required_attr_->SetValue(kTestValue1);

    grp->set_active(true);
  }

  void InitializeAttributesForWriting() {
    auto* grp = db()->NewGrouping(types::kPrimaryService, 4, kTestValue1);

    const att::AccessRequirements encryption(/*encryption=*/true, /*authentication=*/false,
                                             /*authorization=*/false);
    const att::AccessRequirements authentication(/*encryption=*/false, /*authentication=*/true,
                                                 /*authorization=*/false);
    const att::AccessRequirements authorization(/*encryption=*/false, /*authentication=*/false,
                                                /*authorization=*/true);

    auto write_handler = [this](const auto&, att::Handle, uint16_t, const auto& value,
                                auto responder) {
      write_count_++;
      if (responder) {
        responder(att::ErrorCode::kNoError);
      }
    };

    not_permitted_attr_ = grp->AddAttribute(kTestType16);
    not_permitted_attr_->set_write_handler(write_handler);

    encryption_required_attr_ =
        grp->AddAttribute(kTestType16, att::AccessRequirements(), encryption);
    encryption_required_attr_->set_write_handler(write_handler);

    authentication_required_attr_ =
        grp->AddAttribute(kTestType16, att::AccessRequirements(), authentication);
    authentication_required_attr_->set_write_handler(write_handler);

    authorization_required_attr_ =
        grp->AddAttribute(kTestType16, att::AccessRequirements(), authorization);
    authorization_required_attr_->set_write_handler(write_handler);

    grp->set_active(true);
  }

  auto MakeAttError(att::OpCode request, att::Handle handle, att::ErrorCode ecode) {
    return StaticByteBuffer(0x01,                                  // opcode: error response
                            request,                               // request opcode
                            LowerBits(handle), UpperBits(handle),  // handle
                            ecode                                  // error code
    );
  }

  // Blocks until an ATT Error Response PDU with the given parameters is
  // received from the fake channel (i.e. received FROM the ATT bearer).
  bool ExpectAttError(att::OpCode request, att::Handle handle, att::ErrorCode ecode) {
    return Expect(MakeAttError(request, handle, ecode));
  }

  // Helpers for emulating the receipt of an ATT read/write request PDU and
  // expecting back a security error. Expects a valid response if
  // |expected_ecode| is att::ErrorCode::kNoError.
  bool EmulateReadByTypeRequest(att::Handle handle, att::ErrorCode expected_ecode) {
    const auto kReadByTypeRequestPdu =
        StaticByteBuffer(0x08,  // opcode: read by type
                         LowerBits(handle),
                         UpperBits(handle),                     // start handle
                         LowerBits(handle), UpperBits(handle),  // end handle
                         0xEF, 0xBE);                           // type: 0xBEEF, i.e. kTestType16
    if (expected_ecode == att::ErrorCode::kNoError) {
      return ReceiveAndExpect(kReadByTypeRequestPdu,
                              StaticByteBuffer(0x09,  // opcode: read by type response
                                               0x05,  // length: 5 (strlen("foo") + 2)
                                               LowerBits(handle), UpperBits(handle),  // handle
                                               'f', 'o', 'o'  // value: "foo", i.e. kTestValue1
                                               ));
    } else {
      return ReceiveAndExpect(kReadByTypeRequestPdu, MakeAttError(0x08, handle, expected_ecode));
    }
  }

  bool EmulateReadBlobRequest(att::Handle handle, att::ErrorCode expected_ecode) {
    const auto kReadBlobRequestPdu =
        StaticByteBuffer(0x0C,                                  // opcode: read blob
                         LowerBits(handle), UpperBits(handle),  // handle
                         0x00, 0x00);                           // offset: 0
    if (expected_ecode == att::ErrorCode::kNoError) {
      return ReceiveAndExpect(kReadBlobRequestPdu,
                              StaticByteBuffer(0x0D,          // opcode: read blob response
                                               'f', 'o', 'o'  // value: "foo", i.e. kTestValue1
                                               ));
    } else {
      return ReceiveAndExpect(kReadBlobRequestPdu, MakeAttError(0x0C, handle, expected_ecode));
    }
  }

  bool EmulateReadRequest(att::Handle handle, att::ErrorCode expected_ecode) {
    const auto kReadRequestPdu = StaticByteBuffer(0x0A,  // opcode: read request
                                                  LowerBits(handle), UpperBits(handle));  // handle
    if (expected_ecode == att::ErrorCode::kNoError) {
      return ReceiveAndExpect(kReadRequestPdu,
                              StaticByteBuffer(0x0B,          // opcode: read response
                                               'f', 'o', 'o'  // value: "foo", i.e. kTestValue1
                                               ));
    } else {
      return ReceiveAndExpect(kReadRequestPdu, MakeAttError(0x0A, handle, expected_ecode));
    }
  }

  bool EmulateWriteRequest(att::Handle handle, att::ErrorCode expected_ecode) {
    const auto kWriteRequestPdu = StaticByteBuffer(0x12,  // opcode: write request
                                                   LowerBits(handle), UpperBits(handle),  // handle
                                                   't', 'e', 's', 't');  // value: "test"
    if (expected_ecode == att::ErrorCode::kNoError) {
      return ReceiveAndExpect(kWriteRequestPdu, StaticByteBuffer(0x13));  // write response
    } else {
      return ReceiveAndExpect(kWriteRequestPdu, MakeAttError(0x12, handle, expected_ecode));
    }
  }

  bool EmulatePrepareWriteRequest(att::Handle handle, att::ErrorCode expected_ecode) {
    const auto kPrepareWriteRequestPdu =
        StaticByteBuffer(0x16,                                  // opcode: prepare write request
                         LowerBits(handle), UpperBits(handle),  // handle
                         0x00, 0x00,                            // offset: 0
                         't', 'e', 's', 't'                     // value: "test"
        );
    if (expected_ecode == att::ErrorCode::kNoError) {
      return ReceiveAndExpect(kPrepareWriteRequestPdu,
                              StaticByteBuffer(0x17,  // prepare write response
                                               LowerBits(handle), UpperBits(handle),  // handle
                                               0x00, 0x00,                            // offset: 0
                                               't', 'e', 's', 't'  // value: "test"
                                               ));
    } else {
      return ReceiveAndExpect(kPrepareWriteRequestPdu, MakeAttError(0x16, handle, expected_ecode));
    }
  }

  // Emulates the receipt of a Write Command. The expected error code parameter
  // is unused since ATT commands do not have a response.
  bool EmulateWriteCommand(att::Handle handle, att::ErrorCode) {
    fake_chan()->Receive(CreateStaticByteBuffer(0x52,  // opcode: write command
                                                LowerBits(handle), UpperBits(handle),  // handle
                                                't', 'e', 's', 't'  // value: "test"
                                                ));
    RunLoopUntilIdle();
    return true;
  }

  template <bool (ServerTestSecurity::*EmulateMethod)(att::Handle, att::ErrorCode), bool IsWrite>
  void RunTest() {
    const att::ErrorCode kNotPermittedError =
        IsWrite ? att::ErrorCode::kWriteNotPermitted : att::ErrorCode::kReadNotPermitted;

    // No security.
    EXPECT_TRUE((this->*EmulateMethod)(not_permitted_attr()->handle(), kNotPermittedError));
    EXPECT_TRUE((this->*EmulateMethod)(encryption_required_attr()->handle(),
                                       att::ErrorCode::kInsufficientAuthentication));
    EXPECT_TRUE((this->*EmulateMethod)(authentication_required_attr()->handle(),
                                       att::ErrorCode::kInsufficientAuthentication));
    EXPECT_TRUE((this->*EmulateMethod)(authorization_required_attr()->handle(),
                                       att::ErrorCode::kInsufficientAuthentication));

    // Link encrypted.
    fake_chan()->set_security(
        sm::SecurityProperties(sm::SecurityLevel::kEncrypted, 16, /*secure_connections=*/false));
    EXPECT_TRUE((this->*EmulateMethod)(not_permitted_attr()->handle(), kNotPermittedError));
    EXPECT_TRUE((this->*EmulateMethod)(encryption_required_attr()->handle(),
                                       att::ErrorCode::kNoError));  // success
    EXPECT_TRUE((this->*EmulateMethod)(authentication_required_attr()->handle(),
                                       att::ErrorCode::kInsufficientAuthentication));
    EXPECT_TRUE((this->*EmulateMethod)(authorization_required_attr()->handle(),
                                       att::ErrorCode::kInsufficientAuthentication));

    // Link encrypted w/ MITM.
    fake_chan()->set_security(sm::SecurityProperties(sm::SecurityLevel::kAuthenticated, 16,
                                                     /*secure_connections=*/false));
    EXPECT_TRUE((this->*EmulateMethod)(not_permitted_attr()->handle(), kNotPermittedError));
    EXPECT_TRUE((this->*EmulateMethod)(encryption_required_attr()->handle(),
                                       att::ErrorCode::kNoError));  // success
    EXPECT_TRUE((this->*EmulateMethod)(authentication_required_attr()->handle(),
                                       att::ErrorCode::kNoError));  // success
    EXPECT_TRUE((this->*EmulateMethod)(authorization_required_attr()->handle(),
                                       att::ErrorCode::kNoError));  // success
  }

  void RunReadByTypeTest() { RunTest<&ServerTestSecurity::EmulateReadByTypeRequest, false>(); }
  void RunReadBlobTest() { RunTest<&ServerTestSecurity::EmulateReadBlobRequest, false>(); }
  void RunReadRequestTest() { RunTest<&ServerTestSecurity::EmulateReadRequest, false>(); }
  void RunWriteRequestTest() { RunTest<&ServerTestSecurity::EmulateWriteRequest, true>(); }
  void RunPrepareWriteRequestTest() {
    RunTest<&ServerTestSecurity::EmulatePrepareWriteRequest, true>();
  }
  void RunWriteCommandTest() { RunTest<&ServerTestSecurity::EmulateWriteCommand, true>(); }

  const att::Attribute* not_permitted_attr() const { return not_permitted_attr_; }
  const att::Attribute* encryption_required_attr() const { return encryption_required_attr_; }
  const att::Attribute* authentication_required_attr() const {
    return authentication_required_attr_;
  }
  const att::Attribute* authorization_required_attr() const { return authorization_required_attr_; }

  size_t write_count() const { return write_count_; }

 private:
  att::Attribute* not_permitted_attr_ = nullptr;
  att::Attribute* encryption_required_attr_ = nullptr;
  att::Attribute* authentication_required_attr_ = nullptr;
  att::Attribute* authorization_required_attr_ = nullptr;

  size_t write_count_ = 0u;
};

// Tests receiving a Read By Type error under 3 possible link security levels.
TEST_F(ServerTestSecurity, ReadByTypeErrorSecurity) {
  InitializeAttributesForReading();
  RunReadByTypeTest();
}

TEST_F(ServerTestSecurity, ReadBlobErrorSecurity) {
  InitializeAttributesForReading();
  RunReadBlobTest();
}

TEST_F(ServerTestSecurity, ReadErrorSecurity) {
  InitializeAttributesForReading();
  RunReadRequestTest();
}

TEST_F(ServerTestSecurity, WriteErrorSecurity) {
  InitializeAttributesForWriting();
  RunWriteRequestTest();

  // Only 4 writes should have gone through.
  EXPECT_EQ(4u, write_count());
}

TEST_F(ServerTestSecurity, WriteCommandErrorSecurity) {
  InitializeAttributesForWriting();
  RunWriteCommandTest();

  // Only 4 writes should have gone through.
  EXPECT_EQ(4u, write_count());
}

TEST_F(ServerTestSecurity, PrepareWriteRequestSecurity) {
  InitializeAttributesForWriting();
  RunPrepareWriteRequestTest();

  // None of the write handlers should have been called since no execute write
  // request has been sent.
  EXPECT_EQ(0u, write_count());
}

}  // namespace
}  // namespace bt::gatt
