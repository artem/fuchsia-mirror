// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/hidbus/c/banjo.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/defer.h>
#include <lib/hid/boot.h>

#include <virtio/input.h>
#include <zxtest/zxtest.h>

#include "input_kbd.h"
#include "input_mouse.h"
#include "input_touch.h"
#include "src/ui/input/lib/hid-input-report/keyboard.h"
#include "src/ui/input/lib/hid-input-report/mouse.h"

namespace virtio {

namespace {

void SendTouchEvent(HidTouch& touch, uint16_t type, uint16_t code, uint32_t value) {
  virtio_input_event_t event = {};
  event.type = type;
  event.code = code;
  event.value = value;

  touch.ReceiveEvent(&event);
}

}  // namespace

class VirtioInputTest : public zxtest::Test {};

TEST_F(VirtioInputTest, MultiTouchReportDescriptor) {
  virtio_input_absinfo_t x_info = {};
  virtio_input_absinfo_t y_info = {};
  HidTouch touch(x_info, y_info);

  // Assert that the report descriptor is correct.
  // In this case correct means a copy of the paradise touch report descriptor.
  uint8_t desc[HID_MAX_DESC_LEN];
  size_t desc_len;
  zx_status_t status =
      touch.GetDescriptor(HID_DESCRIPTION_TYPE_REPORT, desc, sizeof(desc), &desc_len);
  ASSERT_OK(status);

  size_t paradise_size;
  const uint8_t* paradise_desc = get_paradise_touch_report_desc(&paradise_size);
  ASSERT_EQ(paradise_size, desc_len);
  ASSERT_EQ(0, memcmp(paradise_desc, desc, desc_len));
}

TEST_F(VirtioInputTest, MultiTouchFingerEvents) {
  int VAL_MAX = 100;
  int X_VAL = 50;
  int Y_VAL = 100;
  virtio_input_absinfo_t x_info = {};
  x_info.min = 0;
  x_info.max = static_cast<uint16_t>(VAL_MAX);
  virtio_input_absinfo_t y_info = {};
  y_info.min = 0;
  y_info.max = static_cast<uint16_t>(VAL_MAX);
  HidTouch touch(x_info, y_info);

  // Assert that a single finger works.
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_SLOT, 0);
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_TRACKING_ID, 1);
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_POSITION_X,
                 static_cast<uint16_t>(X_VAL));
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_POSITION_Y,
                 static_cast<uint16_t>(Y_VAL));

  size_t paradise_size;
  const void* report = touch.GetReport(&paradise_size);
  const paradise_touch_t* paradise_touch = reinterpret_cast<const paradise_touch_t*>(report);

  ASSERT_EQ(sizeof(paradise_touch_t), paradise_size);
  ASSERT_EQ(1, paradise_touch->contact_count);
  ASSERT_EQ(PARADISE_FINGER_FLAGS_TSWITCH_MASK,
            paradise_touch->fingers[0].flags & PARADISE_FINGER_FLAGS_TSWITCH_MASK);
  ASSERT_EQ(X_VAL * PARADISE_X_MAX / VAL_MAX, paradise_touch->fingers[0].x);
  ASSERT_EQ(Y_VAL * PARADISE_Y_MAX / VAL_MAX, paradise_touch->fingers[0].y);

  ASSERT_EQ(0, paradise_touch->fingers[1].flags);
  ASSERT_EQ(0, paradise_touch->fingers[2].flags);
  ASSERT_EQ(0, paradise_touch->fingers[3].flags);
  ASSERT_EQ(0, paradise_touch->fingers[4].flags);

  // Assert that a second finger tracks.
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_SLOT, 1);
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_TRACKING_ID, 2);

  report = touch.GetReport(&paradise_size);
  paradise_touch = reinterpret_cast<const paradise_touch_t*>(report);

  ASSERT_EQ(sizeof(paradise_touch_t), paradise_size);
  ASSERT_EQ(2, paradise_touch->contact_count);

  ASSERT_EQ(PARADISE_FINGER_FLAGS_TSWITCH_MASK,
            paradise_touch->fingers[0].flags & PARADISE_FINGER_FLAGS_TSWITCH_MASK);
  ASSERT_EQ(PARADISE_FINGER_FLAGS_TSWITCH_MASK,
            paradise_touch->fingers[1].flags & PARADISE_FINGER_FLAGS_TSWITCH_MASK);
  ASSERT_EQ(0, paradise_touch->fingers[2].flags);
  ASSERT_EQ(0, paradise_touch->fingers[3].flags);
  ASSERT_EQ(0, paradise_touch->fingers[4].flags);

  // Pick up the second finger.

  // We don't send another SLOT event because we will rely on the slot already
  // being 1.
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_TRACKING_ID, -1);

  report = touch.GetReport(&paradise_size);
  paradise_touch = reinterpret_cast<const paradise_touch_t*>(report);

  ASSERT_EQ(sizeof(paradise_touch_t), paradise_size);
  ASSERT_EQ(1, paradise_touch->contact_count);

  ASSERT_EQ(PARADISE_FINGER_FLAGS_TSWITCH_MASK,
            paradise_touch->fingers[0].flags & PARADISE_FINGER_FLAGS_TSWITCH_MASK);
  ASSERT_EQ(0, paradise_touch->fingers[1].flags);
  ASSERT_EQ(0, paradise_touch->fingers[2].flags);
  ASSERT_EQ(0, paradise_touch->fingers[3].flags);
  ASSERT_EQ(0, paradise_touch->fingers[4].flags);

  // Out of bounds slot. GetReport values shouldn't change.
  int NEW_X_VAL = 100;
  int NEW_Y_VAL = 50;
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_SLOT, 5);
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_TRACKING_ID, 1);
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_POSITION_X,
                 static_cast<uint16_t>(NEW_X_VAL));
  SendTouchEvent(touch, VIRTIO_INPUT_EV_ABS, VIRTIO_INPUT_EV_MT_POSITION_Y,
                 static_cast<uint16_t>(NEW_Y_VAL));

  report = touch.GetReport(&paradise_size);
  paradise_touch = reinterpret_cast<const paradise_touch_t*>(report);

  ASSERT_EQ(sizeof(paradise_touch_t), paradise_size);
  ASSERT_EQ(1, paradise_touch->contact_count);
  ASSERT_EQ(PARADISE_FINGER_FLAGS_TSWITCH_MASK,
            paradise_touch->fingers[0].flags & PARADISE_FINGER_FLAGS_TSWITCH_MASK);
  ASSERT_EQ(X_VAL * PARADISE_X_MAX / VAL_MAX, paradise_touch->fingers[0].x);
  ASSERT_EQ(Y_VAL * PARADISE_Y_MAX / VAL_MAX, paradise_touch->fingers[0].y);

  ASSERT_EQ(0, paradise_touch->fingers[1].flags);
  ASSERT_EQ(0, paradise_touch->fingers[2].flags);
  ASSERT_EQ(0, paradise_touch->fingers[3].flags);
  ASSERT_EQ(0, paradise_touch->fingers[4].flags);
}

TEST_F(VirtioInputTest, MouseTest) {
  // Get the HID descriptor.
  HidMouse hid_mouse;
  uint8_t report_descriptor[2048] = {};
  size_t report_descriptor_size = 0;
  ASSERT_OK(hid_mouse.GetDescriptor(HID_DESCRIPTION_TYPE_REPORT, report_descriptor,
                                    sizeof(report_descriptor), &report_descriptor_size));

  // Parse the HID descriptor.
  hid::DeviceDescriptor* dev_desc = nullptr;
  auto parse_res = hid::ParseReportDescriptor(report_descriptor, report_descriptor_size, &dev_desc);
  ASSERT_EQ(parse_res, hid::ParseResult::kParseOk);
  ASSERT_EQ(1, dev_desc->rep_count);
  auto free_descriptor = fit::defer([dev_desc]() { hid::FreeDeviceDescriptor(dev_desc); });

  hid_input_report::Mouse mouse;
  ASSERT_EQ(hid_input_report::ParseResult::kOk, mouse.ParseReportDescriptor(dev_desc->report[0]));

  // Send the Virtio mouse keys.
  virtio_input_event_t event = {};
  event.type = VIRTIO_INPUT_EV_KEY;
  event.value = VIRTIO_INPUT_EV_KEY_PRESSED;

  event.code = 0x110;  // BTN_LEFT.
  hid_mouse.ReceiveEvent(&event);

  // Parse the HID report.
  size_t report_size;
  const uint8_t* report = hid_mouse.GetReport(&report_size);

  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);

    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              mouse.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_EQ(input_report.mouse().pressed_buttons().count(), 1U);
    EXPECT_EQ(input_report.mouse().pressed_buttons()[0], 1U);
  }

  // ------------------------------------------------------------------

  // Send another Virtio mouse key event.
  event.type = VIRTIO_INPUT_EV_KEY;
  event.value = VIRTIO_INPUT_EV_KEY_PRESSED;

  event.code = 0x111;  // BTN_RIGHT.
  hid_mouse.ReceiveEvent(&event);

  // Parse the HID report.
  report = hid_mouse.GetReport(&report_size);
  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);
    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              mouse.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_EQ(input_report.mouse().pressed_buttons().count(), 2U);
    EXPECT_EQ(input_report.mouse().pressed_buttons()[0], 1U);
    EXPECT_EQ(input_report.mouse().pressed_buttons()[1], 2U);
  }

  // ------------------------------------------------------------------

  // Release one Virtio mouse key.
  event.type = VIRTIO_INPUT_EV_KEY;
  event.value = VIRTIO_INPUT_EV_KEY_RELEASED;

  event.code = 0x110;  // BTN_LEFT.
  hid_mouse.ReceiveEvent(&event);

  // Parse the HID report.
  report = hid_mouse.GetReport(&report_size);
  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);
    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              mouse.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_EQ(input_report.mouse().pressed_buttons().count(), 1U);
    EXPECT_EQ(input_report.mouse().pressed_buttons()[0], 2U);
  }

  // ------------------------------------------------------------------

  // Send another Virtio mouse key event.
  event.type = VIRTIO_INPUT_EV_KEY;
  event.value = VIRTIO_INPUT_EV_KEY_PRESSED;
  event.code = 0x112;  // BTN_MID.
  hid_mouse.ReceiveEvent(&event);

  // Parse the HID report.
  report = hid_mouse.GetReport(&report_size);
  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);
    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              mouse.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_EQ(input_report.mouse().pressed_buttons().count(), 2U);
    EXPECT_EQ(input_report.mouse().pressed_buttons()[0], 2U);
    EXPECT_EQ(input_report.mouse().pressed_buttons()[1], 3U);
  }

  // ------------------------------------------------------------------

  // Send a Virtio mouse rel event on X axis.
  event.type = VIRTIO_INPUT_EV_REL;
  event.code = VIRTIO_INPUT_EV_REL_X;
  event.value = 0x0abc;
  hid_mouse.ReceiveEvent(&event);

  // Parse the HID report.
  report = hid_mouse.GetReport(&report_size);
  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);
    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              mouse.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_TRUE(input_report.mouse().has_movement_x());
    EXPECT_EQ(input_report.mouse().movement_x(), 0x0abc);
  }

  // ------------------------------------------------------------------

  // Send a Virtio mouse rel event on Y axis.
  event.type = VIRTIO_INPUT_EV_REL;
  event.code = VIRTIO_INPUT_EV_REL_Y;
  event.value = 0x0123;
  hid_mouse.ReceiveEvent(&event);

  // Parse the HID report.
  report = hid_mouse.GetReport(&report_size);
  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);
    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              mouse.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_TRUE(input_report.mouse().has_movement_y());
    EXPECT_EQ(input_report.mouse().movement_y(), 0x0123);
  }

  // ------------------------------------------------------------------

  // Send a Virtio mouse rel event on wheel.
  event.type = VIRTIO_INPUT_EV_REL;
  event.code = VIRTIO_INPUT_EV_REL_WHEEL;
  event.value = 0x0345;
  hid_mouse.ReceiveEvent(&event);

  // Parse the HID report.
  report = hid_mouse.GetReport(&report_size);
  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);
    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              mouse.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_TRUE(input_report.mouse().has_scroll_v());
    EXPECT_EQ(input_report.mouse().scroll_v(), 0x0345);
  }
}

TEST_F(VirtioInputTest, KeyboardTest) {
  // Get the HID descriptor.
  HidKeyboard hid_keyboard;
  uint8_t report_descriptor[2048] = {};
  size_t report_descriptor_size = 0;
  ASSERT_OK(hid_keyboard.GetDescriptor(HID_DESCRIPTION_TYPE_REPORT, report_descriptor,
                                       sizeof(report_descriptor), &report_descriptor_size));

  // Parse the HID descriptor.
  hid::DeviceDescriptor* dev_desc = nullptr;
  auto parse_res = hid::ParseReportDescriptor(report_descriptor, report_descriptor_size, &dev_desc);
  ASSERT_EQ(parse_res, hid::ParseResult::kParseOk);
  ASSERT_EQ(1, dev_desc->rep_count);
  auto free_descriptor = fit::defer([dev_desc]() { hid::FreeDeviceDescriptor(dev_desc); });

  hid_input_report::Keyboard keyboard;
  ASSERT_EQ(hid_input_report::ParseResult::kOk,
            keyboard.ParseReportDescriptor(dev_desc->report[0]));

  // Send the Virtio keys.
  virtio_input_event_t event = {};
  event.type = VIRTIO_INPUT_EV_KEY;
  event.value = VIRTIO_INPUT_EV_KEY_PRESSED;

  event.code = 42;  // LEFTSHIFT.
  hid_keyboard.ReceiveEvent(&event);

  event.code = 30;  // KEY_A
  hid_keyboard.ReceiveEvent(&event);

  event.code = 100;  // KEY_RIGHTALT
  hid_keyboard.ReceiveEvent(&event);

  event.code = 108;  // KEY_DOWN
  hid_keyboard.ReceiveEvent(&event);

  // Parse the HID report.
  size_t report_size;
  const uint8_t* report = hid_keyboard.GetReport(&report_size);

  {
    fidl::Arena<2048> report_allocator;
    fuchsia_input_report::wire::InputReport input_report(report_allocator);

    EXPECT_EQ(hid_input_report::ParseResult::kOk,
              keyboard.ParseInputReport(report, report_size, report_allocator, input_report));

    ASSERT_EQ(input_report.keyboard().pressed_keys3().count(), 4U);
    EXPECT_EQ(input_report.keyboard().pressed_keys3()[0], fuchsia_input::wire::Key::kLeftShift);
    EXPECT_EQ(input_report.keyboard().pressed_keys3()[1], fuchsia_input::wire::Key::kA);
    EXPECT_EQ(input_report.keyboard().pressed_keys3()[2], fuchsia_input::wire::Key::kRightAlt);
    EXPECT_EQ(input_report.keyboard().pressed_keys3()[3], fuchsia_input::wire::Key::kDown);
  }
}

}  // namespace virtio
