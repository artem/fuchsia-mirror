// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.sensors/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/clock.h>

#include <gtest/gtest.h>

using fuchsia_hardware_sensors::Driver;
using fuchsia_hardware_sensors::Playback;
using fuchsia_math::Vec3F;

using fuchsia_hardware_sensors::ConfigureSensorRateError;
using fuchsia_hardware_sensors::FixedValuesPlaybackConfig;
using fuchsia_hardware_sensors::PlaybackSourceConfig;
using fuchsia_sensors_types::EventPayload;
using fuchsia_sensors_types::SensorEvent;
using fuchsia_sensors_types::SensorId;
using fuchsia_sensors_types::SensorInfo;
using fuchsia_sensors_types::SensorRateConfig;
using fuchsia_sensors_types::SensorReportingMode;
using fuchsia_sensors_types::SensorType;
using fuchsia_sensors_types::SensorWakeUpType;

constexpr SensorId kAccelSensorId = 1;
constexpr int kAccelEventLimit = 100;

constexpr SensorId kGyroSensorId = 2;
constexpr int kGyroEventLimit = 10;

class PlaybackEventHandler : public fidl::AsyncEventHandler<Playback> {
 public:
  PlaybackEventHandler(async::Loop& loop) : loop_(loop) {}

  void handle_unknown_event(fidl::UnknownEventMetadata<Playback> metadata) override {
    FX_LOGS(ERROR) << "Saw unknown event.";
    saw_error_ = true;
    loop_.Quit();
  }

  void on_fidl_error(fidl::UnbindInfo error) override {
    if (expecting_error_) {
      FX_LOGS(INFO) << "Saw aync error: " << error;
    } else {
      FX_LOGS(ERROR) << "Saw async error: " << error;
    }
    saw_error_ = true;
    error_ = error;
    loop_.Quit();
  }

  bool saw_error() { return saw_error_; }
  fidl::UnbindInfo error() { return error_; }
  void reset_error() {
    saw_error_ = false;
    error_ = fidl::UnbindInfo();
  }
  void expecting_error(bool expecting) { expecting_error_ = expecting; }

 private:
  bool expecting_error_ = false;
  bool saw_error_ = false;
  fidl::UnbindInfo error_;
  async::Loop& loop_;
};

class DriverEventHandler : public fidl::AsyncEventHandler<Driver> {
 public:
  DriverEventHandler(async::Loop& loop) : loop_(loop) {}

  void OnSensorEvent(fidl::Event<Driver::OnSensorEvent>& e) override {
    const SensorEvent& event = e.event();
    if (sensor_events_[event.sensor_id()].empty()) {
      FX_LOGS(INFO) << "Got first event from sensor " << event.sensor_id();
    }
    sensor_events_[event.sensor_id()].push_back(event);

    if (sensor_events_[kAccelSensorId].size() >= kAccelEventLimit &&
        sensor_events_[kGyroSensorId].size() >= kGyroEventLimit && !saw_final_event_) {
      FX_LOGS(INFO) << "Got final event from sensor " << event.sensor_id();
      loop_.Quit();
      saw_final_event_ = true;
    }
  }

  void handle_unknown_event(fidl::UnknownEventMetadata<Driver> metadata) override {
    FX_LOGS(ERROR) << "Saw unknown event.";
    saw_error_ = true;
    loop_.Quit();
  }

  void on_fidl_error(fidl::UnbindInfo error) override {
    if (expecting_error_) {
      FX_LOGS(INFO) << "Saw aync error: " << error;
    } else {
      FX_LOGS(ERROR) << "Saw async error: " << error;
    }
    saw_error_ = true;
    error_ = error;
    loop_.Quit();
  }

  const std::unordered_map<SensorId, std::vector<SensorEvent>>& sensor_events() {
    return sensor_events_;
  }

  bool saw_error() { return saw_error_; }
  fidl::UnbindInfo error() { return error_; }
  void reset_error() {
    saw_error_ = false;
    error_ = fidl::UnbindInfo();
  }
  void expecting_error(bool expecting) { expecting_error_ = expecting; }

 private:
  bool expecting_error_ = false;
  bool saw_error_ = false;
  fidl::UnbindInfo error_;

  bool saw_final_event_ = false;
  std::unordered_map<SensorId, std::vector<SensorEvent>> sensor_events_;
  async::Loop& loop_;
};

SensorInfo CreateAccelerometer(SensorId accel_sensor_id) {
  SensorInfo accelerometer_info;
  accelerometer_info.sensor_id(accel_sensor_id);
  accelerometer_info.name("accelerometer");
  accelerometer_info.vendor("Accelomax");
  accelerometer_info.version(1);
  accelerometer_info.sensor_type(SensorType::kAccelerometer);
  accelerometer_info.wake_up(SensorWakeUpType::kWakeUp);
  accelerometer_info.reporting_mode(SensorReportingMode::kContinuous);
  return accelerometer_info;
}

SensorInfo CreateGyroscope(SensorId gyro_sensor_id) {
  SensorInfo gyroscope_info;
  gyroscope_info.sensor_id(gyro_sensor_id);
  gyroscope_info.name("gyroscope");
  gyroscope_info.vendor("Gyrocorp");
  gyroscope_info.version(1);
  gyroscope_info.sensor_type(SensorType::kGyroscope);
  gyroscope_info.wake_up(SensorWakeUpType::kWakeUp);
  gyroscope_info.reporting_mode(SensorReportingMode::kContinuous);
  return gyroscope_info;
}

void CreateAccelerometerEvents(SensorId accel_sensor_id, std::vector<SensorEvent>& event_list) {
  for (int i = 0; i < 3; i++) {
    Vec3F sample{{
        .x = i % 3 == 0 ? 1.0f : 0.0f,
        .y = i % 3 == 1 ? 1.0f : 0.0f,
        .z = i % 3 == 2 ? 1.0f : 0.0f,
    }};

    SensorEvent event{{.payload = EventPayload::WithVec3(sample)}};
    event.sensor_id() = accel_sensor_id;
    event.sensor_type() = SensorType::kAccelerometer;

    event_list.push_back(event);
  }
}

void CreateGyroscopeEvents(SensorId gyro_sensor_id, std::vector<SensorEvent>& event_list) {
  for (int i = 0; i < 3; i++) {
    Vec3F sample{{
        .x = i % 3 == 1 ? 1.0f : 0.0f,
        .y = i % 3 == 2 ? 1.0f : 0.0f,
        .z = i % 3 == 0 ? 1.0f : 0.0f,
    }};

    SensorEvent event{{.payload = EventPayload::WithVec3(sample)}};
    event.sensor_id() = gyro_sensor_id;
    event.sensor_type() = SensorType::kGyroscope;

    event_list.push_back(event);
  }
}

PlaybackSourceConfig CreateFixedValuesPlaybackConfig(std::vector<SensorInfo>& sensor_list) {
  std::vector<SensorEvent> fixed_event_list;

  sensor_list.push_back(CreateAccelerometer(kAccelSensorId));
  CreateAccelerometerEvents(kAccelSensorId, fixed_event_list);

  sensor_list.push_back(CreateGyroscope(kGyroSensorId));
  CreateGyroscopeEvents(kGyroSensorId, fixed_event_list);

  FixedValuesPlaybackConfig fixed_config;
  fixed_config.sensor_list(sensor_list);
  fixed_config.sensor_events(fixed_event_list);

  return PlaybackSourceConfig::WithFixedValuesConfig(fixed_config);
}

// Tests that the playback component only allows one Playback protocol client at a time.
TEST(SensorsPlaybackTest, MultiplePlaybackClientsRejected) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  PlaybackEventHandler handler(loop);
  PlaybackEventHandler handler2(loop);
  handler2.expecting_error(true);

  // Create first connection.
  zx::result playback_client_end = component::Connect<Playback>();
  ASSERT_TRUE(playback_client_end.is_ok());
  fidl::Client playback_client(std::move(*playback_client_end), dispatcher, &handler);

  // Run an operation on the connection to make sure this one is actually connected first.
  std::vector<SensorInfo> sensor_list;
  std::optional<bool> result_ok;
  playback_client->ConfigurePlayback(CreateFixedValuesPlaybackConfig(sensor_list))
      .ThenExactlyOnce([&](fidl::Result<Playback::ConfigurePlayback>& result) {
        result_ok = result.is_ok();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_FALSE(handler.saw_error());
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);

  // Create the second connection.
  zx::result playback_client_end2 = component::Connect<Playback>();
  ASSERT_TRUE(playback_client_end2.is_ok());
  fidl::Client playback_client2(std::move(*playback_client_end2), dispatcher, &handler2);

  // Let the looper run until we get the asynchronous Playback disconnect error.
  loop.Run();
  loop.ResetQuit();
  EXPECT_FALSE(handler.saw_error());
  ASSERT_TRUE(handler2.saw_error());
}

// Tests that the driver component only allows one Driver protocol client at a time.
TEST(SensorsDriverTest, MultipleDriverClientsRejected) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  PlaybackEventHandler playback_handler(loop);
  DriverEventHandler handler(loop);
  DriverEventHandler handler2(loop);
  handler2.expecting_error(true);

  // Connect to the Playback protocol and set a valid config just to keep the flow nominal up till
  // the second Driver connection.
  zx::result playback_client_end = component::Connect<Playback>();
  ASSERT_TRUE(playback_client_end.is_ok());
  fidl::Client playback_client(std::move(*playback_client_end), dispatcher, &playback_handler);

  std::vector<SensorInfo> sensor_list;
  std::optional<bool> result_ok;
  playback_client->ConfigurePlayback(CreateFixedValuesPlaybackConfig(sensor_list))
      .ThenExactlyOnce([&](fidl::Result<Playback::ConfigurePlayback>& result) {
        result_ok = result.is_ok();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_FALSE(handler.saw_error());
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);

  // Create the first connection.
  zx::result driver_client_end = component::Connect<Driver>();
  ASSERT_TRUE(driver_client_end.is_ok());
  fidl::Client driver_client(std::move(*driver_client_end), dispatcher, &handler);

  // Run an operation on the connection to make sure this one is actually connected first.
  result_ok = std::nullopt;
  handler.reset_error();
  driver_client->GetSensorsList().ThenExactlyOnce(
      [&](fidl::Result<Driver::GetSensorsList>& result) {
        result_ok = result.is_ok();
        if (!result.is_ok()) {
          FX_LOGS(INFO) << "Saw error: " << result.error_value();
        }
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);
  ASSERT_FALSE(handler.saw_error());

  // Create the second connection.
  zx::result driver_client_end2 = component::Connect<Driver>();
  ASSERT_TRUE(driver_client_end2.is_ok());
  fidl::Client driver_client2(std::move(*driver_client_end2), dispatcher, &handler2);

  // Let the looper run until we get the asynchronous Driver disconnect error.
  loop.Run();
  loop.ResetQuit();
  EXPECT_FALSE(handler.saw_error());
  ASSERT_TRUE(handler2.saw_error());
}

// Tests that the playback component disconnects the Driver client if the Playback client
// disconnects.
TEST(SensorsPlaybackTest, DriverDisconnectsIfPlaybackDisconnects) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  DriverEventHandler handler(loop);

  // Connect to the Playback protocol and set a valid playback configuration.
  zx::result playback_client_end = component::Connect<Playback>();
  ASSERT_TRUE(playback_client_end.is_ok());
  std::optional<fidl::Client<Playback>> playback_client =
      fidl::Client<Playback>(std::move(*playback_client_end), dispatcher);

  std::optional<bool> result_ok;
  std::vector<SensorInfo> sensor_list;
  (*playback_client)
      ->ConfigurePlayback(CreateFixedValuesPlaybackConfig(sensor_list))
      .ThenExactlyOnce([&](fidl::Result<Playback::ConfigurePlayback>& result) {
        result_ok = result.is_ok();
        loop.Quit();
      });
  loop.Run();
  loop.ResetQuit();
  ASSERT_FALSE(handler.saw_error());
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);

  zx::result driver_client_end = component::Connect<Driver>();
  ASSERT_TRUE(driver_client_end.is_ok());
  fidl::Client driver_client(std::move(*driver_client_end), dispatcher, &handler);

  result_ok = std::nullopt;
  handler.reset_error();
  driver_client->GetSensorsList().ThenExactlyOnce(
      [&](fidl::Result<Driver::GetSensorsList>& result) {
        result_ok = result.is_ok();
        if (!result.is_ok()) {
          FX_LOGS(INFO) << "Saw error: " << result.error_value();
        }
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);
  ASSERT_FALSE(handler.saw_error());

  // Let the Playback connection go out of scope. This will cause the playback configuration in
  // the playback component to be cleared and the Driver connection to be closed remotely.
  // Set the handler to expect an asynchronous error (the remote Driver connection closure).
  handler.expecting_error(true);
  playback_client = std::nullopt;

  // Let the looper run until we get the asynchronous Driver disconnect error.
  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(handler.saw_error());
}

// Tests the following features of the playback component:
// - Play back a bunch of fixed valued sensor events provided by the client.
// - Generate "ideal" timestamps based on the configured sampling period.
// This does not test sensor data buffering (max reporting latency is set to 0).
TEST(SensorsPlaybackTest, FixedValues_UnbufferedGeneratedTimestamps) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  DriverEventHandler handler(loop);

  zx::result playback_client_end = component::Connect<Playback>();
  ASSERT_TRUE(playback_client_end.is_ok());
  zx::result driver_client_end = component::Connect<Driver>();
  ASSERT_TRUE(driver_client_end.is_ok());

  fidl::Client playback_client(std::move(*playback_client_end), dispatcher);
  fidl::Client driver_client(std::move(*driver_client_end), dispatcher, &handler);

  // Set fixed playback mode with the generated sensor list and set of events.
  std::vector<SensorInfo> sensor_list;
  std::optional<bool> result_ok;
  playback_client->ConfigurePlayback(CreateFixedValuesPlaybackConfig(sensor_list))
      .ThenExactlyOnce([&](fidl::Result<Playback::ConfigurePlayback>& result) {
        result_ok = result.is_ok();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);
  ASSERT_FALSE(handler.saw_error());

  // Check that we get back the sensor list we just configured.
  result_ok = std::nullopt;
  driver_client->GetSensorsList().ThenExactlyOnce(
      [&](fidl::Result<Driver::GetSensorsList>& result) {
        result_ok = sensor_list == result->sensor_list();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);
  ASSERT_FALSE(handler.saw_error());

  // Accelerometer rate.
  SensorRateConfig accel_rate_config;
  accel_rate_config.sampling_period_ns(1e7);      // One hundredth of a second.
  accel_rate_config.max_reporting_latency_ns(0);  // No buffering.

  result_ok = std::nullopt;
  driver_client->ConfigureSensorRate({kAccelSensorId, accel_rate_config})
      .ThenExactlyOnce([&](fidl::Result<Driver::ConfigureSensorRate>& result) {
        result_ok = result.is_ok();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);
  ASSERT_FALSE(handler.saw_error());

  // Gyroscope rate.
  SensorRateConfig gyro_rate_config;
  gyro_rate_config.sampling_period_ns(1e8);      // One tenth of a second.
  gyro_rate_config.max_reporting_latency_ns(0);  // No buffering.

  result_ok = std::nullopt;
  driver_client->ConfigureSensorRate({kGyroSensorId, gyro_rate_config})
      .ThenExactlyOnce([&](fidl::Result<Driver::ConfigureSensorRate>& result) {
        result_ok = result.is_ok();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(result_ok.has_value());
  ASSERT_TRUE(*result_ok);
  ASSERT_FALSE(handler.saw_error());

  // Activate the accelerometer.
  std::optional<bool> accel_result_ok;
  driver_client->ActivateSensor({kAccelSensorId})
      .ThenExactlyOnce(
          [&](fidl::Result<Driver::ActivateSensor>& result) { accel_result_ok = result.is_ok(); });

  // Activate the gyroscope.
  std::optional<bool> gyro_result_ok;
  driver_client->ActivateSensor({kGyroSensorId})
      .ThenExactlyOnce(
          [&](fidl::Result<Driver::ActivateSensor>& result) { gyro_result_ok = result.is_ok(); });

  // Set deadline of 10 seconds and run.
  FX_LOGS(INFO) << "Waiting for sensor events.";
  zx_status_t run_result = loop.Run(zx::deadline_after(zx::duration(static_cast<long>(10 * 1e9))));
  loop.ResetQuit();

  if (run_result == ZX_ERR_TIMED_OUT) {
    FX_LOGS(ERROR) << "Timed out waiting for sensor events.";
  }

  ASSERT_TRUE(accel_result_ok.has_value());
  ASSERT_TRUE(*accel_result_ok);
  ASSERT_TRUE(gyro_result_ok.has_value());
  ASSERT_TRUE(*gyro_result_ok);
  ASSERT_EQ(run_result, ZX_ERR_CANCELED);
  ASSERT_FALSE(handler.saw_error());
  ASSERT_GE(handler.sensor_events().at(kAccelSensorId).size(), 100ul);
  ASSERT_GE(handler.sensor_events().at(kGyroSensorId).size(), 10ul);

  // Deactivate the accelerometer.
  accel_result_ok = std::nullopt;
  driver_client->DeactivateSensor({kAccelSensorId})
      .ThenExactlyOnce([&](fidl::Result<Driver::DeactivateSensor>& result) {
        accel_result_ok = result.is_ok();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(accel_result_ok.has_value());
  ASSERT_TRUE(*accel_result_ok);
  ASSERT_FALSE(handler.saw_error());

  // Deactivate the gyroscope.
  gyro_result_ok = std::nullopt;
  driver_client->DeactivateSensor({kGyroSensorId})
      .ThenExactlyOnce([&](fidl::Result<Driver::DeactivateSensor>& result) {
        gyro_result_ok = result.is_ok();
        loop.Quit();
      });

  loop.Run();
  loop.ResetQuit();
  ASSERT_TRUE(gyro_result_ok.has_value());
  ASSERT_TRUE(*gyro_result_ok);
  ASSERT_FALSE(handler.saw_error());
}
