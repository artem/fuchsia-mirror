// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "multiple_device_test.h"

#include <fuchsia/io/llcpp/fidl.h>
#include <zircon/errors.h>

#include <string>

#include "component_lifecycle.h"
#include "coordinator_test_mock_power_manager.h"
#include "src/devices/lib/log/log.h"

TEST_F(MultipleDeviceTestCase, UnbindThenSuspend) {
  size_t parent_index;
  ASSERT_NO_FATAL_FAILURES(
      AddDevice(platform_bus(), "parent-device", 0 /* protocol id */, "", &parent_index));

  size_t child_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(device(parent_index)->device, "child-device",
                                     0 /* protocol id */, "", &child_index));

  ASSERT_NO_FATAL_FAILURES(coordinator().ScheduleRemove(device(parent_index)->device));
  coordinator_loop()->RunUntilIdle();

  zx_txid_t txid;
  // The child should be unbound first.
  ASSERT_NO_FATAL_FAILURES(CheckUnbindReceived(device(child_index)->controller_remote, &txid));
  coordinator_loop()->RunUntilIdle();

  const uint32_t flags = DEVICE_SUSPEND_FLAG_POWEROFF;
  ASSERT_NO_FATAL_FAILURES(DoSuspend(flags));

  ASSERT_NO_FATAL_FAILURES(SendUnbindReply(device(child_index)->controller_remote, txid));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(child_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(parent_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();

  // The suspend task should complete but not send a suspend message.
  ASSERT_FALSE(DeviceHasPendingMessages(device(parent_index)->controller_remote));
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(platform_bus_controller_remote(), flags, ZX_OK));
  coordinator_loop()->RunUntilIdle();
}

TEST_F(MultipleDeviceTestCase, SuspendThenUnbind) {
  size_t parent_index;
  ASSERT_NO_FATAL_FAILURES(
      AddDevice(platform_bus(), "parent-device", 0 /* protocol id */, "", &parent_index));

  size_t child_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(device(parent_index)->device, "child-device",
                                     0 /* protocol id */, "", &child_index));

  const uint32_t flags = DEVICE_SUSPEND_FLAG_POWEROFF;
  ASSERT_NO_FATAL_FAILURES(DoSuspend(flags));

  zx_txid_t txid;

  // Don't reply to the suspend yet.
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceived(device(child_index)->controller_remote, flags, &txid));
  ASSERT_NO_FATAL_FAILURES(coordinator().ScheduleRemove(device(parent_index)->device));
  coordinator_loop()->RunUntilIdle();

  // Check that the child device has not yet started unbinding.
  ASSERT_FALSE(DeviceHasPendingMessages(device(child_index)->controller_remote));

  ASSERT_NO_FATAL_FAILURES(SendSuspendReply(device(child_index)->controller_remote, ZX_OK, txid));
  coordinator_loop()->RunUntilIdle();

  // The parent should not have received a suspend. It is in process of removal.
  ASSERT_FALSE(DeviceHasPendingMessages(device(parent_index)->controller_remote));

  // Finish unbinding the child.
  ASSERT_NO_FATAL_FAILURES(CheckUnbindReceivedAndReply(device(child_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(child_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(platform_bus_controller_remote(), flags, ZX_OK));
  coordinator_loop()->RunUntilIdle();

  // The parent should now be removed.
  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(parent_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();
}

TEST_F(MultipleDeviceTestCase, ConcurrentSuspend) {
  size_t parent_index;
  ASSERT_NO_FATAL_FAILURES(
      AddDevice(platform_bus(), "parent-device", 0 /* protocol id */, "", &parent_index));

  size_t child_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(device(parent_index)->device, "child-device",
                                     0 /* protocol id */, "", &child_index));

  const uint32_t flags = DEVICE_SUSPEND_FLAG_POWEROFF;
  zx_status_t first_suspend_status = ZX_ERR_INTERNAL;
  ASSERT_NO_FATAL_FAILURES(
      DoSuspendWithCallback(flags, [&first_suspend_status](zx_status_t completion_status) {
        first_suspend_status = completion_status;
      }));

  zx_txid_t txid;

  // Don't reply to the suspend yet.
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceived(device(child_index)->controller_remote, flags, &txid));

  zx_status_t second_suspend_status = ZX_OK;
  ASSERT_NO_FATAL_FAILURES(
      DoSuspendWithCallback(flags, [&second_suspend_status](zx_status_t completion_status) {
        second_suspend_status = completion_status;
      }));
  ASSERT_EQ(second_suspend_status, ZX_ERR_UNAVAILABLE);
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(SendSuspendReply(device(child_index)->controller_remote, ZX_OK, txid));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(device(parent_index)->controller_remote, flags, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(platform_bus_controller_remote(), flags, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(sys_proxy_controller_remote_, flags, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_EQ(first_suspend_status, ZX_OK);
}

TEST_F(MultipleDeviceTestCase, UnbindThenResume) {
  size_t parent_index;
  ASSERT_NO_FATAL_FAILURES(
      AddDevice(platform_bus(), "parent-device", 0 /* protocol id */, "", &parent_index));

  size_t child_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(device(parent_index)->device, "child-device",
                                     0 /* protocol id */, "", &child_index));

  coordinator().sys_device()->set_state(Device::State::kSuspended);
  coordinator().sys_device()->proxy()->set_state(Device::State::kSuspended);
  platform_bus()->set_state(Device::State::kSuspended);
  device(parent_index)->device->set_state(Device::State::kSuspended);
  device(child_index)->device->set_state(Device::State::kSuspended);

  ASSERT_NO_FATAL_FAILURES(coordinator().ScheduleRemove(device(parent_index)->device));
  coordinator_loop()->RunUntilIdle();
  // The child should be unbound first.
  zx_txid_t txid;
  ASSERT_NO_FATAL_FAILURES(CheckUnbindReceived(device(child_index)->controller_remote, &txid));

  ASSERT_NO_FATAL_FAILURES(DoResume(SystemPowerState::FULLY_ON));

  ASSERT_NO_FATAL_FAILURES(
      CheckResumeReceived(sys_proxy_controller_remote_, SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(
      CheckResumeReceived(platform_bus_controller_remote(), SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(CheckResumeReceived(device(parent_index)->controller_remote,
                                               SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(SendUnbindReply(device(child_index)->controller_remote, txid));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(child_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(parent_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();

  // The resume task should complete but not send a resume message.
  ASSERT_FALSE(DeviceHasPendingMessages(device(parent_index)->controller_remote));
  ASSERT_FALSE(DeviceHasPendingMessages(device(child_index)->controller_remote));
}

TEST_F(MultipleDeviceTestCase, ResumeThenUnbind) {
  size_t parent_index;
  ASSERT_NO_FATAL_FAILURES(
      AddDevice(platform_bus(), "parent-device", 0 /* protocol id */, "", &parent_index));

  size_t child_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(device(parent_index)->device, "child-device",
                                     0 /* protocol id */, "", &child_index));

  coordinator().sys_device()->set_state(Device::State::kSuspended);
  coordinator().sys_device()->proxy()->set_state(Device::State::kSuspended);
  platform_bus()->set_state(Device::State::kSuspended);
  device(parent_index)->device->set_state(Device::State::kSuspended);
  device(child_index)->device->set_state(Device::State::kSuspended);

  ASSERT_NO_FATAL_FAILURES(DoResume(SystemPowerState::FULLY_ON));

  zx_txid_t txid;
  ASSERT_NO_FATAL_FAILURES(
      CheckResumeReceived(sys_proxy_controller_remote_, SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(
      CheckResumeReceived(platform_bus_controller_remote(), SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  // Don't reply to the resume yet.
  ASSERT_NO_FATAL_FAILURES(CheckResumeReceived(device(parent_index)->controller_remote,
                                               SystemPowerState::FULLY_ON, &txid));
  ASSERT_NO_FATAL_FAILURES(coordinator().ScheduleRemove(device(parent_index)->device));
  coordinator_loop()->RunUntilIdle();

  // Check that the child device has not yet started unbinding.
  ASSERT_FALSE(DeviceHasPendingMessages(device(child_index)->controller_remote));

  ASSERT_NO_FATAL_FAILURES(SendResumeReply(device(parent_index)->controller_remote, ZX_OK, txid));
  coordinator_loop()->RunUntilIdle();

  // The Child should have started resuming now. Complete resume of child.
  ASSERT_NO_FATAL_FAILURES(CheckResumeReceived(device(child_index)->controller_remote,
                                               SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  // Since the resume is complete, unbinding the child should start now.
  ASSERT_NO_FATAL_FAILURES(CheckUnbindReceivedAndReply(device(child_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(child_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();

  // The parent should now be removed.
  ASSERT_NO_FATAL_FAILURES(CheckRemoveReceivedAndReply(device(parent_index)->controller_remote));
  coordinator_loop()->RunUntilIdle();
}

TEST_F(MultipleDeviceTestCase, SuspendThenResume) {
  size_t parent_index;
  ASSERT_NO_FATAL_FAILURES(
      AddDevice(platform_bus(), "parent-device", 0 /* protocol id */, "", &parent_index));

  size_t child_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(device(parent_index)->device, "child-device",
                                     0 /* protocol id */, "", &child_index));

  const uint32_t flags = DEVICE_SUSPEND_FLAG_POWEROFF;
  ASSERT_NO_FATAL_FAILURES(DoSuspend(flags));

  zx_txid_t txid;
  // Don't reply to the suspend yet.
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceived(device(child_index)->controller_remote, flags, &txid));

  // This should return without scheduling resume tasks since suspend is in
  // progress.
  ASSERT_NO_FATAL_FAILURES(DoResume(SystemPowerState::FULLY_ON));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(SendSuspendReply(device(child_index)->controller_remote, ZX_OK, txid));
  coordinator_loop()->RunUntilIdle();

  // The parent should have started suspending.
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(device(parent_index)->controller_remote, flags, ZX_OK));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(platform_bus_controller_remote(), flags, ZX_OK));
  ASSERT_FALSE(DeviceHasPendingMessages(device(parent_index)->controller_remote));
  ASSERT_FALSE(DeviceHasPendingMessages(device(child_index)->controller_remote));
  ASSERT_EQ(device(parent_index)->device->state(), Device::State::kSuspended);
  ASSERT_EQ(device(child_index)->device->state(), Device::State::kSuspended);
}

TEST_F(MultipleDeviceTestCase, ResumeThenSuspend) {
  size_t parent_index;
  ASSERT_NO_FATAL_FAILURES(
      AddDevice(platform_bus(), "parent-device", 0 /* protocol id */, "", &parent_index));

  size_t child_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(device(parent_index)->device, "child-device",
                                     0 /* protocol id */, "", &child_index));

  coordinator().sys_device()->set_state(Device::State::kSuspended);
  coordinator().sys_device()->proxy()->set_state(Device::State::kSuspended);
  platform_bus()->set_state(Device::State::kSuspended);
  device(parent_index)->device->set_state(Device::State::kSuspended);
  device(child_index)->device->set_state(Device::State::kSuspended);

  ASSERT_NO_FATAL_FAILURES(DoResume(SystemPowerState::FULLY_ON));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(
      CheckResumeReceived(sys_proxy_controller_remote_, SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_NO_FATAL_FAILURES(
      CheckResumeReceived(platform_bus_controller_remote(), SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  // Dont reply yet for the resume
  zx_txid_t txid;
  ASSERT_NO_FATAL_FAILURES(CheckResumeReceived(device(parent_index)->controller_remote,
                                               SystemPowerState::FULLY_ON, &txid));
  coordinator_loop()->RunUntilIdle();

  const uint32_t flags = DEVICE_SUSPEND_FLAG_SUSPEND_RAM;
  // Should be a no-op because resume is in progress.
  ASSERT_NO_FATAL_FAILURES(DoSuspend(flags));

  ASSERT_NO_FATAL_FAILURES(SendResumeReply(device(parent_index)->controller_remote, ZX_OK, txid));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(CheckResumeReceived(device(child_index)->controller_remote,
                                               SystemPowerState::FULLY_ON, ZX_OK));
  coordinator_loop()->RunUntilIdle();
  ASSERT_FALSE(DeviceHasPendingMessages(device(parent_index)->controller_remote));
  ASSERT_FALSE(DeviceHasPendingMessages(device(child_index)->controller_remote));
  ASSERT_EQ(device(parent_index)->device->state(), Device::State::kActive);
  ASSERT_EQ(device(child_index)->device->state(), Device::State::kActive);
}

TEST_F(MultipleDeviceTestCase, DISABLED_ResumeTimeout) {
  ASSERT_OK(coordinator_loop()->StartThread("DevCoordLoop"));
  set_coordinator_loop_thread_running(true);

  async::Loop driver_host_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  ASSERT_OK(driver_host_loop.StartThread("DriverHostLoop"));

  coordinator().sys_device()->set_state(Device::State::kSuspended);
  coordinator().sys_device()->proxy()->set_state(Device::State::kSuspended);
  platform_bus()->set_state(Device::State::kSuspended);

  bool resume_callback_executed = false;
  zx::event resume_received_event;
  zx::event::create(0, &resume_received_event);

  ResumeCallback callback = [&resume_callback_executed,
                             &resume_received_event](zx_status_t status) {
    ASSERT_EQ(status, ZX_ERR_TIMED_OUT);
    resume_callback_executed = true;
    resume_received_event.signal(0, ZX_USER_SIGNAL_0);
  };

  ASSERT_NO_FATAL_FAILURES(DoResume(SystemPowerState::FULLY_ON, std::move(callback)));

  // Dont reply for sys proxy resume. we should timeout
  async::Wait resume_task_sys_proxy(
      sys_proxy_controller_remote_.get(), ZX_CHANNEL_READABLE, 0,
      [this](async_dispatcher_t*, async::Wait*, zx_status_t, const zx_packet_signal_t*) {
        zx_txid_t txid;
        ASSERT_NO_FATAL_FAILURES(
            CheckResumeReceived(sys_proxy_controller_remote_, SystemPowerState::FULLY_ON, &txid));
      });
  ASSERT_OK(resume_task_sys_proxy.Begin(driver_host_loop.dispatcher()));

  // Wait for the event that the callback sets, otherwise the test will quit.
  resume_received_event.wait_one(ZX_USER_SIGNAL_0, zx::time(ZX_TIME_INFINITE), nullptr);
  ASSERT_TRUE(resume_callback_executed);
}

// Test devices are suspended when component lifecycle stop event is received.
TEST_F(MultipleDeviceTestCase, ComponentLifecycleStop) {
  ASSERT_OK(coordinator_loop()->StartThread("DevCoordLoop"));
  set_coordinator_loop_thread_running(true);

  async::Loop devhost_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  ASSERT_OK(devhost_loop.StartThread("DevHostLoop"));

  async::Wait suspend_task_pbus(
      platform_bus_controller_remote().get(), ZX_CHANNEL_READABLE, 0,
      [this](async_dispatcher_t*, async::Wait*, zx_status_t, const zx_packet_signal_t*) {
        CheckSuspendReceivedAndReply(platform_bus_controller_remote(), DEVICE_SUSPEND_FLAG_MEXEC,
                                     ZX_OK);
      });
  ASSERT_OK(suspend_task_pbus.Begin(devhost_loop.dispatcher()));

  async::Wait suspend_task_sys(
      sys_proxy_controller_remote_.get(), ZX_CHANNEL_READABLE, 0,
      [this](async_dispatcher_t*, async::Wait*, zx_status_t, const zx_packet_signal_t*) {
        CheckSuspendReceivedAndReply(sys_proxy_controller_remote_, DEVICE_SUSPEND_FLAG_MEXEC,
                                     ZX_OK);
      });
  ASSERT_OK(suspend_task_sys.Begin(devhost_loop.dispatcher()));

  zx::channel component_lifecycle_client, component_lifecycle_server;
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  ASSERT_OK(zx::channel::create(0, &component_lifecycle_client, &component_lifecycle_server));
  SuspendCallback suspend_callback = [&event](zx_status_t status) {
    event.signal(0, ZX_USER_SIGNAL_0);
  };
  ASSERT_OK(devmgr::ComponentLifecycleServer::Create(
      coordinator_loop()->dispatcher(), &coordinator(), std::move(component_lifecycle_server),
      std::move(suspend_callback)));
  fidl::WireSyncClient<fuchsia_process_lifecycle::Lifecycle> client(
      std::move(component_lifecycle_client));
  auto result = client.Stop();
  ASSERT_OK(result.status());
  event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), nullptr);
  ASSERT_FALSE(suspend_task_pbus.is_pending());
  ASSERT_FALSE(suspend_task_sys.is_pending());
}

TEST_F(MultipleDeviceTestCase, SetTerminationSystemState_fidl) {
  ASSERT_OK(coordinator_loop()->StartThread("DevCoordLoop"));
  set_coordinator_loop_thread_running(true);
  zx::channel system_state_transition_client, system_state_transition_server;
  ASSERT_OK(
      zx::channel::create(0, &system_state_transition_client, &system_state_transition_server));

  std::unique_ptr<SystemStateManager> state_mgr;
  ASSERT_OK(SystemStateManager::Create(coordinator_loop()->dispatcher(), &coordinator(),
                                       std::move(system_state_transition_server), &state_mgr));
  coordinator().set_system_state_manager(std::move(state_mgr));
  auto response = fidl::WireCall<fuchsia_device_manager::SystemStateTransition>(
                      zx::unowned_channel(system_state_transition_client.get()))
                      .SetTerminationSystemState(
                          fuchsia_hardware_power_statecontrol::wire::SystemPowerState::POWEROFF);

  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->result.is_err()) {
    call_status = response->result.err();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(coordinator().shutdown_system_state(),
            fuchsia_hardware_power_statecontrol::wire::SystemPowerState::POWEROFF);
}

TEST_F(MultipleDeviceTestCase, SetTerminationSystemState_svchost_fidl) {
  ASSERT_OK(coordinator_loop()->StartThread("DevCoordLoop"));
  set_coordinator_loop_thread_running(true);

  zx::channel services, services_remote;
  ASSERT_OK(zx::channel::create(0, &services, &services_remote));

  svc::Outgoing outgoing{coordinator_loop()->dispatcher()};
  ASSERT_OK(coordinator().InitOutgoingServices(outgoing.svc_dir()));
  ASSERT_OK(outgoing.Serve(std::move(services_remote)));

  zx::channel channel, channel_remote;
  ASSERT_OK(zx::channel::create(0, &channel, &channel_remote));
  ASSERT_OK(fdio_service_connect_at(
      services.get(),
      fidl::DiscoverableProtocolDefaultPath<fuchsia_device_manager::SystemStateTransition>,
      channel_remote.release()));

  auto response = fidl::WireCall<fuchsia_device_manager::SystemStateTransition>(
                      zx::unowned_channel(channel.get()))
                      .SetTerminationSystemState(
                          fuchsia_hardware_power_statecontrol::wire::SystemPowerState::MEXEC);
  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->result.is_err()) {
    call_status = response->result.err();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(coordinator().shutdown_system_state(),
            fuchsia_hardware_power_statecontrol::wire::SystemPowerState::MEXEC);
}

TEST_F(MultipleDeviceTestCase, SetTerminationSystemState_fidl_wrong_state) {
  ASSERT_OK(coordinator_loop()->StartThread("DevCoordLoop"));
  set_coordinator_loop_thread_running(true);

  zx::channel system_state_transition_client, system_state_transition_server;
  ASSERT_OK(
      zx::channel::create(0, &system_state_transition_client, &system_state_transition_server));

  std::unique_ptr<SystemStateManager> state_mgr;
  ASSERT_OK(SystemStateManager::Create(coordinator_loop()->dispatcher(), &coordinator(),
                                       std::move(system_state_transition_server), &state_mgr));
  coordinator().set_system_state_manager(std::move(state_mgr));

  auto response = fidl::WireCall<fuchsia_device_manager::SystemStateTransition>(
                      zx::unowned_channel(system_state_transition_client.get()))
                      .SetTerminationSystemState(
                          fuchsia_hardware_power_statecontrol::wire::SystemPowerState::FULLY_ON);

  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->result.is_err()) {
    call_status = response->result.err();
  }
  ASSERT_EQ(call_status, ZX_ERR_INVALID_ARGS);
  // Default shutdown_system_state in test is MEXEC.
  ASSERT_EQ(coordinator().shutdown_system_state(),
            fuchsia_hardware_power_statecontrol::wire::SystemPowerState::MEXEC);
}

TEST_F(MultipleDeviceTestCase, PowerManagerRegistration) {
  ASSERT_OK(coordinator_loop()->StartThread("DevCoordLoop"));
  set_coordinator_loop_thread_running(true);

  zx::channel system_state_transition_client, system_state_transition_server;
  ASSERT_OK(
      zx::channel::create(0, &system_state_transition_client, &system_state_transition_server));

  std::unique_ptr<SystemStateManager> state_mgr;
  ASSERT_OK(SystemStateManager::Create(coordinator_loop()->dispatcher(), &coordinator(),
                                       std::move(system_state_transition_server), &state_mgr));
  coordinator().set_system_state_manager(std::move(state_mgr));

  MockPowerManager mock_power_manager;
  zx::channel mock_power_manager_client, mock_power_manager_server;
  zx::channel dev_local, dev_remote;
  ASSERT_OK(zx::channel::create(0, &dev_local, &dev_remote));
  ASSERT_OK(zx::channel::create(0, &mock_power_manager_client, &mock_power_manager_server));
  ASSERT_OK(fidl::BindSingleInFlightOnly(
      coordinator_loop()->dispatcher(), std::move(mock_power_manager_server), &mock_power_manager));
  ASSERT_OK(coordinator().RegisterWithPowerManager(std::move(mock_power_manager_client),
                                                   std::move(system_state_transition_client),
                                                   std::move(dev_local)));
  mock_power_manager.wait_until_register_called();
}

TEST_F(MultipleDeviceTestCase, DevfsWatcherCleanup) {
  Devnode* root_node = coordinator().root_device()->self;
  ASSERT_FALSE(devfs_has_watchers(root_node));

  // Create the watcher and make sure it's been registered.
  zx::channel local, remote;
  ASSERT_OK(zx::channel::create(0, &local, &remote));
  ASSERT_OK(devfs_watch(root_node, std::move(remote), fuchsia_io::wire::WATCH_MASK_ADDED));
  ASSERT_TRUE(devfs_has_watchers(root_node));

  // Free our channel and make sure it gets de-registered.
  local.reset();
  coordinator_loop()->RunUntilIdle();
  ASSERT_FALSE(devfs_has_watchers(root_node));
}

TEST_F(MultipleDeviceTestCase, DevfsUnsupportedAPICheck) {
  zx::channel chan = devfs_root_clone();
  fidl::Client<fuchsia_io::DirectoryAdmin> client(std::move(chan),
                                                  coordinator_loop()->dispatcher());

  {
    auto result = client->GetDevicePath([](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    zx::channel s, c;
    ASSERT_EQ(ZX_OK, zx::channel::create(0, &s, &c));
    auto result =
        client->Mount(std::move(c), [](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    zx::channel s, c;
    ASSERT_EQ(ZX_OK, zx::channel::create(0, &s, &c));
    auto result = client->MountAndCreate(
        std::move(c), "", 0, [](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    auto result = client->Unmount([](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    auto result = client->UnmountNode([](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    auto result = client->GetDevicePath([](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    zx::channel s, c;
    ASSERT_EQ(ZX_OK, zx::channel::create(0, &s, &c));
    auto result = client->Link("", std::move(s), "",
                               [](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    zx::channel s, c;
    ASSERT_EQ(ZX_OK, zx::channel::create(0, &s, &c));
    auto result = client->Rename("", std::move(s), "",
                                 [](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    auto result = client->GetToken([](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    fuchsia_io::wire::NodeAttributes attrs = {};
    auto result =
        client->SetAttr(0, attrs, [](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }
  {
    auto result = client->Sync([](auto* ret) { ASSERT_EQ(ret->s, ZX_ERR_NOT_SUPPORTED); });
    ASSERT_EQ(result.status(), ZX_OK);
  }

  coordinator_loop()->RunUntilIdle();
}

// Check that UnregisterSystemStorageForShutdown works when no system devices exist.
TEST_F(MultipleDeviceTestCase, UnregisterSystemStorageForShutdown_NoSystemDevices) {
  bool finished = false;
  zx_status_t remove_status;
  coordinator().suspend_handler().UnregisterSystemStorageForShutdown([&](zx_status_t status) {
    finished = true;
    remove_status = status;
  });
  coordinator_loop()->RunUntilIdle();
  ASSERT_TRUE(finished);
  ASSERT_EQ(remove_status, ZX_OK);
}

// Check that UnregisterSystemStorageForShutdown removes system devices but not boot devices.
TEST_F(MultipleDeviceTestCase, UnregisterSystemStorageForShutdown_DevicesRemoveCorrectly) {
  // Create a system device.
  size_t system_device_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(platform_bus(), "system-1", 0 /* protocol id */,
                                     "/system/driver/my-device.so", &system_device_index));
  fbl::RefPtr<Device> system_device = device(system_device_index)->device;

  // Create a child of the system device that lives in boot.
  size_t child_boot_device_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(system_device, "boot-1", 0 /* protocol id */,
                                     "/boot/driver/my-device.so", &child_boot_device_index));
  fbl::RefPtr<Device> child_boot_device = device(child_boot_device_index)->device;

  // Create a child of the system device that lives in system.
  size_t child_system_device_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(system_device, "system-2", 0 /* protocol id */,
                                     "/system/driver/my-device.so", &child_system_device_index));
  fbl::RefPtr<Device> child_system_device = device(child_system_device_index)->device;

  // Create a boot device.
  size_t boot_device_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(platform_bus(), "boot-2", 0 /* protocol id */,
                                     "/boot/driver/my-device.so", &boot_device_index));
  fbl::RefPtr<Device> boot_device = device(boot_device_index)->device;

  // Create a child of the boot that lives in system.
  size_t boot_child_system_device_index;
  ASSERT_NO_FATAL_FAILURES(AddDevice(platform_bus(), "system-3", 0 /* protocol id */,
                                     "/system/driver/my-device.so",
                                     &boot_child_system_device_index));
  fbl::RefPtr<Device> boot_child_system_device = device(boot_child_system_device_index)->device;

  coordinator_loop()->RunUntilIdle();

  bool finished = false;
  zx_status_t remove_status;
  coordinator().suspend_handler().UnregisterSystemStorageForShutdown([&](zx_status_t status) {
    finished = true;
    remove_status = status;
  });
  coordinator_loop()->RunUntilIdle();

  // Respond to Suspends. Go children then parents.
  ASSERT_NO_FATAL_FAILURES(
      CheckSuspendReceivedAndReply(device(boot_child_system_device_index)->controller_remote,
                                   DEVICE_SUSPEND_FLAG_REBOOT, ZX_OK));
  ASSERT_NO_FATAL_FAILURES(CheckSuspendReceivedAndReply(
      device(child_system_device_index)->controller_remote, DEVICE_SUSPEND_FLAG_REBOOT, ZX_OK));
  ASSERT_NO_FATAL_FAILURES(CheckSuspendReceivedAndReply(
      device(child_boot_device_index)->controller_remote, DEVICE_SUSPEND_FLAG_REBOOT, ZX_OK));
  coordinator_loop()->RunUntilIdle();

  ASSERT_NO_FATAL_FAILURES(CheckSuspendReceivedAndReply(
      device(system_device_index)->controller_remote, DEVICE_SUSPEND_FLAG_REBOOT, ZX_OK));
  coordinator_loop()->RunUntilIdle();

  // Check that the callback was called.
  ASSERT_TRUE(finished);
  ASSERT_EQ(remove_status, ZX_OK);

  // Check that our devices were suspended.
  ASSERT_EQ(system_device->state(), Device::State::kSuspended);
  ASSERT_EQ(child_boot_device->state(), Device::State::kSuspended);
  ASSERT_EQ(child_system_device->state(), Device::State::kSuspended);
  ASSERT_EQ(boot_child_system_device->state(), Device::State::kSuspended);
}
