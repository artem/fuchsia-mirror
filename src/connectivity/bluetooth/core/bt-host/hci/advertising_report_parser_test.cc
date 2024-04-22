// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/advertising_report_parser.h"

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"

namespace bt::hci::test {
namespace {

TEST(AdvertisingReportParserTest, EmptyReport) {
  StaticByteBuffer bytes(0x3E, 0x02, 0x02, 0x00);

  auto event = EmbossEventPacket::New(bytes.size());
  event.mutable_data().Write(bytes);

  hci::AdvertisingReportParser parser(event);
  EXPECT_FALSE(parser.HasMoreReports());

  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;
  EXPECT_FALSE(parser.GetNextReport(&view, &rssi));
}

TEST(AdvertisingReportParserTest, SingleReportMalformed) {
  // clang-format off

  StaticByteBuffer bytes(
      0x3E, 0x0B, 0x02, 0x01,  // HCI event header and LE Meta Event params
      0x03, 0x02,              // event_type, address_type
      0x01, 0x02, 0x03, 0x04, 0x05, 0x06,  // address
      0x00                                 // |length_data|. RSSI is missing
      );

  // clang-format on

  auto event = EmbossEventPacket::New(bytes.size());
  event.mutable_data().Write(bytes);

  hci::AdvertisingReportParser parser(event);
  EXPECT_TRUE(parser.HasMoreReports());
  EXPECT_FALSE(parser.encountered_error());

  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;
  EXPECT_FALSE(parser.GetNextReport(&view, &rssi));
  EXPECT_TRUE(parser.encountered_error());
}

TEST(AdvertisingReportParserTest, SingleReportNoData) {
  // clang-format off

  StaticByteBuffer bytes(
      0x3E, 0x0C, 0x02, 0x01,  // HCI event header and LE Meta Event params
      0x03, 0x02,              // event_type, address_type
      0x01, 0x02, 0x03, 0x04, 0x05, 0x06,  // address
      0x00, 0x7F                           // |length_data|, RSSI
      );

  // clang-format on

  auto event = EmbossEventPacket::New(bytes.size());
  event.mutable_data().Write(bytes);

  AdvertisingReportParser parser(event);
  EXPECT_TRUE(parser.HasMoreReports());

  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;
  EXPECT_TRUE(parser.GetNextReport(&view, &rssi));
  EXPECT_EQ(
      pw::bluetooth::emboss::LEAdvertisingEventType::NON_CONNECTABLE_UNDIRECTED,
      view.event_type().Read());
  EXPECT_EQ(pw::bluetooth::emboss::LEAddressType::PUBLIC_IDENTITY,
            view.address_type().Read());
  EXPECT_EQ("06:05:04:03:02:01", DeviceAddressBytes(view.address()).ToString());
  EXPECT_EQ(0u, view.data_length().Read());
  EXPECT_EQ(hci_spec::kRSSIInvalid, rssi);

  // No other reports
  EXPECT_FALSE(parser.HasMoreReports());
  EXPECT_FALSE(parser.GetNextReport(&view, &rssi));

  EXPECT_FALSE(parser.encountered_error());
}

TEST(AdvertisingReportParserTest, ReportsValidInvalid) {
  // clang-format off

  StaticByteBuffer bytes(
      0x3E, 0x16, 0x02, 0x02,  // HCI event header and LE Meta Event params
      0x03, 0x02,              // event_type, address_type
      0x01, 0x02, 0x03, 0x04, 0x05, 0x06,  // address
      0x00, 0x7F,                          // |length_data|, RSSI
      0x03, 0x02,                          // event_type, address_type
      0x01, 0x02, 0x03, 0x04, 0x05, 0x06,  // address
      0x0A, 0x7F                           // malformed |length_data|, RSSI
      );

  // clang-format on

  auto event = EmbossEventPacket::New(bytes.size());
  event.mutable_data().Write(bytes);

  AdvertisingReportParser parser(event);
  EXPECT_TRUE(parser.HasMoreReports());
  EXPECT_FALSE(parser.encountered_error());

  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;
  EXPECT_TRUE(parser.GetNextReport(&view, &rssi));
  EXPECT_EQ(
      pw::bluetooth::emboss::LEAdvertisingEventType::NON_CONNECTABLE_UNDIRECTED,
      view.event_type().Read());
  EXPECT_EQ(pw::bluetooth::emboss::LEAddressType::PUBLIC_IDENTITY,
            view.address_type().Read());
  EXPECT_EQ("06:05:04:03:02:01", DeviceAddressBytes(view.address()).ToString());
  EXPECT_EQ(0u, view.data_length().Read());
  EXPECT_EQ(hci_spec::kRSSIInvalid, rssi);

  // There are more reports...
  EXPECT_TRUE(parser.HasMoreReports());
  EXPECT_FALSE(parser.encountered_error());

  // ...but the report is malformed.
  EXPECT_FALSE(parser.GetNextReport(&view, &rssi));
  EXPECT_TRUE(parser.encountered_error());
}

TEST(AdvertisingReportParserTest, ReportsAllValid) {
  // clang-format off

  StaticByteBuffer bytes(
      0x3E, 0x28, 0x02, 0x03,              // HCI event header and LE Meta Event params
      0x03, 0x02,                          // event_type, address_type
      0x01, 0x02, 0x03, 0x04, 0x05, 0x06,  // address
      0x00, 0x7F,                          // |length_data|, RSSI

      0x00, 0x01,                          // event_type, address_type
      0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C,  // address
      0x03, 0x01, 0x02, 0x03, 0x0F,        // |length_data|, data, RSSI

      0x01, 0x00,                          // event_type, address_type
      0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12,  // address
      0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,  // |length_data|, data
      0x01                                 // RSSI
      );

  // clang-format on

  auto event = EmbossEventPacket::New(bytes.size());
  event.mutable_data().Write(bytes);

  AdvertisingReportParser parser(event);
  EXPECT_TRUE(parser.HasMoreReports());

  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;
  EXPECT_TRUE(parser.GetNextReport(&view, &rssi));
  EXPECT_EQ(
      pw::bluetooth::emboss::LEAdvertisingEventType::NON_CONNECTABLE_UNDIRECTED,
      view.event_type().Read());
  EXPECT_EQ(pw::bluetooth::emboss::LEAddressType::PUBLIC_IDENTITY,
            view.address_type().Read());
  EXPECT_EQ("06:05:04:03:02:01", DeviceAddressBytes(view.address()).ToString());
  EXPECT_EQ(0u, view.data_length().Read());
  EXPECT_EQ(hci_spec::kRSSIInvalid, rssi);

  // There are more reports
  EXPECT_TRUE(parser.HasMoreReports());
  EXPECT_TRUE(parser.GetNextReport(&view, &rssi));
  EXPECT_EQ(pw::bluetooth::emboss::LEAdvertisingEventType::
                CONNECTABLE_AND_SCANNABLE_UNDIRECTED,
            view.event_type().Read());
  EXPECT_EQ(pw::bluetooth::emboss::LEAddressType::RANDOM,
            view.address_type().Read());
  EXPECT_EQ("0C:0B:0A:09:08:07", DeviceAddressBytes(view.address()).ToString());
  EXPECT_EQ(3, view.data_length().Read());
  EXPECT_TRUE(ContainersEqual(std::array<uint8_t, 3>{{0x01, 0x02, 0x03}},
                              view.data().BackingStorage().data(),
                              view.data_length().Read()));
  EXPECT_EQ(15, rssi);

  // There are more reports
  EXPECT_TRUE(parser.HasMoreReports());
  EXPECT_TRUE(parser.GetNextReport(&view, &rssi));
  EXPECT_EQ(pw::bluetooth::emboss::LEAdvertisingEventType::CONNECTABLE_DIRECTED,
            view.event_type().Read());
  EXPECT_EQ(pw::bluetooth::emboss::LEAddressType::PUBLIC,
            view.address_type().Read());
  EXPECT_EQ("12:11:10:0F:0E:0D", DeviceAddressBytes(view.address()).ToString());
  EXPECT_EQ(5, view.data_length().Read());
  EXPECT_TRUE(
      ContainersEqual(std::array<uint8_t, 5>{{0xFF, 0xFF, 0xFF, 0xFF, 0xFF}},
                      view.data().BackingStorage().data(),
                      view.data_length().Read()));
  EXPECT_EQ(1, rssi);

  // No more reports.
  EXPECT_FALSE(parser.HasMoreReports());
  EXPECT_FALSE(parser.GetNextReport(&view, &rssi));

  EXPECT_FALSE(parser.encountered_error());
}

TEST(AdvertisingReportParserTest, ReportCountLessThanPayloadSize) {
  // clang-format off

  StaticByteBuffer bytes(
      0x3E, 0x28, 0x02,  // HCI event header and LE Meta Event param
      0x01,              // Event count is 1, even though packet contains 3
      0x03, 0x02,        // event_type, address_type
      0x01, 0x02, 0x03, 0x04, 0x05, 0x06,  // address
      0x00, 0x7F,                          // |length_data|, RSSI

      0x00, 0x01,                          // event_type, address_type
      0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C,  // address
      0x03, 0x01, 0x02, 0x03, 0x0F,        // |length_data|, data, RSSI

      0x01, 0x00,                          // event_type, address_type
      0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12,  // address
      0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,  // |length_data|, data
      0x01                                 // RSSI
      );

  // clang-format on

  auto event = EmbossEventPacket::New(bytes.size());
  event.mutable_data().Write(bytes);

  AdvertisingReportParser parser(event);
  EXPECT_TRUE(parser.HasMoreReports());
  EXPECT_FALSE(parser.encountered_error());

  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;
  EXPECT_TRUE(parser.GetNextReport(&view, &rssi));
  EXPECT_EQ(
      pw::bluetooth::emboss::LEAdvertisingEventType::NON_CONNECTABLE_UNDIRECTED,
      view.event_type().Read());
  EXPECT_EQ(pw::bluetooth::emboss::LEAddressType::PUBLIC_IDENTITY,
            view.address_type().Read());
  EXPECT_EQ("06:05:04:03:02:01", DeviceAddressBytes(view.address()).ToString());
  EXPECT_EQ(0u, view.data_length().Read());
  EXPECT_EQ(hci_spec::kRSSIInvalid, rssi);

  // Since the packet is malformed (the event payload contains 3 reports while
  // the header states there is only 1) this should return false.
  EXPECT_FALSE(parser.encountered_error());
  EXPECT_FALSE(parser.HasMoreReports());
  EXPECT_TRUE(parser.encountered_error());

  EXPECT_FALSE(parser.GetNextReport(&view, &rssi));
  EXPECT_TRUE(parser.encountered_error());
}

TEST(AdvertisingReportParserTest, ReportCountGreaterThanPayloadSize) {
  // clang-format off

  StaticByteBuffer bytes(
      0x3E, 0x0C, 0x02,  // HCI event header and LE Meta Event param
      0x02,              // Event count is 2, even though packet contains 1
      0x03, 0x02,        // event_type, address_type
      0x01, 0x02, 0x03, 0x04, 0x05, 0x06,  // address
      0x00, 0x7F                           // |length_data|, RSSI
      );

  // clang-format on

  auto event = EmbossEventPacket::New(bytes.size());
  event.mutable_data().Write(bytes);

  AdvertisingReportParser parser(event);
  EXPECT_TRUE(parser.HasMoreReports());

  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;
  EXPECT_TRUE(parser.GetNextReport(&view, &rssi));
  EXPECT_EQ(
      pw::bluetooth::emboss::LEAdvertisingEventType::NON_CONNECTABLE_UNDIRECTED,
      view.event_type().Read());
  EXPECT_EQ(pw::bluetooth::emboss::LEAddressType::PUBLIC_IDENTITY,
            view.address_type().Read());
  EXPECT_EQ("06:05:04:03:02:01", DeviceAddressBytes(view.address()).ToString());
  EXPECT_EQ(0u, view.data_length().Read());
  EXPECT_EQ(hci_spec::kRSSIInvalid, rssi);

  EXPECT_FALSE(parser.encountered_error());

  // Since the packet is malformed (the event payload contains 1 report while
  // the header states there are 2) this should return false.
  EXPECT_FALSE(parser.HasMoreReports());
  EXPECT_FALSE(parser.GetNextReport(&view, &rssi));

  EXPECT_TRUE(parser.encountered_error());
}

}  // namespace
}  // namespace bt::hci::test
