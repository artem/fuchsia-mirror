// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/input/lib/hid-input-report/touch.h"

#include <lib/hid-parser/parser.h>
#include <lib/hid-parser/report.h>
#include <lib/hid-parser/units.h>
#include <lib/hid-parser/usages.h>
#include <stdint.h>

#include "src/ui/input/lib/hid-input-report/device.h"

namespace hid_input_report {

ParseResult TouchConfiguration::ParseReportDescriptor(
    const hid::ReportDescriptor& hid_report_descriptor) {
  hid::Attributes input_mode;
  bool has_input_mode = false;
  SelectiveReporting selective_reporting;
  bool has_selective_reporting = false;

  // Traverse up the nested collections to the Application collection.
  hid::Collection* main_collection = hid_report_descriptor.input_fields[0].col;
  while (main_collection != nullptr) {
    if (main_collection->type == hid::CollectionType::kApplication) {
      break;
    }
    main_collection = main_collection->parent;
  }
  if (!main_collection) {
    return ParseResult::kNoCollection;
  }

  if (main_collection->usage !=
      hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kTouchScreenConfiguration)) {
    return ParseResult::kNoCollection;
  }

  for (size_t i = 0; i < hid_report_descriptor.feature_count; i++) {
    const hid::ReportField field = hid_report_descriptor.feature_fields[i];
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kTouchScreenInputMode)) {
      input_mode = field.attr;
      has_input_mode = true;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kSurfaceSwitch)) {
      selective_reporting.surface_switch = field.attr;
      has_selective_reporting = true;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kButtonSwitch)) {
      selective_reporting.button_switch = field.attr;
      has_selective_reporting = true;
    }
  }

  if (!has_input_mode && !has_selective_reporting) {
    return ParseResult::kNoCollection;
  }

  // No error, write to class members.
  if (has_input_mode) {
    input_mode_.emplace(std::move(input_mode));
  }
  if (has_selective_reporting) {
    selective_reporting_.emplace(std::move(selective_reporting));
  }

  report_size_ = hid_report_descriptor.feature_byte_sz;
  report_id_ = hid_report_descriptor.report_id;

  return ParseResult::kOk;
}

ParseResult TouchConfiguration::CreateDescriptor(
    fidl::AnyArena& allocator, fuchsia_input_report::wire::DeviceDescriptor& descriptor) {
  if (!descriptor.has_touch()) {
    fuchsia_input_report::wire::TouchDescriptor touch(allocator);
    descriptor.set_touch(allocator, std::move(touch));
  }
  // Create feature descriptor
  if (input_mode_.has_value() || selective_reporting_.has_value()) {
    if (!descriptor.touch().has_feature()) {
      fuchsia_input_report::wire::TouchFeatureDescriptor feature(allocator);
      descriptor.touch().set_feature(allocator, std::move(feature));
    }

    if (input_mode_.has_value()) {
      descriptor.touch().feature().set_supports_input_mode(true);
    }
    if (selective_reporting_.has_value()) {
      descriptor.touch().feature().set_supports_selective_reporting(true);
    }
  }

  return ParseResult::kOk;
}

ParseResult TouchConfiguration::ParseFeatureReportInternal(
    const uint8_t* data, size_t len, fidl::AnyArena& allocator,
    fuchsia_input_report::wire::FeatureReport& feature_report) {
  if (len != report_size_) {
    return ParseResult::kReportSizeMismatch;
  }

  if (!feature_report.has_touch()) {
    fuchsia_input_report::wire::TouchFeatureReport touch(allocator);
    feature_report.set_touch(allocator, touch);
  }
  uint32_t val_out;
  if (input_mode_.has_value() && ExtractUint(data, len, *input_mode_, &val_out)) {
    feature_report.touch().set_input_mode(
        static_cast<fuchsia_input_report::wire::TouchConfigurationInputMode>(val_out));
  }
  if (selective_reporting_.has_value()) {
    fuchsia_input_report::wire::SelectiveReportingFeatureReport selective_reporting(allocator);
    if (ExtractUint(data, len, selective_reporting_->surface_switch, &val_out)) {
      selective_reporting.set_surface_switch(static_cast<bool>(val_out));
    }
    if (ExtractUint(data, len, selective_reporting_->button_switch, &val_out)) {
      selective_reporting.set_button_switch(static_cast<bool>(val_out));
    }
    feature_report.touch().set_selective_reporting(allocator, std::move(selective_reporting));
  }

  return ParseResult::kOk;
}

ParseResult TouchConfiguration::SetFeatureReportInternal(
    const fuchsia_input_report::wire::FeatureReport* report, uint8_t* data, size_t data_size,
    size_t* data_out_size) {
  if (!report->has_touch()) {
    return ParseResult::kItemNotFound;
  }
  if (!report->touch().has_input_mode() && !report->touch().has_selective_reporting()) {
    return ParseResult::kBadReport;
  }
  if (data_size < report_size_) {
    return ParseResult::kNoMemory;
  }
  for (size_t i = 0; i < data_size; i++) {
    data[i] = 0;
  }

  bool found = false;
  if (report->touch().has_input_mode() && input_mode_.has_value()) {
    if (!hid::InsertUint(data, data_size, *input_mode_,
                         static_cast<uint32_t>(report->touch().input_mode()))) {
      return ParseResult::kBadReport;
    }
    found = true;
  }

  if (report->touch().has_selective_reporting() && selective_reporting_.has_value()) {
    if (report->touch().selective_reporting().has_surface_switch()) {
      if (!hid::InsertUint(
              data, data_size, selective_reporting_->surface_switch,
              static_cast<uint32_t>(report->touch().selective_reporting().surface_switch()))) {
        return ParseResult::kBadReport;
      }
      found = true;
    }

    if (report->touch().selective_reporting().has_button_switch()) {
      if (!hid::InsertUint(
              data, data_size, selective_reporting_->button_switch,
              static_cast<uint32_t>(report->touch().selective_reporting().button_switch()))) {
        return ParseResult::kBadReport;
      }
      found = true;
    }
  }

  if (!found) {
    return ParseResult::kItemNotFound;
  }
  data[0] = report_id_;
  *data_out_size = report_size_;
  return ParseResult::kOk;
}

ParseResult Touch::ParseReportDescriptor(const hid::ReportDescriptor& hid_report_descriptor) {
  ContactConfig contacts[fuchsia_input_report::wire::kTouchMaxContacts];
  size_t num_contacts = 0;
  hid::Attributes buttons[fuchsia_input_report::wire::kTouchMaxNumButtons];
  uint8_t num_buttons = 0;

  // Traverse up the nested collections to the Application collection.
  hid::Collection* main_collection = hid_report_descriptor.input_fields[0].col;
  while (main_collection != nullptr) {
    if (main_collection->type == hid::CollectionType::kApplication) {
      break;
    }
    main_collection = main_collection->parent;
  }
  if (!main_collection) {
    return ParseResult::kNoCollection;
  }

  if (main_collection->usage ==
      hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kTouchScreen)) {
    touch_type_ = fuchsia_input_report::wire::TouchType::kTouchscreen;
  } else if (main_collection->usage ==
             hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kTouchPad)) {
    touch_type_ = fuchsia_input_report::wire::TouchType::kTouchpad;
  } else {
    return ParseResult::kNoCollection;
  }

  hid::Collection* finger_collection = nullptr;

  for (size_t i = 0; i < hid_report_descriptor.input_count; i++) {
    const hid::ReportField field = hid_report_descriptor.input_fields[i];

    // Process the global items.
    if (field.attr.usage.page == hid::usage::Page::kButton) {
      if (num_buttons == fuchsia_input_report::wire::kTouchMaxNumButtons) {
        return ParseResult::kTooManyItems;
      }
      buttons[num_buttons] = field.attr;
      num_buttons++;
    }

    // Process touch points. Don't process the item if it's not part of a touch point collection.
    if (field.col->usage !=
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kFinger)) {
      continue;
    }

    // If our collection pointer is different than the previous collection
    // pointer, we have started a new collection and are on a new touch point
    if (field.col != finger_collection) {
      finger_collection = field.col;
      num_contacts++;
    }

    if (num_contacts < 1) {
      return ParseResult::kNoCollection;
    }
    if (num_contacts > fuchsia_input_report::wire::kTouchMaxContacts) {
      return ParseResult::kTooManyItems;
    }
    ContactConfig* contact = &contacts[num_contacts - 1];

    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kContactID)) {
      contact->contact_id = field.attr;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kTipSwitch)) {
      contact->tip_switch = field.attr;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kGenericDesktop, hid::usage::GenericDesktop::kX)) {
      contact->position_x = field.attr;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kGenericDesktop, hid::usage::GenericDesktop::kY)) {
      contact->position_y = field.attr;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kTipPressure)) {
      contact->pressure = field.attr;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kWidth)) {
      contact->contact_width = field.attr;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kHeight)) {
      contact->contact_height = field.attr;
    }
    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kDigitizer, hid::usage::Digitizer::kConfidence)) {
      contact->confidence = field.attr;
    }
  }

  if (num_contacts == 0 && num_buttons == 0) {
    return ParseResult::kItemNotFound;
  }

  // No error, write to class members.
  for (size_t i = 0; i < num_contacts; i++) {
    contacts_[i] = contacts[i];
  }
  num_contacts_ = num_contacts;
  for (size_t i = 0; i < num_buttons; i++) {
    buttons_[i] = buttons[i];
  }
  num_buttons_ = num_buttons;

  report_size_ = hid_report_descriptor.input_byte_sz;
  report_id_ = hid_report_descriptor.report_id;

  return ParseResult::kOk;
}

ParseResult Touch::CreateDescriptor(fidl::AnyArena& allocator,
                                    fuchsia_input_report::wire::DeviceDescriptor& descriptor) {
  fuchsia_input_report::wire::TouchInputDescriptor input(allocator);

  input.set_touch_type(touch_type_);

  fidl::VectorView<fuchsia_input_report::wire::ContactInputDescriptor> input_contacts(
      allocator, num_contacts_);

  for (size_t i = 0; i < num_contacts_; i++) {
    const ContactConfig& config = contacts_[i];
    fuchsia_input_report::wire::ContactInputDescriptor contact(allocator);

    if (config.position_x) {
      contact.set_position_x(allocator, LlcppAxisFromAttribute(*config.position_x));
    }
    if (config.position_y) {
      contact.set_position_y(allocator, LlcppAxisFromAttribute(*config.position_y));
    }
    if (config.pressure) {
      contact.set_pressure(allocator, LlcppAxisFromAttribute(*config.pressure));
    }
    if (config.contact_width) {
      contact.set_contact_width(allocator, LlcppAxisFromAttribute(*config.contact_width));
    }
    if (config.contact_height) {
      contact.set_contact_height(allocator, LlcppAxisFromAttribute(*config.contact_height));
    }

    input_contacts[i] = std::move(contact);
  }

  input.set_contacts(allocator, std::move(input_contacts));

  // Set the buttons array.
  {
    fidl::VectorView<uint8_t> buttons(allocator, num_buttons_);
    size_t index = 0;
    for (auto& button : buttons_) {
      buttons[index++] = button.usage.usage;
    }
    input.set_buttons(allocator, std::move(buttons));
  }

  if (!descriptor.has_touch()) {
    fuchsia_input_report::wire::TouchDescriptor touch(allocator);
    descriptor.set_touch(allocator, std::move(touch));
  }
  descriptor.touch().set_input(allocator, std::move(input));

  return ParseResult::kOk;
}

ParseResult Touch::ParseInputReportInternal(const uint8_t* data, size_t len,
                                            fidl::AnyArena& allocator,
                                            fuchsia_input_report::wire::InputReport& input_report) {
  if (len != report_size_) {
    return ParseResult::kReportSizeMismatch;
  }
  fuchsia_input_report::wire::TouchInputReport touch(allocator);

  // Calculate the number of active contacts.
  size_t num_active_contacts = 0;
  for (size_t i = 0; i < num_contacts_; i++) {
    if (!contacts_[i].tip_switch) {
      num_active_contacts = num_contacts_;
      break;
    }
    double val_out_double;
    if (!ExtractAsUnitType(data, len, *contacts_[i].tip_switch, &val_out_double)) {
      continue;
    }
    if (static_cast<uint32_t>(val_out_double) != 0) {
      num_active_contacts++;
    }
  }

  fidl::VectorView<fuchsia_input_report::wire::ContactInputReport> input_contacts(
      allocator, num_active_contacts, num_contacts_);

  size_t contact_index = 0;
  for (size_t i = 0; i < num_contacts_; i++) {
    double val_out;

    if (contacts_[i].tip_switch) {
      if (ExtractAsUnitType(data, len, *contacts_[i].tip_switch, &val_out)) {
        if (static_cast<uint32_t>(val_out) == 0) {
          continue;
        }
      }
    }

    fuchsia_input_report::wire::ContactInputReport contact(allocator);

    if (contacts_[i].contact_id) {
      // Some touchscreens we support mistakenly set the logical range to 0-1 for the
      // tip switch and then never reset the range for the contact id. For this reason,
      // we have to do an "unconverted" extraction.
      uint32_t contact_id;
      if (hid::ExtractUint(data, len, *contacts_[i].contact_id, &contact_id)) {
        contact.set_contact_id(contact_id);
      }
    }

    if (contacts_[i].position_x) {
      contact.set_position_x(Extract<int64_t>(data, len, *contacts_[i].position_x, allocator));
    }
    if (contacts_[i].position_y) {
      contact.set_position_y(Extract<int64_t>(data, len, *contacts_[i].position_y, allocator));
    }
    if (contacts_[i].pressure) {
      contact.set_pressure(Extract<int64_t>(data, len, *contacts_[i].pressure, allocator));
    }
    if (contacts_[i].contact_width) {
      contact.set_contact_width(
          Extract<int64_t>(data, len, *contacts_[i].contact_width, allocator));
    }
    if (contacts_[i].contact_height) {
      contact.set_contact_height(
          Extract<int64_t>(data, len, *contacts_[i].contact_height, allocator));
    }
    uint32_t confidence;
    if (contacts_[i].confidence && ExtractUint(data, len, *contacts_[i].confidence, &confidence)) {
      contact.set_confidence(confidence);
    }

    input_contacts[contact_index++] = std::move(contact);
  }

  touch.set_contacts(allocator, std::move(input_contacts));

  // Parse Buttons.
  std::array<uint8_t, fuchsia_input_report::wire::kMouseMaxNumButtons> buttons;
  size_t buttons_size = 0;
  for (size_t i = 0; i < num_buttons_; i++) {
    double value_out;
    if (hid::ExtractAsUnitType(data, len, buttons_[i], &value_out)) {
      uint8_t pressed = (value_out > 0) ? 1 : 0;
      if (pressed) {
        buttons[buttons_size++] = static_cast<uint8_t>(buttons_[i].usage.usage);
      }
    }
  }

  fidl::VectorView<uint8_t> fidl_buttons(allocator, buttons_size);
  for (size_t i = 0; i < buttons_size; i++) {
    fidl_buttons[i] = buttons[i];
  }
  touch.set_pressed_buttons(allocator, std::move(fidl_buttons));

  input_report.set_touch(allocator, std::move(touch));

  return ParseResult::kOk;
}

}  // namespace hid_input_report
