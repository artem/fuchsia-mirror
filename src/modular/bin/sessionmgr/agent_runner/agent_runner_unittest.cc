// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/modular/bin/sessionmgr/agent_runner/agent_runner.h"

#include <fuchsia/modular/cpp/fidl.h>
#include <fuchsia/sys/cpp/fidl.h>
#include <fuchsia/testing/modular/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/gtest/real_loop_fixture.h>
#include <lib/svc/cpp/service_namespace.h>
#include <lib/sys/cpp/testing/fake_launcher.h>
#include <lib/zx/object.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>

#include <fs/service.h>
#include <fs/synchronous_vfs.h>
#include <gtest/gtest.h>

#include "src/lib/files/scoped_temp_dir.h"
#include "src/lib/fxl/macros.h"
#include "src/modular/lib/fidl/array_to_string.h"
#include "src/modular/lib/pseudo_dir/pseudo_dir_server.h"

namespace modular_testing {
namespace {

using ::sys::testing::FakeLauncher;

// The choice of "TestProtocol" as the test service is arbitrary, but the
// ConnectToAgentService() tests require an existing service type.
const char* kTestAgentService = fuchsia::testing::modular::TestProtocol::Name_;
constexpr char kTestAgentUrl[] = "file:///my_agent";

// Configuration for testing |ComponentContext| ConnectToAgentService().
struct ConnectToAgentServiceTestConfig {
  // The map of |service_name|->|agent_url| used to look up a service
  // handler |agent_url| by name.
  std::map<std::string, std::string> agent_service_index = {};

  // If true, include the service_name in the |AgentServiceRequest|.
  // This is required for a successful connection.
  bool provide_service_name = false;

  // If true, include the specific handler (agent URL) in the
  // |AgentServiceRequest|. This is *not* required for a successful connection.
  bool provide_handler = false;

  // If true, include the service client-side channel in the
  // |AgentServiceRequest|. This is required for a successful connection.
  bool provide_channel = false;

  // If true, include the |AgentController| in the |AgentServiceRequest|.
  // This is required for a successful connection.
  bool provide_agent_controller = false;
};

// Expected test results.
struct ConnectToAgentServiceExpect {
  // If true, the test should connect to the test agent and verify the
  // agent-side channel has a |koid| that matches the client-side channel.
  bool agent_got_service_request = false;

  // If set to an error code, the service channel should receive the given
  // error.
  zx_status_t service_status = ZX_OK;
};

static zx_koid_t get_object_koid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  if (zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), NULL, NULL) != ZX_OK) {
    return 0;
  }
  return info.koid;
}

class TestAgent : fuchsia::modular::Agent,
                  public fuchsia::sys::ComponentController {
 public:
  TestAgent(zx::channel directory_request,
            fidl::InterfaceRequest<fuchsia::sys::ComponentController> ctrl,
            std::unique_ptr<component::ServiceNamespace> services_ptr = nullptr)
      : controller_(this, std::move(ctrl)),
        agent_binding_(this),
        services_ptr_(std::move(services_ptr)) {
    auto outgoing_dir = std::make_unique<vfs::PseudoDir>();
    outgoing_dir->AddEntry(
        fuchsia::modular::Agent::Name_,
        std::make_unique<vfs::Service>([this](zx::channel channel, async_dispatcher_t* /*unused*/) {
          agent_binding_.Bind(std::move(channel));
        }));
    outgoing_dir_server_ = std::make_unique<modular::PseudoDirServer>(std::move(outgoing_dir));
    outgoing_dir_server_->Serve(std::move(directory_request));
  }

  void KillApplication() { controller_.Unbind(); }

  int connect_call_count() { return connect_call_count_; }

 private:
  // |ComponentController|
  void Kill() override { FX_NOTREACHED(); }
  // |ComponentController|
  void Detach() override { FX_NOTREACHED(); }

  // |fuchsia::modular::Agent|
  void Connect(std::string requestor_url,
               fidl::InterfaceRequest<fuchsia::sys::ServiceProvider> outgoing_services) override {
    ++connect_call_count_;
    if (services_ptr_) {
      services_ptr_->AddBinding(std::move(outgoing_services));
    }
  }

 private:
  fidl::Binding<fuchsia::sys::ComponentController> controller_;
  fidl::Binding<fuchsia::modular::Agent> agent_binding_;
  std::unique_ptr<component::ServiceNamespace> services_ptr_;

  // `outgoing_dir_server_` must be initialized after `agent_binding_` (which itself serves
  // `services_ptr_`) so that it is guaranteed to be destroyed *before* `agent_binding_` to protect
  // access to `services_ptr_`. See fxb/49304.
  std::unique_ptr<modular::PseudoDirServer> outgoing_dir_server_;

  // The number of times Connect() has been called.
  int connect_call_count_ = 0;

  FXL_DISALLOW_COPY_AND_ASSIGN(TestAgent);
};

class AgentRunnerTest : public gtest::RealLoopFixture {
 public:
  AgentRunnerTest() = default;

  void SetUp() override { gtest::RealLoopFixture::SetUp(); }

  void TearDown() override {
    agent_runner_.reset();

    gtest::RealLoopFixture::TearDown();
  }

 protected:
  modular::AgentRunner* agent_runner() {
    if (agent_runner_ == nullptr) {
      agent_runner_ = std::make_unique<modular::AgentRunner>(
          &launcher_, /*agent_services_factory=*/nullptr, &node_, std::move(agent_service_index_));
    }
    return agent_runner_.get();
  }

  void set_agent_service_index(std::map<std::string, std::string> agent_service_index) {
    agent_service_index_ = std::move(agent_service_index);
  }

  template <typename Interface>
  void request_agent_service(ConnectToAgentServiceTestConfig test_config, std::string service_name,
                             std::string agent_url,
                             fidl::InterfaceRequest<Interface> service_request,
                             fuchsia::modular::AgentControllerPtr agent_controller) {
    fuchsia::modular::AgentServiceRequest agent_service_request;
    if (test_config.provide_service_name) {
      agent_service_request.set_service_name(service_name);
    }
    if (test_config.provide_handler) {
      agent_service_request.set_handler(agent_url);
    }
    if (test_config.provide_channel) {
      agent_service_request.set_channel(service_request.TakeChannel());
    }
    if (test_config.provide_agent_controller) {
      agent_service_request.set_agent_controller(agent_controller.NewRequest());
    }
    agent_runner()->ConnectToAgentService("requestor_url", std::move(agent_service_request));
  }

  void execute_connect_to_agent_service_test(ConnectToAgentServiceTestConfig test_config,
                                             ConnectToAgentServiceExpect expect) {
    // Client-side service pointer
    fuchsia::testing::modular::TestProtocolPtr service_ptr;
    auto service_name = service_ptr->Name_;
    auto service_request = service_ptr.NewRequest();
    zx_status_t service_status = ZX_OK;
    service_ptr.set_error_handler(
        [&service_status](zx_status_t status) { service_status = status; });

    // standard AgentController initialization
    fuchsia::modular::AgentControllerPtr agent_controller;
    zx_status_t agent_controller_status = ZX_OK;
    agent_controller.set_error_handler(
        [&agent_controller_status](zx_status_t status) { agent_controller_status = status; });

    // register a service for the agent to serve, and expect the client's
    // request
    auto services_ptr = std::make_unique<component::ServiceNamespace>();
    bool agent_got_service_request = false;
    services_ptr->AddService<fuchsia::testing::modular::TestProtocol>(
        [&agent_got_service_request,
         client_request_koid = get_object_koid(service_request.channel().get())](
            fidl::InterfaceRequest<fuchsia::testing::modular::TestProtocol> request) {
          auto server_request_koid = get_object_koid(request.channel().get());
          EXPECT_EQ(server_request_koid, client_request_koid);
          agent_got_service_request = true;
        });

    // register and launch the test agent, with services
    std::unique_ptr<TestAgent> test_agent;
    launcher()->RegisterComponent(
        kTestAgentUrl, [&test_agent, &services_ptr](
                           fuchsia::sys::LaunchInfo launch_info,
                           fidl::InterfaceRequest<fuchsia::sys::ComponentController> ctrl) {
          test_agent = std::make_unique<TestAgent>(std::move(launch_info.directory_request),
                                                   std::move(ctrl), std::move(services_ptr));
        });

    request_agent_service(test_config, service_name, kTestAgentUrl, std::move(service_request),
                          std::move(agent_controller));

    RunLoopWithTimeoutOrUntil([&] {
      return agent_got_service_request || service_status != ZX_OK ||
             (expect.service_status == ZX_OK && agent_controller_status != ZX_OK);
      // The order of error callbacks is non-deterministic. If checking for a
      // specific service error, wait for it.
    });

    EXPECT_EQ(agent_got_service_request, expect.agent_got_service_request);
    if (!agent_got_service_request) {
      // If the agent successfully got the expected service request, ignore
      // service errors. This test does not actually complete the connection.
      EXPECT_EQ(service_status, expect.service_status);
    }
  }

  FakeLauncher* launcher() { return &launcher_; }

 private:
  FakeLauncher launcher_;
  inspect::Node node_;

  files::ScopedTempDir mq_data_dir_;
  std::unique_ptr<modular::AgentRunner> agent_runner_;
  std::map<std::string, std::string> agent_service_index_;

  FXL_DISALLOW_COPY_AND_ASSIGN(AgentRunnerTest);
};

}  // namespace

// Test that connecting to an agent will start it up.
// Then there should be an fuchsia::modular::Agent.Connect().
TEST_F(AgentRunnerTest, ConnectToAgent) {
  int agent_launch_count = 0;
  std::unique_ptr<TestAgent> test_agent;
  launcher()->RegisterComponent(
      kTestAgentUrl, [&test_agent, &agent_launch_count](
                         fuchsia::sys::LaunchInfo launch_info,
                         fidl::InterfaceRequest<fuchsia::sys::ComponentController> ctrl) {
        test_agent =
            std::make_unique<TestAgent>(std::move(launch_info.directory_request), std::move(ctrl));
        ++agent_launch_count;
      });

  fuchsia::sys::ServiceProviderPtr incoming_services;
  fuchsia::modular::AgentControllerPtr agent_controller;
  agent_runner()->ConnectToAgent("requestor_url", kTestAgentUrl, incoming_services.NewRequest(),
                                 agent_controller.NewRequest());

  RunLoopWithTimeoutOrUntil(
      [&test_agent] { return test_agent && test_agent->connect_call_count() > 0; });
  EXPECT_EQ(1, agent_launch_count);
  EXPECT_EQ(1, test_agent->connect_call_count());

  // Connecting to the same agent again shouldn't launch a new instance and
  // shouldn't re-initialize the existing instance of the agent application,
  // but should call |Connect()|.

  fuchsia::modular::AgentControllerPtr agent_controller2;
  fuchsia::sys::ServiceProviderPtr incoming_services2;
  agent_runner()->ConnectToAgent("requestor_url2", kTestAgentUrl, incoming_services2.NewRequest(),
                                 agent_controller2.NewRequest());
  RunLoopWithTimeoutOrUntil(
      [&test_agent] { return test_agent && test_agent->connect_call_count() > 1; });
  EXPECT_EQ(1, agent_launch_count);
  EXPECT_EQ(2, test_agent->connect_call_count());
}

// Test that if an agent application dies, it is removed from agent runner
// (which means outstanding AgentControllers are closed).
TEST_F(AgentRunnerTest, AgentController) {
  std::unique_ptr<TestAgent> test_agent;
  launcher()->RegisterComponent(
      kTestAgentUrl, [&test_agent](fuchsia::sys::LaunchInfo launch_info,
                                   fidl::InterfaceRequest<fuchsia::sys::ComponentController> ctrl) {
        test_agent =
            std::make_unique<TestAgent>(std::move(launch_info.directory_request), std::move(ctrl));
      });

  fuchsia::sys::ServiceProviderPtr incoming_services;
  fuchsia::modular::AgentControllerPtr agent_controller;
  agent_runner()->ConnectToAgent("requestor_url", kTestAgentUrl, incoming_services.NewRequest(),
                                 agent_controller.NewRequest());

  RunLoopWithTimeoutOrUntil([&test_agent] { return !!test_agent; });
  test_agent->KillApplication();

  // fuchsia::modular::Agent application died, so check that
  // fuchsia::modular::AgentController dies here.
  agent_controller.set_error_handler(
      [&agent_controller](zx_status_t status) { agent_controller.Unbind(); });
  RunLoopWithTimeoutOrUntil([&agent_controller] { return !agent_controller.is_bound(); });
  EXPECT_FALSE(agent_controller.is_bound());
}

TEST_F(AgentRunnerTest, NoServiceNameInAgentServiceRequest) {
  ConnectToAgentServiceTestConfig test_config;
  // test_config.provide_service_name = true;
  test_config.provide_channel = true;
  test_config.provide_agent_controller = true;

  ConnectToAgentServiceExpect expect;
  expect.agent_got_service_request = false;
  expect.service_status = ZX_ERR_PEER_CLOSED;

  execute_connect_to_agent_service_test(test_config, expect);
}

TEST_F(AgentRunnerTest, NoChannelInAgentServiceRequest) {
  ConnectToAgentServiceTestConfig test_config;
  test_config.provide_service_name = true;
  // test_config.provide_channel = true;
  test_config.provide_agent_controller = true;

  ConnectToAgentServiceExpect expect;
  expect.agent_got_service_request = false;
  expect.service_status = ZX_ERR_PEER_CLOSED;

  execute_connect_to_agent_service_test(test_config, expect);
}

TEST_F(AgentRunnerTest, NoAgentControllerInAgentServiceRequest) {
  ConnectToAgentServiceTestConfig test_config;
  test_config.provide_service_name = true;
  test_config.provide_channel = true;
  // test_config.provide_agent_controller = true;

  ConnectToAgentServiceExpect expect;
  expect.agent_got_service_request = false;
  expect.service_status = ZX_ERR_PEER_CLOSED;

  execute_connect_to_agent_service_test(test_config, expect);
}

TEST_F(AgentRunnerTest, NoAgentForServiceName) {
  ConnectToAgentServiceTestConfig test_config;
  // Default agent_service_index is empty, so agent_url will not be found
  test_config.provide_service_name = true;
  test_config.provide_channel = true;
  test_config.provide_agent_controller = true;

  ConnectToAgentServiceExpect expect;
  expect.agent_got_service_request = false;
  expect.service_status = ZX_ERR_NOT_FOUND;

  execute_connect_to_agent_service_test(test_config, expect);
}

TEST_F(AgentRunnerTest, ConnectToServiceName) {
  ConnectToAgentServiceTestConfig test_config;
  // requested service will map to test agent
  test_config.agent_service_index = {
      {kTestAgentService, kTestAgentUrl},
  };
  test_config.provide_service_name = true;
  test_config.provide_channel = true;
  test_config.provide_agent_controller = true;

  ConnectToAgentServiceExpect expect;
  expect.agent_got_service_request = true;
  expect.service_status = ZX_ERR_PEER_CLOSED;

  set_agent_service_index(test_config.agent_service_index);

  execute_connect_to_agent_service_test(test_config, expect);
}

}  // namespace modular_testing
