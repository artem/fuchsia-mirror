// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_MANAGER_CONFIG_H_
#define SRC_PERFORMANCE_TRACE_MANAGER_CONFIG_H_

#include <map>
#include <string>

namespace tracing {

class Config {
 public:
  Config();
  ~Config();

  Config(Config&& other) = default;
  Config& operator=(Config&& other) = default;
  Config(const Config&) = delete;
  Config& operator=(const Config&) = delete;

  // Tries to parse configuration from |command_line|.
  // Returns false if an error occurs.
  bool ReadFrom(const std::string& config_file);

  // All categories known to the |TraceManager|, with every
  // category being described by a short string.
  const std::map<std::string, std::string>& known_categories() const { return known_categories_; }

 private:
  std::map<std::string, std::string> known_categories_;
};

}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_MANAGER_CONFIG_H_
