// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/input/lib/hid-input-report/mouse.h"

#include <stdint.h>

#include <hid-parser/parser.h>
#include <hid-parser/report.h>
#include <hid-parser/units.h>
#include <hid-parser/usages.h>

#include "src/ui/input/lib/hid-input-report/device.h"

namespace hid_input_report {

ParseResult Mouse::ParseReportDescriptor(const hid::ReportDescriptor& hid_report_descriptor) {
  hid::Attributes movement_x = {};
  hid::Attributes movement_y = {};
  hid::Attributes position_x = {};
  hid::Attributes position_y = {};
  hid::Attributes scroll_v = {};
  hid::Attributes buttons[fuchsia_input_report::MOUSE_MAX_NUM_BUTTONS];
  uint8_t num_buttons = 0;

  MouseInputDescriptor mouse_descriptor = {};

  for (size_t i = 0; i < hid_report_descriptor.input_count; i++) {
    const hid::ReportField& field = hid_report_descriptor.input_fields[i];

    if (field.attr.usage ==
        hid::USAGE(hid::usage::Page::kGenericDesktop, hid::usage::GenericDesktop::kX)) {
      if (field.flags & hid::FieldTypeFlags::kAbsolute) {
        position_x = field.attr;
        mouse_descriptor.position_x = LlcppAxisFromAttribute(position_x);
      } else {
        movement_x = field.attr;
        mouse_descriptor.movement_x = LlcppAxisFromAttribute(movement_x);
      }
    } else if (field.attr.usage ==
               hid::USAGE(hid::usage::Page::kGenericDesktop, hid::usage::GenericDesktop::kY)) {
      if (field.flags & hid::FieldTypeFlags::kAbsolute) {
        position_y = field.attr;
        mouse_descriptor.position_y = LlcppAxisFromAttribute(position_y);
      } else {
        movement_y = field.attr;
        mouse_descriptor.movement_y = LlcppAxisFromAttribute(movement_y);
      }
    } else if (field.attr.usage ==
               hid::USAGE(hid::usage::Page::kGenericDesktop, hid::usage::GenericDesktop::kWheel)) {
      scroll_v = field.attr;
      mouse_descriptor.scroll_v = LlcppAxisFromAttribute(scroll_v);
    } else if (field.attr.usage.page == hid::usage::Page::kButton) {
      if (num_buttons == fuchsia_input_report::MOUSE_MAX_NUM_BUTTONS) {
        return ParseResult::kTooManyItems;
      }
      buttons[num_buttons++] = field.attr;
      mouse_descriptor.buttons[num_buttons] = static_cast<uint8_t>(field.attr.usage.usage);
    }
  }

  // No error, write to class members.
  movement_x_ = movement_x;
  movement_y_ = movement_y;
  position_x_ = position_x;
  position_y_ = position_y;
  scroll_v_ = scroll_v;
  for (size_t i = 0; i < num_buttons; i++) {
    buttons_[i] = buttons[i];
  }
  num_buttons_ = num_buttons;

  mouse_descriptor.num_buttons = num_buttons;
  descriptor_.input = mouse_descriptor;

  report_size_ = hid_report_descriptor.input_byte_sz;
  report_id_ = hid_report_descriptor.report_id;

  return ParseResult::kOk;
}

ReportDescriptor Mouse::GetDescriptor() {
  ReportDescriptor report_descriptor = {};
  report_descriptor.descriptor = descriptor_;
  return report_descriptor;
}

ParseResult Mouse::ParseInputReport(const uint8_t* data, size_t len, InputReport* report) {
  MouseInputReport mouse_report = {};
  if (len != report_size_) {
    return ParseResult::kReportSizeMismatch;
  }

  if (descriptor_.input->movement_x) {
    double value_out;
    if (hid::ExtractAsUnitType(data, len, movement_x_, &value_out)) {
      mouse_report.movement_x = static_cast<int64_t>(value_out);
    }
  }
  if (descriptor_.input->movement_y) {
    double value_out;
    if (hid::ExtractAsUnitType(data, len, movement_y_, &value_out)) {
      mouse_report.movement_y = static_cast<int64_t>(value_out);
    }
  }
  if (descriptor_.input->position_x) {
    double value_out;
    if (hid::ExtractAsUnitType(data, len, position_x_, &value_out)) {
      mouse_report.position_x = static_cast<int64_t>(value_out);
    }
  }
  if (descriptor_.input->position_y) {
    double value_out;
    if (hid::ExtractAsUnitType(data, len, position_y_, &value_out)) {
      mouse_report.position_y = static_cast<int64_t>(value_out);
    }
  }
  if (descriptor_.input->scroll_v) {
    double value_out;
    if (hid::ExtractAsUnitType(data, len, scroll_v_, &value_out)) {
      mouse_report.scroll_v = static_cast<int64_t>(value_out);
    }
  }
  for (size_t i = 0; i < num_buttons_; i++) {
    double value_out;
    if (hid::ExtractAsUnitType(data, len, buttons_[i], &value_out)) {
      uint8_t pressed = (value_out > 0) ? 1 : 0;
      if (pressed) {
        mouse_report.buttons_pressed[mouse_report.num_buttons_pressed] =
            static_cast<uint8_t>(buttons_[i].usage.usage);
        mouse_report.num_buttons_pressed++;
      }
    }
  }

  // Now that we can't fail, set the real report.
  report->report = mouse_report;

  return ParseResult::kOk;
}

}  // namespace hid_input_report
