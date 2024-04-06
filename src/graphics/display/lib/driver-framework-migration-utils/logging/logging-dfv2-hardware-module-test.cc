// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.logger/cpp/fidl.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/syslog/structured_backend/fuchsia_syslog.h>
#include <lib/zx/socket.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/driver-framework-migration-utils/logging/testing/logging-hardware-module.h"

namespace display {

namespace {

TEST(LoggingHardwareModule, MinimumLogLevelTrace) {
  testing::LoggingHardwareModule logging_hardware_module;
  fdf::Logger logger("test", FUCHSIA_LOG_TRACE, zx::socket{},
                     fidl::WireClient<fuchsia_logger::LogSink>{});
  fdf::Logger::SetGlobalInstance(&logger);

  EXPECT_TRUE(logging_hardware_module.LogTrace());
  EXPECT_TRUE(logging_hardware_module.LogDebug());
  EXPECT_TRUE(logging_hardware_module.LogInfo());
  EXPECT_TRUE(logging_hardware_module.LogWarning());
  EXPECT_TRUE(logging_hardware_module.LogError());

  fdf::Logger::SetGlobalInstance(nullptr);
}

TEST(LoggingHardwareModule, MinimumLogLevelDebug) {
  testing::LoggingHardwareModule logging_hardware_module;
  fdf::Logger logger("test", FUCHSIA_LOG_DEBUG, zx::socket{},
                     fidl::WireClient<fuchsia_logger::LogSink>{});
  fdf::Logger::SetGlobalInstance(&logger);

  EXPECT_FALSE(logging_hardware_module.LogTrace());
  EXPECT_TRUE(logging_hardware_module.LogDebug());
  EXPECT_TRUE(logging_hardware_module.LogInfo());
  EXPECT_TRUE(logging_hardware_module.LogWarning());
  EXPECT_TRUE(logging_hardware_module.LogError());

  fdf::Logger::SetGlobalInstance(nullptr);
}

TEST(LoggingHardwareModule, MinimumLogLevelInfo) {
  testing::LoggingHardwareModule logging_hardware_module;
  fdf::Logger logger("test", FUCHSIA_LOG_INFO, zx::socket{},
                     fidl::WireClient<fuchsia_logger::LogSink>{});
  fdf::Logger::SetGlobalInstance(&logger);

  EXPECT_FALSE(logging_hardware_module.LogTrace());
  EXPECT_FALSE(logging_hardware_module.LogDebug());
  EXPECT_TRUE(logging_hardware_module.LogInfo());
  EXPECT_TRUE(logging_hardware_module.LogWarning());
  EXPECT_TRUE(logging_hardware_module.LogError());

  fdf::Logger::SetGlobalInstance(nullptr);
}

TEST(LoggingHardwareModule, MinimumLogLevelWarning) {
  testing::LoggingHardwareModule logging_hardware_module;
  fdf::Logger logger("test", FUCHSIA_LOG_WARNING, zx::socket{},
                     fidl::WireClient<fuchsia_logger::LogSink>{});
  fdf::Logger::SetGlobalInstance(&logger);

  EXPECT_FALSE(logging_hardware_module.LogTrace());
  EXPECT_FALSE(logging_hardware_module.LogDebug());
  EXPECT_FALSE(logging_hardware_module.LogInfo());
  EXPECT_TRUE(logging_hardware_module.LogWarning());
  EXPECT_TRUE(logging_hardware_module.LogError());

  fdf::Logger::SetGlobalInstance(nullptr);
}

TEST(LoggingHardwareModule, MinimumLogLevelError) {
  testing::LoggingHardwareModule logging_hardware_module;
  fdf::Logger logger("test", FUCHSIA_LOG_ERROR, zx::socket{},
                     fidl::WireClient<fuchsia_logger::LogSink>{});
  fdf::Logger::SetGlobalInstance(&logger);

  EXPECT_FALSE(logging_hardware_module.LogTrace());
  EXPECT_FALSE(logging_hardware_module.LogDebug());
  EXPECT_FALSE(logging_hardware_module.LogInfo());
  EXPECT_FALSE(logging_hardware_module.LogWarning());
  EXPECT_TRUE(logging_hardware_module.LogError());

  fdf::Logger::SetGlobalInstance(nullptr);
}

}  // namespace

}  // namespace display
