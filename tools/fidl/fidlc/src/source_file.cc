// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/source_file.h"

#include <assert.h>
#include <zircon/assert.h>

#include <algorithm>
#include <functional>

namespace fidlc {

SourceFile::SourceFile(std::string filename, std::string data)
    : filename_(std::move(filename)), data_(std::move(data)) {
  size_t size = 0u;
  auto start_of_line = data_.cbegin();

  for (auto it = data_.cbegin(); it != data_.cend(); ++it) {
    if (*it == '\n' || *it == '\0') {
      auto& position = *start_of_line;
      lines_.emplace_back(&position, size);

      size = 0u;
      start_of_line = it + 1;
    } else {
      ++size;
    }
  }

  // Include the last line if the file does not end in a newline
  if (size > 0u) {
    ZX_ASSERT(start_of_line != data_.cend());
    auto& position = *start_of_line;
    lines_.emplace_back(&position, size);
  }
}

SourceFile::~SourceFile() = default;

std::string_view SourceFile::LineContaining(std::string_view view, Position* position_out) const {
  auto ptr_less_equal = std::less_equal<const char*>();

  const char* file_data = data().data();
  size_t file_size = data().size();

  if (view.data() == file_data + file_size) {
    // Gracefully handle a view indicating the end of the file
    //
    // Assert that this view is either zero-sized or references the null-terminating character
    // in the std::string.
    ZX_ASSERT_MSG(view.size() <= 1, "the view goes beyond the end of the SourceFile");

    if (file_size == 0) {
      *position_out = {1, 1};
      return std::string_view(view.data(), 0);
    }
    ZX_ASSERT_MSG(!lines_.empty(), "file size is greater than 0 but no lines were parsed");
    auto line = lines_.back();
    int line_number = static_cast<int>(lines_.size());
    int column_number = static_cast<int>(line.size() + 1);
    *position_out = {line_number, column_number};
    return line;
  }

  ZX_ASSERT_MSG(ptr_less_equal(file_data, view.data()), "the view is not part of this SourceFile");
  ZX_ASSERT_MSG(ptr_less_equal(view.data() + view.size(), file_data + file_size),
                "the view is not part of this SourceFile");

  // We are looking from the end of the file backwards (hence
  // crbegin and crend), looking for the first line (hence
  // upper_bound) to start at or before before the token in
  // question.
  auto is_in_line = [&ptr_less_equal](const std::string_view& left, const std::string_view& right) {
    return ptr_less_equal(right.data(), left.data());
  };
  auto line = std::upper_bound(lines_.crbegin(), lines_.crend(), view, is_in_line);
  ZX_ASSERT(line != lines_.crend());

  if (position_out != nullptr) {
    // Humans number lines from 1. Calculating this from the end
    // accounts for this.
    int line_number = static_cast<int>(lines_.crend() - line);
    // Columns should also start from 1.
    int column_number = static_cast<int>(view.data() - line->data()) + 1;
    *position_out = {line_number, column_number};
  }
  return std::string_view(*line);
}

}  // namespace fidlc
