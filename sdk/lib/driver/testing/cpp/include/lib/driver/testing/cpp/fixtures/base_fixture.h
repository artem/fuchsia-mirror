// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_FIXTURES_BASE_FIXTURE_H_
#define LIB_DRIVER_TESTING_CPP_FIXTURES_BASE_FIXTURE_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/fixtures/internal/fixture_internals.h>

namespace fdf_testing {

// The base class that driver test fixtures should inherit from. The Configuration class must be
// provided with certain values that dictate how the test should run.
//
// Here is an example Configuration class:
//
// class FixtureConfiguration final {
//  public:
//   static constexpr bool kDriverOnForeground = true;
//   static constexpr bool kAutoStartDriver = true;
//   static constexpr bool kAutoStopDriver = true;
//
//   using DriverType = MyDriverType;
//   using EnvironmentType = MyTestEnvironment;
// };
//
// Arguments:
//
//   DriverType: The type of the driver under test. This must be an inheritor of fdf::DriverBase.
//     If using a custom driver for the test, ensure the DriverType here contains a static function
//     in the format below. This registration is what the test uses to manage the driver.
//       `static DriverRegistration GetDriverRegistration()`
//
//   EnvironmentType: A class that contains custom dependencies for the driver under test.
//   The environment will always live on a background dispatcher.
//
//     It must contain a default constructor, and provide the following function:
//     zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs)
//
//     This is called automatically on the background environment dispatcher during initialization.
//     This function must serve all of its components into the provided fdf::OutgoingDirectory
//     object, generally done through the AddService method. This OutgoingDirectory backs the
//     driver's incoming namespace, hence its name `to_driver_vfs`.
//
//   kDriverOnForeground: Whether to have the driver under test run on the foreground dispatcher,
//   or to run it on a dedicated background dispatcher.
//
//     When this is `true`, the test can access the driver under test using the |driver()| method
//     and directly make calls into it, but sync client tasks must go through |RunInBackground()|.
//
//     When this is `false`, the test can run tasks on the driver context using the
//     |RunInDriverContext()| methods, but sync client tasks can be run directly.
//
//   kAutoStartDriver: If true, the test will automatically start the driver on construction,
//   and expect a successful start.
//
//   kAutoStopDriver: If true, the test will automatically stop the driver on destruction,
//   and expect a successful stop.
//
template <class Configuration>
class BaseDriverTestFixture : internal::ConfigurationExtractor<Configuration> {
  using Config = internal::ConfigurationExtractor<Configuration>;
  using Config::kAutoStartDriver;
  using Config::kAutoStopDriver;
  using Config::kDriverOnForeground;
  using typename Config::DriverType;
  using typename Config::EnvironmentType;

 public:
  BaseDriverTestFixture()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        env_wrapper_(env_dispatcher_->async_dispatcher(), std::in_place) {
    if constexpr (kAutoStartDriver) {
      auto result = StartDriverImpl({});
      ZX_ASSERT_MSG(result.is_ok(), "Failed to StartDriver: %s.", result.status_string());
    }
  }

  virtual ~BaseDriverTestFixture() {
    if constexpr (kAutoStopDriver) {
      auto result = StopDriverImpl();
      ZX_ASSERT_MSG(result.is_ok(), "Failed to StopDriver: %s.", result.status_string());
    }

    ShutdownDispatchersAndDestroyDriverImpl();
  }

  // Access the driver runtime object. This can be used to create new background dispatchers
  // or to run the foreground dispatcher.
  fdf_testing::DriverRuntime& runtime() { return runtime_; }

  // Connects to a service member that the driver under test provides.
  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  zx::result<fidl::internal::ClientEndType<typename ServiceMember::ProtocolType>> Connect(
      std::string_view instance = component::kDefaultInstance) {
    if constexpr (std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::ChannelTransport>) {
      return component::ConnectAtMember<ServiceMember>(ConnectToDriverSvcDir(), instance);
    } else if constexpr (std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                        fidl::internal::DriverTransport>) {
      return fdf::internal::DriverTransportConnect<ServiceMember>(ConnectToDriverSvcDir(),
                                                                  instance);
    } else {
      static_assert(std::false_type{});
    }
  }

  // Start the driver. The start arguments will be moved into the driver, therefore this must be
  // called only once.
  //
  // Enabled only when kAutoStartDriver is false.
  template <bool A = kAutoStartDriver, typename = std::enable_if_t<!A>>
  zx::result<> StartDriver() {
    static_assert(A == kAutoStartDriver, "Do not override the A template parameter.");
    return StartDriverImpl({});
  }

  // Start the driver with modified DriverStartArgs. This is done through the |args_modifier| which
  // is called with a reference to the start args that will be used to start the driver.
  // Modifications can happen in-place with this reference.
  //
  // Enabled only when kAutoStartDriver is false.
  template <bool A = kAutoStartDriver, typename = std::enable_if_t<!A>>
  zx::result<> StartDriverCustomized(fit::callback<void(fdf::DriverStartArgs&)> args_modifier) {
    static_assert(A == kAutoStartDriver, "Do not override the A template parameter.");
    return StartDriverImpl(std::move(args_modifier));
  }

  // Stops the driver by calling the driver's PrepareStop hook and waiting for it to complete
  //
  // Enabled only when kAutoStopDriver is false.
  template <bool A = kAutoStopDriver, typename = std::enable_if_t<!A>>
  zx::result<> StopDriver() {
    static_assert(A == kAutoStopDriver, "Do not override the A template parameter.");
    return StopDriverImpl();
  }

  // Shuts down all of the driver dispatchers, then destroys the driver. This must be called after
  // having called StopDriver(). This is an optional call, if the user doesn't call this, then it
  // will happen in the destructor. This calls the DriverBase's Stop hook
  //
  // Enabled only when kAutoStopDriver is false.
  template <bool A = kAutoStopDriver, typename = std::enable_if_t<!A>>
  void ShutdownDispatchersAndDestroyDriver() {
    static_assert(A == kAutoStopDriver, "Do not override the A template parameter.");
    ShutdownDispatchersAndDestroyDriverImpl();
  }

  // Runs a task on the dispatcher context of the driver under test. This will be a different
  // thread than the main test thread, so be careful when capturing and returning pointers to
  // objects that live on different dispatchers like test fixture properties, or the environment.
  //
  // Returns the result of the given task once it has completed.
  //
  // Enabled only when kDriverOnForeground is false.
  template <typename T, bool F = kDriverOnForeground, typename = std::enable_if_t<!F>>
  T RunInDriverContext(fit::callback<T(DriverType&)> task) {
    static_assert(F == kDriverOnForeground, "Do not override the F template parameter.");
    ZX_ASSERT_MSG(dut_.has_value(),
                  "Cannot call RunInDriverContext after ShutdownDispatchersAndDestroyDriver.");
    return dut_->RunInDriverContext(std::move(task));
  }

  // Runs a task on the dispatcher context of the driver under test. This will be a different
  // thread than the main test thread, so be careful when capturing and returning pointers to
  // objects that live on different dispatchers like test fixture properties, or the environment.
  //
  // Returns when the given task has completed.
  //
  // Enabled only when kDriverOnForeground is false.
  template <bool F = kDriverOnForeground, typename = std::enable_if_t<!F>>
  void RunInDriverContext(fit::callback<void(DriverType&)> task) {
    static_assert(F == kDriverOnForeground, "Do not override the F template parameter.");
    ZX_ASSERT_MSG(dut_.has_value(),
                  "Cannot call RunInDriverContext after ShutdownDispatchersAndDestroyDriver.");
    dut_->RunInDriverContext(std::move(task));
  }

  // Get a pointer to the driver under test.
  //
  // Enabled only when kDriverOnForeground is true.
  template <bool F = kDriverOnForeground, typename = std::enable_if_t<F>>
  DriverType* driver() {
    static_assert(F == kDriverOnForeground, "Do not override the F template parameter.");
    ZX_ASSERT_MSG(dut_.has_value(),
                  "Cannot call driver after ShutdownDispatchersAndDestroyDriver.");
    return dut_->driver();
  }

  // Runs a task in a background context while running the foreground driver. This must be used
  // if calling synchronously into the driver.
  //
  // Returns when the given task has completed.
  //
  // Enabled only when kDriverOnForeground is true.
  template <bool F = kDriverOnForeground, typename = std::enable_if_t<F>>
  zx::result<> RunInBackground(fit::callback<void()> task) {
    static_assert(F == kDriverOnForeground, "Do not override the F template parameter.");

    if (!bg_task_dispatcher_.has_value()) {
      bg_task_dispatcher_.emplace(runtime_.StartBackgroundDispatcher());
    }

    libsync::Completion completion;
    zx_status_t status = async::PostTask(bg_task_dispatcher_.value()->async_dispatcher(),
                                         [moved_task = std::move(task), &completion]() mutable {
                                           moved_task();
                                           completion.Signal();
                                         });

    if (status != ZX_OK) {
      return zx::error(status);
    }

    while (!completion.signaled()) {
      runtime_.RunUntilIdle();
    }

    return zx::ok();
  }

  // Runs a task in a background context while running the foreground driver. This must be used
  // if calling synchronously into the driver.
  //
  // Returns the result of the given task once it has completed.
  //
  // Enabled only when kDriverOnForeground is true.
  template <typename T, bool F = kDriverOnForeground, typename = std::enable_if_t<F>>
  zx::result<T> RunInBackground(fit::callback<T()> task) {
    static_assert(F == kDriverOnForeground, "Do not override the F template parameter.");

    if (!bg_task_dispatcher_.has_value()) {
      bg_task_dispatcher_.emplace(runtime_.StartBackgroundDispatcher());
    }

    libsync::Completion completion;
    std::optional<T> result_container;
    zx_status_t status =
        async::PostTask(bg_task_dispatcher_.value()->async_dispatcher(),
                        [moved_task = std::move(task), &completion, &result_container]() mutable {
                          result_container.emplace(moved_task());
                          completion.Signal();
                        });

    if (status != ZX_OK) {
      return zx::error(status);
    }

    while (!completion.signaled()) {
      runtime_.RunUntilIdle();
    }

    return zx::ok(std::move(result_container.value()));
  }

  // Runs a task on the dispatcher context of the EnvironmentType. This will be a different thread
  // than the main test thread, so be careful when capturing and returning pointers to objects that
  // live on different dispatchers like test fixture properties, or the driver.
  //
  // Returns the result of the given task once it has completed.
  template <typename T>
  T RunInEnvironmentTypeContext(fit::callback<T(EnvironmentType&)> task) {
    return env_wrapper_.SyncCall(
        [env_task = std::move(task)](internal::EnvWrapper<EnvironmentType>* env_ptr) mutable {
          return env_task(env_ptr->user_env());
        });
  }

  // Runs a task on the dispatcher context of the EnvironmentType. This will be a different thread
  // than the main test thread, so be careful when capturing and returning pointers to objects that
  // live on different dispatchers like test fixture properties, or the driver.
  //
  // Returns when the given task has completed.
  void RunInEnvironmentTypeContext(fit::callback<void(EnvironmentType&)> task) {
    env_wrapper_.SyncCall(
        [env_task = std::move(task)](internal::EnvWrapper<EnvironmentType>* env_ptr) mutable {
          env_task(env_ptr->user_env());
        });
  }

  // Runs a task on the dispatcher context of the TestNode. This will be a different thread than
  // the main test thread, so be careful when capturing and returning pointers to objects that live
  // on different dispatchers like test fixture properties, or the driver.
  //
  // Returns the result of the given task once it has completed.
  template <typename T>
  T RunInNodeContext(fit::callback<T(fdf_testing::TestNode&)> task) {
    return env_wrapper_.SyncCall(
        [node_task = std::move(task)](internal::EnvWrapper<EnvironmentType>* env_ptr) mutable {
          return node_task(env_ptr->node_server());
        });
  }

  // Runs a task on the dispatcher context of the TestNode. This will be a different thread than
  // the main test thread, so be careful when capturing and returning pointers to objects that live
  // on different dispatchers like test fixture properties, or the driver.
  //
  // Returns when the given task has completed.
  void RunInNodeContext(fit::callback<void(fdf_testing::TestNode&)> task) {
    env_wrapper_.SyncCall(
        [node_task = std::move(task)](internal::EnvWrapper<EnvironmentType>* env_ptr) mutable {
          node_task(env_ptr->node_server());
        });
  }

  // Connect to a zircon transport based protocol through a devfs node that the driver under test
  // exports. The |devfs_node_name| is the name of the created node with the 'devfs_args'. This
  // node must have been created through the driver's immediate node. If the devfs node is nested,
  // use the variant that takes a vector of strings.
  template <typename ProtocolType, typename = std::enable_if_t<fidl::IsProtocolV<ProtocolType>>>
  zx::result<fidl::ClientEnd<ProtocolType>> ConnectThroughDevfs(std::string_view devfs_node_name) {
    return ConnectThroughDevfs<ProtocolType>(std::vector{std::string(devfs_node_name)});
  }

  // Connect to a zircon transport based protocol through a devfs node that the driver under test
  // exports. The |devfs_node_name_path| is a list of node names that should be traversed to reach
  // the devfs node. The last element in the vector is the name of the created node with the
  // 'devfs_args'.
  template <typename ProtocolType, typename = std::enable_if_t<fidl::IsProtocolV<ProtocolType>>>
  zx::result<fidl::ClientEnd<ProtocolType>> ConnectThroughDevfs(
      std::vector<std::string> devfs_node_name_path) {
    zx::result<zx::channel> raw_channel_result =
        env_wrapper_.SyncCall([devfs_node_name_path = std::move(devfs_node_name_path)](
                                  internal::EnvWrapper<EnvironmentType>* env_ptr) mutable {
          fdf_testing::TestNode* current = &env_ptr->node_server();
          for (auto& node : devfs_node_name_path) {
            current = &current->children().at(node);
          }

          return current->ConnectToDevice();
        });
    if (raw_channel_result.is_error()) {
      return raw_channel_result.take_error();
    }

    return zx::ok(fidl::ClientEnd<ProtocolType>(std::move(raw_channel_result.value())));
  }

 private:
  bool StartedSuccessfully() const { return start_result_.has_value() && start_result_->is_ok(); }

  fidl::ClientEnd<fuchsia_io::Directory> ConnectToDriverSvcDir() {
    auto [client_end, server_end] = fidl::Endpoints<fuchsia_io::Directory>::Create();
    zx_status_t status = fdio_open_at(outgoing_directory_client_.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      server_end.TakeChannel().release());
    ZX_ASSERT_MSG(ZX_OK == status, "Failed to fdio_open_at '/svc' on the driver's outgoing: %s.",
                  zx_status_get_string(status));
    return std::move(client_end);
  }

  zx::result<> StartDriverImpl(fit::callback<void(fdf::DriverStartArgs&)> args_modifier) {
    ZX_ASSERT_MSG(
        !start_result_.has_value(),
        "Cannot call |StartDriver| multiple times in a row. If multiple starts are needed, "
        "ensure to go through |StopDriver| and |ShutdownDispatchersAndDestroyDriver| first.");

    dut_.emplace(&runtime_);
    fdf::DriverStartArgs start_args =
        env_wrapper_.SyncCall(&internal::EnvWrapper<EnvironmentType>::Init);
    outgoing_directory_client_ =
        env_wrapper_.SyncCall(&internal::EnvWrapper<EnvironmentType>::TakeOutgoingClient);
    if (args_modifier) {
      args_modifier(start_args);
    }

    start_result_ = dut_->Start(std::move(start_args));
    return start_result_.value();
  }

  zx::result<> StopDriverImpl() {
    ZX_ASSERT_MSG(start_result_.has_value(), "Cannot stop without having started.");
    ZX_ASSERT_MSG(!prepare_stop_result_.has_value(),
                  "Ensure |StopDriver| is only called once after a |StartDriver| call.");

    if (StartedSuccessfully()) {
      prepare_stop_result_.emplace(dut_->PrepareStop());
    } else {
      // Drivers that failed to stop don't receive a PrepareStop call. Set the result as ok so that
      // the teardown continues successfully.
      prepare_stop_result_.emplace(zx::ok());
    }

    return prepare_stop_result_.value();
  }

  void ShutdownDispatchersAndDestroyDriverImpl() {
    // Allow for calling this method multiple times, but only do this work the first time.
    if (dut_.has_value()) {
      if (StartedSuccessfully()) {
        ZX_ASSERT_MSG(prepare_stop_result_.has_value(),
                      "Ensure |ShutdownDispatchersAndDestroyDriver| is only called once after "
                      "a |StopDriver| call when kAutoStopDriver=false and start was successful.");
      }

      // Shuts down the driver dispatcher and all dispatchers created under it by the driver.
      if constexpr (kDriverOnForeground) {
        runtime_.ResetForegroundDispatcher([this]() { dut_.reset(); });
      } else {
        runtime_.ShutdownBackgroundDispatcher(dut_->GetDispatcher()->get(),
                                              [this]() { dut_.reset(); });
      }

      // Reset the start_result_ and prepare_stop_result_ to allow another iteration of the driver.
      start_result_.reset();
      prepare_stop_result_.reset();
    }
  }

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  async_patterns::TestDispatcherBound<internal::EnvWrapper<EnvironmentType>> env_wrapper_;

  // The dut_ is created through the |StartDriverImpl|, and becomes a nullopt through
  // |ShutdownDispatchersAndDestroyDriverImpl|.
  std::optional<internal::DriverWrapper<DriverType, kDriverOnForeground>> dut_;

  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client_;

  std::optional<fdf::UnownedSynchronizedDispatcher> bg_task_dispatcher_;
  std::optional<zx::result<>> start_result_;
  std::optional<zx::result<>> prepare_stop_result_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_FIXTURES_BASE_FIXTURE_H_
