// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_SRC_VIRTUAL_SOURCE_FILE_H_
#define TOOLS_FIDL_FIDLC_SRC_VIRTUAL_SOURCE_FILE_H_

#include <memory>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "tools/fidl/fidlc/src/source_file.h"
#include "tools/fidl/fidlc/src/source_span.h"

namespace fidlc {

// TODO(https://fxbug.dev/42160595): Remove this class.
class VirtualSourceFile : public SourceFile {
 public:
  explicit VirtualSourceFile(std::string filename) : SourceFile(std::move(filename), "") {}

  bool IsVirtual() const override { return true; }

  std::string_view LineContaining(std::string_view view, Position* position_out) const override;

  SourceSpan AddLine(std::string_view line);

 private:
  std::vector<std::unique_ptr<std::string>> virtual_lines_;
};

}  // namespace fidlc

#endif  // TOOLS_FIDL_FIDLC_SRC_VIRTUAL_SOURCE_FILE_H_
