// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef NINSPECT

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/inspectable.h"

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/inspect.h"

namespace bt {

namespace {

using namespace inspect::testing;

template <typename T>
class TestProperty {
 public:
  using ValueCallback = fit::function<void(const T& value)>;
  TestProperty() = default;
  TestProperty(T value, ValueCallback cb) : value_(value), value_cb_(std::move(cb)) {}

  void Set(const T& value) {
    value_ = value;
    if (value_cb_) {
      value_cb_(value_);
    }
  }

 private:
  T value_;
  fit::function<void(const T& value)> value_cb_;
};

struct TestValue {
  explicit TestValue(int val) : value(val) {}
  void set_value(int val) { value = val; }
  int value;
};

struct StringValue {
  std::string ToString() const { return value; }
  std::string value;
};

}  // namespace

using TestInspectable = Inspectable<int, TestProperty<int>, int>;

TEST(InspectableTest, SetPropertyChangesProperty) {
  std::optional<int> prop_value_0;
  auto prop_cb_0 = [&](auto value) { prop_value_0 = value; };

  TestInspectable inspectable(0, TestProperty<int>(1, prop_cb_0));
  EXPECT_EQ(0, *inspectable);
  ASSERT_TRUE(prop_value_0.has_value());
  EXPECT_EQ(0, prop_value_0.value());

  std::optional<int> prop_value_1;
  auto prop_cb_1 = [&](auto value) { prop_value_1 = value; };
  inspectable.SetProperty(TestProperty<int>(1, prop_cb_1));
  ASSERT_TRUE(prop_value_1.has_value());
  // New property should be updated with current value.
  EXPECT_EQ(0, prop_value_1.value());

  inspectable.Set(2);
  // Old property should not be updated.
  ASSERT_TRUE(prop_value_0.has_value());
  EXPECT_EQ(0, prop_value_0.value());
  // New property should be updated.
  ASSERT_TRUE(prop_value_1.has_value());
  EXPECT_EQ(2, prop_value_1.value());
}

TEST(InspectableTest, ConstructionWithValueOnly) {
  TestInspectable inspectable(2);
  EXPECT_EQ(2, *inspectable);

  // Updating property should be a no-op, but value should still be updated.
  inspectable.Set(1);
  EXPECT_EQ(1, *inspectable);
}

TEST(InspectableTest, PropertyValueUpdatedOnConstruction) {
  std::optional<int> prop_value;
  auto prop_cb = [&](auto value) { prop_value = value; };

  TestInspectable inspectable(0, TestProperty<int>(1, std::move(prop_cb)));
  EXPECT_EQ(0, *inspectable);
  ASSERT_TRUE(prop_value.has_value());
  // Property value should not still be 1.
  EXPECT_EQ(0, prop_value.value());
}

TEST(InspectableTest, Set) {
  std::optional<int> prop_value;
  auto prop_cb = [&](auto value) { prop_value = value; };

  TestInspectable inspectable(0, TestProperty<int>(0, std::move(prop_cb)));
  inspectable.Set(1);
  EXPECT_EQ(1, *inspectable);
  ASSERT_TRUE(prop_value.has_value());
  EXPECT_EQ(1, prop_value.value());
}

TEST(InspectableTest, UpdateValueThroughMutable) {
  std::optional<int> prop_value;
  auto prop_cb = [&](auto value) { prop_value = value; };

  Inspectable<TestValue, TestProperty<int>, int> inspectable(
      TestValue(0), TestProperty<int>(0, std::move(prop_cb)),
      [](const TestValue& v) { return v.value; });
  inspectable.Mutable()->set_value(1);
  EXPECT_EQ(1, inspectable->value);
  ASSERT_TRUE(prop_value.has_value());
  EXPECT_EQ(1, prop_value.value());
}

TEST(InspectableTest, MakeToStringInspectConvertFunction) {
  const auto kPropertyName = "test_property";
  inspect::Inspector inspector;
  auto& root = inspector.GetRoot();

  StringInspectable inspectable(StringValue{""}, root.CreateString(kPropertyName, ""),
                                MakeToStringInspectConvertFunction());

  const std::string kExpectedValue = "fuchsia";
  inspectable.Mutable()->value = kExpectedValue;
  EXPECT_EQ(kExpectedValue, inspectable->value);

  auto hierarchy = inspect::ReadFromVmo(inspector.DuplicateVmo());
  ASSERT_TRUE(hierarchy.is_ok());
  EXPECT_THAT(
      hierarchy.take_value(),
      AllOf(NodeMatches(PropertyList(ElementsAre(StringIs(kPropertyName, kExpectedValue))))));
}

TEST(InspectableTest, MakeContainerOfToStringConvertFunction) {
  const auto kPropertyName = "test_property";
  inspect::Inspector inspector;
  auto& root = inspector.GetRoot();

  std::array values = {StringValue{"fuchsia"}, StringValue{"purple"}, StringValue{"magenta"}};
  StringInspectable inspectable(std::move(values), root.CreateString(kPropertyName, ""),
                                MakeContainerOfToStringConvertFunction(
                                    {.prologue = "👉", .delimiter = "🥺", .epilogue = "👈"}));

  auto hierarchy = inspect::ReadFromVmo(inspector.DuplicateVmo());
  ASSERT_TRUE(hierarchy.is_ok());
  EXPECT_THAT(hierarchy.take_value(), AllOf(NodeMatches(PropertyList(ElementsAre(StringIs(
                                          kPropertyName, "👉fuchsia🥺purple🥺magenta👈"))))));
}

TEST(InspectableTest, InspectRealStringProperty) {
  const auto kPropertyName = "test_property";

  inspect::Inspector inspector;
  auto& root = inspector.GetRoot();
  StringInspectable inspectable(std::string("A"));
  inspectable.AttachInspect(root, kPropertyName);

  auto hierarchy = inspect::ReadFromVmo(inspector.DuplicateVmo());
  ASSERT_TRUE(hierarchy.is_ok());
  EXPECT_THAT(hierarchy.take_value(),
              AllOf(NodeMatches(PropertyList(ElementsAre(StringIs(kPropertyName, "A"))))));

  inspectable.Set("B");

  hierarchy = inspect::ReadFromVmo(inspector.DuplicateVmo());
  ASSERT_TRUE(hierarchy.is_ok());
  EXPECT_THAT(hierarchy.take_value(),
              AllOf(NodeMatches(PropertyList(ElementsAre(StringIs(kPropertyName, "B"))))));
}

}  // namespace bt

#endif  // NINSPECT
