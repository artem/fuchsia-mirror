// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TEMPLATE_STRING_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TEMPLATE_STRING_H_

#include <map>
#include <string>
#include <utility>

namespace fidlc {

using Substitutions = std::map<std::string, std::string>;

// Holds a string with named variables to replace, using a dictionary of
// key/value string pairs (map<std::string,std::string>). The variable format is
// $name or ${name}. Variables that without a matching key in the substitution
// map are left unchanged. "$$" converts to "$" without interpreting either "$"
// as part of a variable name.
//
// Example:
//   TemplateString ts("change '${ORIGINAL}' to '${REPLACEMENT}'");
//   std::string value = ts.Substitute({
//       {"ORIGINAL", prefix},
//       {"REPLACEMENT", replacement},
//   });
class TemplateString {
 public:
  explicit TemplateString(std::string str) : str_(std::move(str)) {}

  // Constructs an empty template.
  TemplateString() : TemplateString("") {}

  // Returns true if the template string is not empty.
  explicit operator bool() const { return !str_.empty(); }

  // Returns the string value after replacing all matched variables in the
  // template string with the values for the matching keys.
  // If |remove_unmatched| is true, variables without matching keys are
  // removed from the string.
  std::string Substitute(Substitutions substitutions, bool remove_unmatched) const;

  // Returns the string value after replacing all matched variables in the
  // template string with the values for the matching keys.
  // Variables without matching keys are left in place.
  std::string Substitute(Substitutions substitutions) const {
    return Substitute(std::move(substitutions), false);
  }

  // Returns the template string with unreplaced variables (as given at
  // construction).
  inline const std::string& str() const { return str_; }

 private:
  std::string str_;
};

}  // namespace fidlc

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TEMPLATE_STRING_H_
