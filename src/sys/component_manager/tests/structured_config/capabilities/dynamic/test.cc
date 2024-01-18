// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/test.config/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>

#include <zxtest/zxtest.h>

#include "src/sys/component_manager/tests/structured_config/capabilities/dynamic/config.h"

namespace {

struct ExpectedValues {
  bool my_flag;
  uint8_t my_int;
  uint8_t my_transitional;
};

void CheckValues(fidl::SyncClient<test_config::Config>& client, ExpectedValues values) {
  fidl::Result result = client->Get();
  ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  config::Config my_config = config::Config::CreateFromVmo(std::move(result->config()));
  ASSERT_EQ(my_config.my_flag(), values.my_flag);
  ASSERT_EQ(my_config.my_int(), values.my_int);
  ASSERT_EQ(my_config.transitional(), values.my_transitional);
}

void ConnectAndCheckValues(fidl::SyncClient<fuchsia_component::Realm>& realm,
                           fuchsia_component_decl::ChildRef child_ref, ExpectedValues values) {
  zx::result exposed_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_OK(exposed_endpoints);
  {
    fidl::Result result =
        realm->OpenExposedDir({{std::move(child_ref), std::move(exposed_endpoints->server)}});
    ASSERT_TRUE(result.is_ok());
  }

  zx::result config_endpoints = fidl::CreateEndpoints<test_config::Config>();
  ASSERT_OK(config_endpoints);
  ASSERT_OK(component::ConnectAt(exposed_endpoints->client, std::move(config_endpoints->server)));

  fidl::SyncClient config_client(std::move(config_endpoints->client));
  ASSERT_NO_FATAL_FAILURE(CheckValues(config_client, values));
}

void AddChildComponent(component_testing::RealmBuilder& builder) {
  builder.AddChild("child", "#meta/child.cm");
  builder.AddRoute(component_testing::Route{
      .capabilities = {component_testing::Capability{
          component_testing::Protocol{.name = "test.config.Config"}}},
      .source = component_testing::Ref{component_testing::ChildRef{.name = "child"}},
      .targets = {component_testing::ParentRef{}},
  });
}

TEST(ScTest, CheckValuesVoidOptional) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(true),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyTransitional",
      .value = component_testing::ConfigValue::Uint8(5),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyTransitional"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });
  builder.AddRoute(component_testing::Route{
      .capabilities = {component_testing::Config{.name = "fuchsia.config.MyInt"}},
      .source = component_testing::VoidRef(),
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  ASSERT_NO_FATAL_FAILURE(CheckValues(
      client, {
                  .my_flag = true,
                  // This value is coming from the CVF file because there's a void optional.
                  .my_int = 0,
                  .my_transitional = 5,
              }));
}

TEST(ScTest, CheckValuesNoOptional) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(false),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyTransitional",
      .value = component_testing::ConfigValue::Uint8(5),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyTransitional"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();
  // This call should fail because 'fuchsia.config.MyInt' is not being routed.
  ASSERT_TRUE(result.is_error());
}

TEST(ScTest, CheckValues) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(false),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyInt",
      .value = component_testing::ConfigValue::Uint8(10),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyTransitional",
      .value = component_testing::ConfigValue::Uint8(10),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyInt"},
              component_testing::Config{.name = "fuchsia.config.MyTransitional"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  ASSERT_NO_FATAL_FAILURE(CheckValues(client, {
                                                  .my_flag = false,
                                                  .my_int = 10,
                                                  .my_transitional = 10,
                                              }));
}

TEST(ScTest, NoTransitionalValue) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(false),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyInt",
      .value = component_testing::ConfigValue::Uint8(10),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyInt"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });
  // We are specifically not routing fuchsia.config.MyTransitional.

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  ASSERT_NO_FATAL_FAILURE(CheckValues(client, {
                                                  .my_flag = false,
                                                  .my_int = 10,
                                                  // This value is coming from the CVF file.
                                                  .my_transitional = 5,
                                              }));
}

TEST(ScTest, BadValueType) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Int8(7),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();

  // This call should fail because the component cannot start with the wrong type.
  ASSERT_TRUE(result.is_error());
}

TEST(Collection, CreateChild) {
  zx::result client_end = component::Connect<fuchsia_component::Realm>();
  ASSERT_OK(client_end);
  fidl::SyncClient client = fidl::SyncClient(std::move(client_end.value()));

  fuchsia_component::CreateChildArgs args = fuchsia_component::CreateChildArgs();
  args.config_capabilities(std::vector{
      fuchsia_component_decl::Configuration()
          .name("fuchsia.config.MyFlag")
          .value(fuchsia_component_decl::ConfigValue::WithSingle(
              fuchsia_component_decl::ConfigSingleValue::WithBool_(false))),
      fuchsia_component_decl::Configuration()
          .name("fuchsia.config.MyInt")
          .value(fuchsia_component_decl::ConfigValue::WithSingle(
              fuchsia_component_decl::ConfigSingleValue::WithUint8(10))),
      fuchsia_component_decl::Configuration()
          .name("fuchsia.config.MyTransitional")
          .value(fuchsia_component_decl::ConfigValue::WithSingle(
              fuchsia_component_decl::ConfigSingleValue::WithUint8(10))),
  });

  fidl::Result result = client->CreateChild({{
      .collection = fuchsia_component_decl::CollectionRef().name("collection"),
      .decl = fuchsia_component_decl::Child()
                  .name("test")
                  .url("#meta/child.cm")
                  .startup(fuchsia_component_decl::StartupMode::kLazy),
      .args = std::move(args),
  }});
  ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());

  zx::result exposed_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_OK(exposed_endpoints);
  fuchsia_component_decl::ChildRef child_ref;
  child_ref.collection("collection");
  child_ref.name("test");
  {
    fidl::Result result =
        client->OpenExposedDir({{child_ref, std::move(exposed_endpoints->server)}});
    ASSERT_TRUE(result.is_ok());
  }

  zx::result config_endpoints = fidl::CreateEndpoints<test_config::Config>();
  ASSERT_OK(config_endpoints);
  ASSERT_OK(component::ConnectAt(exposed_endpoints->client, std::move(config_endpoints->server)));

  fidl::SyncClient config_client(std::move(config_endpoints->client));
  {
    fidl::Result result = config_client->Get();
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
    config::Config my_config = config::Config::CreateFromVmo(std::move(result->config()));
    ASSERT_EQ(my_config.my_flag(), false);
    ASSERT_EQ(my_config.my_int(), 10);
    ASSERT_EQ(my_config.transitional(), 10);
  }

  {
    fidl::Result result =
        client->DestroyChild(fuchsia_component_decl::ChildRef("test", "collection"));
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  }
}

TEST(Collection, CreateSameChildTwice) {
  zx::result client_end = component::Connect<fuchsia_component::Realm>();
  ASSERT_OK(client_end);
  fidl::SyncClient client = fidl::SyncClient(std::move(client_end.value()));

  // Create the child once.
  {
    fuchsia_component::CreateChildArgs args = fuchsia_component::CreateChildArgs();
    args.config_capabilities(std::vector{
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyFlag")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithBool_(false))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyInt")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(0))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyTransitional")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(0))),
    });
    fidl::Result result = client->CreateChild({{
        .collection = fuchsia_component_decl::CollectionRef().name("collection"),
        .decl = fuchsia_component_decl::Child()
                    .name("test")
                    .url("#meta/child.cm")
                    .startup(fuchsia_component_decl::StartupMode::kLazy),
        .args = std::move(args),
    }});
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  }

  // Check the results.
  fuchsia_component_decl::ChildRef child_ref;
  child_ref.collection("collection");
  child_ref.name("test");
  ASSERT_NO_FATAL_FAILURE(ConnectAndCheckValues(
      client, child_ref, {.my_flag = false, .my_int = 0, .my_transitional = 0}));

  // Destroy it.
  {
    fidl::Result result =
        client->DestroyChild(fuchsia_component_decl::ChildRef("test", "collection"));
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  }

  // Create the child again.
  {
    fuchsia_component::CreateChildArgs args = fuchsia_component::CreateChildArgs();
    args.config_capabilities(std::vector{
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyFlag")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithBool_(false))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyInt")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(10))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyTransitional")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(10))),
    });
    fidl::Result result = client->CreateChild({{
        .collection = fuchsia_component_decl::CollectionRef().name("collection"),
        .decl = fuchsia_component_decl::Child()
                    .name("test")
                    .url("#meta/child.cm")
                    .startup(fuchsia_component_decl::StartupMode::kLazy),
        .args = std::move(args),
    }});
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  }

  // Check the results.
  ASSERT_NO_FATAL_FAILURE(ConnectAndCheckValues(
      client, child_ref, {.my_flag = false, .my_int = 10, .my_transitional = 10}));

  {
    fidl::Result result =
        client->DestroyChild(fuchsia_component_decl::ChildRef("test", "collection"));
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  }
}

TEST(Collection, CreateTwoChildren) {
  zx::result client_end = component::Connect<fuchsia_component::Realm>();
  ASSERT_OK(client_end);
  fidl::SyncClient client = fidl::SyncClient(std::move(client_end.value()));

  // Create the first child.
  {
    fuchsia_component::CreateChildArgs args = fuchsia_component::CreateChildArgs();
    args.config_capabilities(std::vector{
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyFlag")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithBool_(false))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyInt")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(0))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyTransitional")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(0))),
    });
    fidl::Result result = client->CreateChild({{
        .collection = fuchsia_component_decl::CollectionRef().name("collection"),
        .decl = fuchsia_component_decl::Child()
                    .name("test1")
                    .url("#meta/child.cm")
                    .startup(fuchsia_component_decl::StartupMode::kLazy),
        .args = std::move(args),
    }});
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  }

  // Check the results.
  fuchsia_component_decl::ChildRef child_ref;
  child_ref.collection("collection");
  child_ref.name("test1");
  ASSERT_NO_FATAL_FAILURE(ConnectAndCheckValues(
      client, child_ref, {.my_flag = false, .my_int = 0, .my_transitional = 0}));

  // Create the second child.
  {
    fuchsia_component::CreateChildArgs args = fuchsia_component::CreateChildArgs();
    args.config_capabilities(std::vector{
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyFlag")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithBool_(false))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyInt")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(10))),
        fuchsia_component_decl::Configuration()
            .name("fuchsia.config.MyTransitional")
            .value(fuchsia_component_decl::ConfigValue::WithSingle(
                fuchsia_component_decl::ConfigSingleValue::WithUint8(10))),
    });
    fidl::Result result = client->CreateChild({{
        .collection = fuchsia_component_decl::CollectionRef().name("collection"),
        .decl = fuchsia_component_decl::Child()
                    .name("test2")
                    .url("#meta/child.cm")
                    .startup(fuchsia_component_decl::StartupMode::kLazy),
        .args = std::move(args),
    }});
    ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());
  }

  // Check the results.
  fuchsia_component_decl::ChildRef child_ref2;
  child_ref2.collection("collection");
  child_ref2.name("test2");
  ASSERT_NO_FATAL_FAILURE(ConnectAndCheckValues(
      client, std::move(child_ref2), {.my_flag = false, .my_int = 10, .my_transitional = 10}));
}

}  // namespace
