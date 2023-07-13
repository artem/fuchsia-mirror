// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/lib/acpi/fidl.h"

#include <fidl/fuchsia.hardware.acpi/cpp/wire.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/resource.h>

#include <zxtest/zxtest.h>

#include "fidl/fuchsia.hardware.acpi/cpp/wire_types.h"
#include "lib/fake-resource/resource.h"
#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/test/mock-acpi.h"

namespace facpi = fuchsia_hardware_acpi::wire;

using acpi::test::Device;
using EvaluateObjectRequestView =
    fidl::WireServer<fuchsia_hardware_acpi::Device>::EvaluateObjectRequestView;
using EvaluateObjectCompleter =
    fidl::WireServer<fuchsia_hardware_acpi::Device>::EvaluateObjectCompleter;
using fuchsia_hardware_acpi::wire::EvaluateObjectMode;

namespace {

void CheckEq(ACPI_OBJECT value, ACPI_OBJECT expected) {
  ASSERT_EQ(value.Type, expected.Type);
  switch (value.Type) {
    case ACPI_TYPE_INTEGER:
      ASSERT_EQ(value.Integer.Value, expected.Integer.Value);
      break;
    case ACPI_TYPE_STRING:
      ASSERT_EQ(value.String.Length, expected.String.Length);
      ASSERT_STREQ(value.String.Pointer, expected.String.Pointer);
      break;
    case ACPI_TYPE_PACKAGE:
      ASSERT_EQ(value.Package.Count, expected.Package.Count);
      for (size_t i = 0; i < value.Package.Count; i++) {
        ASSERT_NO_FATAL_FAILURE(CheckEq(value.Package.Elements[i], expected.Package.Elements[i]));
      }
      break;
    case ACPI_TYPE_BUFFER:
      ASSERT_EQ(value.Buffer.Length, expected.Buffer.Length);
      ASSERT_BYTES_EQ(value.Buffer.Pointer, expected.Buffer.Pointer, value.Buffer.Length);
      break;
    case ACPI_TYPE_POWER:
      ASSERT_EQ(value.PowerResource.ResourceOrder, expected.PowerResource.ResourceOrder);
      ASSERT_EQ(value.PowerResource.SystemLevel, expected.PowerResource.SystemLevel);
      break;
    case ACPI_TYPE_PROCESSOR:
      ASSERT_EQ(value.Processor.PblkAddress, expected.Processor.PblkAddress);
      ASSERT_EQ(value.Processor.PblkLength, expected.Processor.PblkLength);
      ASSERT_EQ(value.Processor.ProcId, expected.Processor.ProcId);
      break;
    case ACPI_TYPE_LOCAL_REFERENCE:
      ASSERT_EQ(value.Reference.ActualType, expected.Reference.ActualType);
      ASSERT_EQ(value.Reference.Handle, expected.Reference.Handle);
      break;
    default:
      ASSERT_FALSE(true, "Unexpected object type");
  }
}

void CheckEq(facpi::Object value, facpi::Object expected) {
  using Tag = fuchsia_hardware_acpi::wire::Object::Tag;
  ASSERT_EQ(value.Which(), expected.Which());
  switch (value.Which()) {
    case Tag::kIntegerVal: {
      ASSERT_EQ(value.integer_val(), expected.integer_val());
      break;
    }
    case Tag::kStringVal: {
      ASSERT_EQ(value.string_val().size(), expected.string_val().size());
      ASSERT_BYTES_EQ(value.string_val().data(), expected.string_val().data(),
                      value.string_val().size());
      break;
    }
    case Tag::kPackageVal: {
      auto &val_list = value.package_val().value;
      auto &exp_list = expected.package_val().value;
      ASSERT_EQ(val_list.count(), exp_list.count());
      for (size_t i = 0; i < val_list.count(); i++) {
        ASSERT_NO_FATAL_FAILURE(CheckEq(val_list[i], exp_list[i]));
      }
      break;
    }
    case Tag::kBufferVal: {
      auto &val = value.buffer_val();
      auto &exp = expected.buffer_val();
      ASSERT_EQ(val.count(), exp.count());
      ASSERT_BYTES_EQ(val.data(), exp.data(), val.count());
      break;
    }
    case Tag::kPowerResourceVal: {
      auto &val = value.power_resource_val();
      auto &exp = expected.power_resource_val();
      ASSERT_EQ(val.resource_order, exp.resource_order);
      ASSERT_EQ(val.system_level, exp.system_level);
      break;
    }
    case Tag::kProcessorVal: {
      auto &val = value.processor_val();
      auto &exp = expected.processor_val();
      ASSERT_EQ(val.id, exp.id);
      ASSERT_EQ(val.pblk_address, exp.pblk_address);
      ASSERT_EQ(val.pblk_length, exp.pblk_length);
      break;
    }
    case Tag::kReferenceVal: {
      auto &val = value.reference_val();
      auto &exp = expected.reference_val();
      ASSERT_EQ(val.path.size(), exp.path.size());
      ASSERT_BYTES_EQ(val.path.data(), exp.path.data(), val.path.size());
      ASSERT_EQ(val.object_type, exp.object_type);
      break;
    }
    default:
      FAIL("Unexpected tag: %" PRIu64, static_cast<fidl_xunion_tag_t>(value.Which()));
  }
}

acpi::UniquePtr<ACPI_RESOURCE> MakeResources(cpp20::span<ACPI_RESOURCE> resources) {
  ACPI_RESOURCE *alloc = static_cast<ACPI_RESOURCE *>(ACPI_ALLOCATE_ZEROED(resources.size_bytes()));
  ZX_ASSERT(alloc != nullptr);
  memcpy(alloc, resources.data(), resources.size_bytes());
  return acpi::UniquePtr<ACPI_RESOURCE>(alloc);
}

}  // namespace

class FidlEvaluateObjectTest : public zxtest::Test {
 public:
  void SetUp() override { acpi_.SetDeviceRoot(std::make_unique<Device>("\\")); }

  void InsertDeviceBelow(std::string path, std::unique_ptr<Device> d) {
    Device *parent = acpi_.GetDeviceRoot()->FindByPath(path);
    ASSERT_NE(parent, nullptr);
    parent->AddChild(std::move(d));
  }

 protected:
  acpi::test::MockAcpi acpi_;
};

TEST_F(FidlEvaluateObjectTest, TestCantEvaluateParent) {
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\", std::make_unique<Device>("_SB_")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI0")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_.PCI0", std::make_unique<Device>("I2C0")));

  acpi::EvaluateObjectFidlHelper helper(
      {}, &acpi_, acpi_.GetDeviceRoot()->FindByPath("\\_SB_.PCI0.I2C0"), "\\_SB_.PCI0",
      EvaluateObjectMode::kPlainObject, fidl::VectorView<facpi::Object>(nullptr, 0));

  auto result = helper.ValidateAndLookupPath("\\_SB_.PCI0");
  ASSERT_EQ(result.status_value(), AE_ACCESS);
}

TEST_F(FidlEvaluateObjectTest, TestCantEvaluateSibling) {
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\", std::make_unique<Device>("_SB_")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI0")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI1")));

  acpi::EvaluateObjectFidlHelper helper(
      {}, &acpi_, acpi_.GetDeviceRoot()->FindByPath("\\_SB_.PCI1"), "\\_SB_.PCI0",
      EvaluateObjectMode::kPlainObject, fidl::VectorView<facpi::Object>(nullptr, 0));

  auto result = helper.ValidateAndLookupPath("\\_SB_.PCI0");
  ASSERT_EQ(result.status_value(), AE_ACCESS);
}

TEST_F(FidlEvaluateObjectTest, TestCanEvaluateChild) {
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\", std::make_unique<Device>("_SB_")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI0")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_.PCI0", std::make_unique<Device>("I2C0")));

  acpi::EvaluateObjectFidlHelper helper(
      {}, &acpi_, acpi_.GetDeviceRoot()->FindByPath("\\_SB_.PCI0"), "I2C0",
      EvaluateObjectMode::kPlainObject, fidl::VectorView<facpi::Object>(nullptr, 0));

  ACPI_HANDLE hnd;
  auto result = helper.ValidateAndLookupPath("I2C0", &hnd);
  ASSERT_EQ(result.status_value(), AE_OK);
  ASSERT_EQ(result.value(), "\\_SB_.PCI0.I2C0");
  ASSERT_EQ(hnd, acpi_.GetDeviceRoot()->FindByPath("\\_SB_.PCI0.I2C0"));
}

TEST_F(FidlEvaluateObjectTest, TestDecodeInteger) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  facpi::Object obj;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;
  obj = facpi::Object::WithIntegerVal(alloc, 42);

  auto status = helper.DecodeObject(obj, &out);
  ASSERT_OK(status.status_value());
  ASSERT_NO_FATAL_FAILURE(CheckEq(out, ACPI_OBJECT{
                                           .Integer =
                                               {
                                                   .Type = ACPI_TYPE_INTEGER,
                                                   .Value = 42,
                                               },
                                       }));
}

TEST_F(FidlEvaluateObjectTest, TestDecodeString) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  facpi::Object obj;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;
  obj = facpi::Object::WithStringVal(alloc, "test string");
  auto status = helper.DecodeObject(obj, &out);
  ASSERT_OK(status.status_value());
  ASSERT_NO_FATAL_FAILURE(CheckEq(out, ACPI_OBJECT{
                                           .String =
                                               {
                                                   .Type = ACPI_TYPE_STRING,
                                                   .Length = 11,
                                                   .Pointer = const_cast<char *>("test string"),
                                               },
                                       }));
}

TEST_F(FidlEvaluateObjectTest, TestDecodeBuffer) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  facpi::Object obj;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;
  static constexpr uint8_t kBuffer[] = {0x12, 0x34, 0x56, 0x78, 0x76, 0x54, 0x32, 0x10};
  obj = facpi::Object::WithBufferVal(
      alloc,
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t *>(kBuffer), std::size(kBuffer)));
  auto status = helper.DecodeObject(obj, &out);
  ASSERT_OK(status.status_value());
  ASSERT_NO_FATAL_FAILURE(CheckEq(out, ACPI_OBJECT{
                                           .Buffer =
                                               {
                                                   .Type = ACPI_TYPE_BUFFER,
                                                   .Length = std::size(kBuffer),
                                                   .Pointer = const_cast<uint8_t *>(kBuffer),
                                               },
                                       }));
}

TEST_F(FidlEvaluateObjectTest, TestDecodePowerResource) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  facpi::Object obj;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;
  facpi::PowerResource power;
  power.resource_order = 9;
  power.system_level = 32;
  obj = facpi::Object::WithPowerResourceVal(alloc, power);
  auto status = helper.DecodeObject(obj, &out);
  ASSERT_OK(status.status_value());
  ASSERT_NO_FATAL_FAILURE(CheckEq(out, ACPI_OBJECT{
                                           .PowerResource =
                                               {
                                                   .Type = ACPI_TYPE_POWER,
                                                   .SystemLevel = 32,
                                                   .ResourceOrder = 9,
                                               },
                                       }));
}

TEST_F(FidlEvaluateObjectTest, TestDecodeProcessorVal) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  facpi::Object obj;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;
  facpi::Processor processor;
  processor.pblk_address = 0xd00dfeed;
  processor.pblk_length = 0xabc;
  processor.id = 7;
  obj = facpi::Object::WithProcessorVal(alloc, processor);
  auto status = helper.DecodeObject(obj, &out);
  ASSERT_OK(status.status_value());
  ASSERT_NO_FATAL_FAILURE(CheckEq(out, ACPI_OBJECT{
                                           .Processor =
                                               {
                                                   .Type = ACPI_TYPE_PROCESSOR,
                                                   .ProcId = 7,
                                                   .PblkAddress = 0xd00dfeed,
                                                   .PblkLength = 0xabc,
                                               },
                                       }));
}

TEST_F(FidlEvaluateObjectTest, TestDecodeReference) {
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\", std::make_unique<Device>("_SB_")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI0")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_.PCI0", std::make_unique<Device>("I2C0")));

  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot()->FindByPath("\\_SB_"),
                                        "\\_SB_", EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  facpi::Object obj;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;
  facpi::Handle ref;
  ref.object_type = facpi::ObjectType::kDevice;
  ref.path = "PCI0.I2C0";
  obj = facpi::Object::WithReferenceVal(alloc, ref);

  auto status = helper.DecodeObject(obj, &out);
  ASSERT_OK(status.status_value());
  ASSERT_NO_FATAL_FAILURE(
      CheckEq(out, ACPI_OBJECT{
                       .Reference =
                           {
                               .Type = ACPI_TYPE_LOCAL_REFERENCE,
                               .ActualType = ACPI_TYPE_DEVICE,
                               .Handle = acpi_.GetDeviceRoot()->FindByPath("\\_SB_.PCI0.I2C0"),
                           },
                   }));
}

TEST_F(FidlEvaluateObjectTest, TestDecodeParentReferenceFails) {
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\", std::make_unique<Device>("_SB_")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI0")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_.PCI0", std::make_unique<Device>("I2C0")));

  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot()->FindByPath("\\_SB_"),
                                        "\\_SB_", EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  facpi::Object obj;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;
  facpi::Handle ref;
  ref.object_type = facpi::ObjectType::kDevice;
  ref.path = "\\";
  obj = facpi::Object::WithReferenceVal(alloc, ref);

  auto status = helper.DecodeObject(obj, &out);
  ASSERT_EQ(status.status_value(), AE_ACCESS);
}

TEST_F(FidlEvaluateObjectTest, TestDecodePackage) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  std::vector<facpi::Object> elements;
  facpi::Object obj;
  facpi::ObjectList list;
  ACPI_OBJECT out;
  fidl::Arena<> alloc;

  obj = facpi::Object::WithIntegerVal(alloc, 32);
  elements.emplace_back(obj);
  obj = facpi::Object::WithStringVal(alloc, "test string");
  elements.emplace_back(obj);

  list.value = fidl::VectorView<facpi::Object>::FromExternal(elements);
  obj = facpi::Object::WithPackageVal(alloc, list);

  auto status = helper.DecodeObject(obj, &out);
  ASSERT_OK(status.status_value());

  constexpr ACPI_OBJECT kObjects[2] = {
      {.Integer =
           {
               .Type = ACPI_TYPE_INTEGER,
               .Value = 32,
           }},
      {.String =
           {
               .Type = ACPI_TYPE_STRING,
               .Length = 11,
               .Pointer = const_cast<char *>("test string"),
           }},
  };
  ACPI_OBJECT expected = {
      .Package =
          {
              .Type = ACPI_TYPE_PACKAGE,
              .Count = 2,
              .Elements = const_cast<acpi_object *>(kObjects),
          },
  };

  ASSERT_NO_FATAL_FAILURE(CheckEq(out, expected));
}

TEST_F(FidlEvaluateObjectTest, TestDecodeParameters) {
  fidl::Arena<> alloc;
  std::vector<facpi::Object> params;
  params.emplace_back(facpi::Object::WithIntegerVal(alloc, 32));
  params.emplace_back(facpi::Object::WithStringVal(alloc, "hello"));
  constexpr uint8_t kData[] = {1, 2, 3};
  params.emplace_back(facpi::Object::WithBufferVal(
      alloc,
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t *>(kData), std::size(kData))));

  auto view = fidl::VectorView<facpi::Object>::FromExternal(params);
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject, view);

  auto result = helper.DecodeParameters(view);
  ASSERT_OK(result.zx_status_value());
  ACPI_OBJECT expected[] = {
      {.Integer = {.Type = ACPI_TYPE_INTEGER, .Value = 32}},
      {.String = {.Type = ACPI_TYPE_STRING, .Length = 5, .Pointer = const_cast<char *>("hello")}},
      {.Buffer = {.Type = ACPI_TYPE_BUFFER, .Length = 3, .Pointer = const_cast<uint8_t *>(kData)}},
  };
  auto value = std::move(result.value());
  ASSERT_EQ(value.size(), std::size(expected));
  for (size_t i = 0; i < std::size(expected); i++) {
    ASSERT_NO_FATAL_FAILURE(CheckEq(value[i], expected[i]), "param %zd", i);
  }
}

TEST_F(FidlEvaluateObjectTest, TestEncodeInt) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  fidl::Arena<> alloc;
  ACPI_OBJECT obj = {.Integer = {.Type = ACPI_TYPE_INTEGER, .Value = 320}};
  auto result = helper.EncodeObject(alloc, &obj);
  ASSERT_OK(result.zx_status_value());

  facpi::Object expected = facpi::Object::WithIntegerVal(alloc, 320);
  ASSERT_NO_FATAL_FAILURE(CheckEq(result.value(), expected));
}

TEST_F(FidlEvaluateObjectTest, TestEncodeString) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  fidl::Arena<> alloc;
  ACPI_OBJECT obj = {.String = {
                         .Type = ACPI_TYPE_STRING,
                         .Length = 3,
                         .Pointer = const_cast<char *>("abc"),
                     }};
  auto result = helper.EncodeObject(alloc, &obj);
  ASSERT_OK(result.zx_status_value());

  facpi::Object expected = facpi::Object::WithStringVal(alloc, "abc");
  ASSERT_NO_FATAL_FAILURE(CheckEq(result.value(), expected));
}

TEST_F(FidlEvaluateObjectTest, TestEncodeBuffer) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  fidl::Arena<> alloc;
  static constexpr uint8_t kBuffer[] = {0x12, 0x34, 0x56, 0x78, 0x76, 0x54, 0x32, 0x10};
  ACPI_OBJECT obj = {.Buffer = {
                         .Type = ACPI_TYPE_BUFFER,
                         .Length = std::size(kBuffer),
                         .Pointer = const_cast<uint8_t *>(kBuffer),
                     }};
  auto result = helper.EncodeObject(alloc, &obj);
  ASSERT_OK(result.zx_status_value());

  facpi::Object expected = facpi::Object::WithBufferVal(
      alloc,
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t *>(kBuffer), std::size(kBuffer)));
  ASSERT_NO_FATAL_FAILURE(CheckEq(result.value(), expected));
}

TEST_F(FidlEvaluateObjectTest, TestEncodeProcessorVal) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  fidl::Arena<> alloc;
  ACPI_OBJECT obj = {
      .Processor =
          {
              .Type = ACPI_TYPE_PROCESSOR,
              .ProcId = 7,
              .PblkAddress = 0xd00dfeed,
              .PblkLength = 0xabc,
          },
  };
  auto result = helper.EncodeObject(alloc, &obj);
  ASSERT_OK(result.zx_status_value());
  facpi::Processor processor;
  processor.pblk_address = 0xd00dfeed;
  processor.pblk_length = 0xabc;
  processor.id = 7;
  facpi::Object expected = facpi::Object::WithProcessorVal(alloc, processor);

  ASSERT_NO_FATAL_FAILURE(CheckEq(result.value(), expected));
}

TEST_F(FidlEvaluateObjectTest, TestEncodeReference) {
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\", std::make_unique<Device>("_SB_")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI0")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_.PCI0", std::make_unique<Device>("I2C0")));

  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot()->FindByPath("\\_SB_"),
                                        "\\_SB_", EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  fidl::Arena<> alloc;
  ACPI_OBJECT obj = {.Reference = {
                         .Type = ACPI_TYPE_LOCAL_REFERENCE,
                         .ActualType = ACPI_TYPE_DEVICE,
                         .Handle = acpi_.GetDeviceRoot()->FindByPath("\\_SB_.PCI0.I2C0"),
                     }};
  facpi::Object expected;
  facpi::Handle ref;
  ref.object_type = facpi::ObjectType::kDevice;
  ref.path = "\\_SB_.PCI0.I2C0";
  expected = facpi::Object::WithReferenceVal(alloc, ref);

  auto result = helper.EncodeObject(alloc, &obj);
  ASSERT_OK(result.zx_status_value());
  ASSERT_NO_FATAL_FAILURE(CheckEq(result.value(), expected));
}

TEST_F(FidlEvaluateObjectTest, TestEncodeParentReferenceFails) {
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\", std::make_unique<Device>("_SB_")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_", std::make_unique<Device>("PCI0")));
  ASSERT_NO_FATAL_FAILURE(InsertDeviceBelow("\\_SB_.PCI0", std::make_unique<Device>("I2C0")));

  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot()->FindByPath("\\_SB_"),
                                        "\\_SB_", EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  ACPI_OBJECT obj = {.Reference = {
                         .Type = ACPI_TYPE_LOCAL_REFERENCE,
                         .ActualType = ACPI_TYPE_DEVICE,
                         .Handle = acpi_.GetDeviceRoot(),
                     }};
  fidl::Arena<> alloc;
  auto status = helper.EncodeObject(alloc, &obj);
  ASSERT_EQ(status.status_value(), AE_ACCESS);
}

TEST_F(FidlEvaluateObjectTest, TestEncodePackage) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));
  constexpr ACPI_OBJECT kObjects[2] = {
      {.Integer =
           {
               .Type = ACPI_TYPE_INTEGER,
               .Value = 32,
           }},
      {.String =
           {
               .Type = ACPI_TYPE_STRING,
               .Length = 11,
               .Pointer = const_cast<char *>("test string"),
           }},
  };
  ACPI_OBJECT obj = {
      .Package =
          {
              .Type = ACPI_TYPE_PACKAGE,
              .Count = 2,
              .Elements = const_cast<acpi_object *>(kObjects),
          },
  };
  fidl::Arena<> alloc;

  auto result = helper.EncodeObject(alloc, &obj);
  ASSERT_OK(result.zx_status_value());
  std::vector<facpi::Object> elements;
  facpi::ObjectList list;

  elements.emplace_back(facpi::Object::WithIntegerVal(alloc, 32));
  elements.emplace_back(facpi::Object::WithStringVal(alloc, "test string"));

  list.value = fidl::VectorView<facpi::Object>::FromExternal(elements);
  auto expected = facpi::Object::WithPackageVal(alloc, list);

  ASSERT_NO_FATAL_FAILURE(CheckEq(result.value(), expected));
}

TEST_F(FidlEvaluateObjectTest, TestEncodeReturnValue) {
  ACPI_OBJECT obj = {.Integer = {.Type = ACPI_TYPE_INTEGER, .Value = 47}};
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  fidl::Arena<> alloc;
  auto result = helper.EncodeReturnValue(alloc, &obj);
  ASSERT_OK(result.zx_status_value());
  ASSERT_FALSE(result.value().is_err());
  auto &object = result.value().response().result;
  // Expect a value of this size to be encoded in-line.
  auto expected = facpi::Object::WithIntegerVal(alloc, 47);
  ASSERT_NO_FATAL_FAILURE(CheckEq(object->object(), expected));
}

TEST_F(FidlEvaluateObjectTest, TestEncodeMmioResource) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kParseResources,
                                        fidl::VectorView<facpi::Object>(nullptr, 0));

  // Set up a list of fake ACPI_RESOURCEs.
  std::vector<ACPI_RESOURCE> resources;
  resources.emplace_back(ACPI_RESOURCE{
      .Type = ACPI_RESOURCE_TYPE_ADDRESS64,
      .Length = sizeof(ACPI_RESOURCE),
      .Data =
          {
              .Address64 =
                  {
                      .Address =
                          {
                              .Minimum = 32,
                              .AddressLength = zx_system_get_page_size(),
                          },
                  },
          },
  });
  resources.emplace_back(ACPI_RESOURCE{
      .Type = ACPI_RESOURCE_TYPE_END_TAG,
      .Length = sizeof(ACPI_RESOURCE),
  });
  acpi_.SetResource(MakeResources(resources));

  zx_handle_t fake_root_resource;
  ASSERT_OK(fake_root_resource_create(&fake_root_resource));

  // Make a resource that exactly encompasses the MMIO range we expect, so that we ensure the test
  // requests the right range.
  zx_handle_t child_resource;
  ASSERT_OK(zx_resource_create(fake_root_resource, ZX_RSRC_KIND_MMIO, 0,
                               2lu * zx_system_get_page_size(), nullptr, 0, &child_resource));
  helper.SetMmioResource(child_resource);

  fidl::Arena<> arena;
  ACPI_OBJECT fake_object = {
      .Buffer =
          {
              .Type = ACPI_TYPE_BUFFER,
              .Length = 0,
              .Pointer = nullptr,
          },
  };
  auto result = helper.EncodeResourcesReturnValue(arena, &fake_object);
  ASSERT_OK(result.zx_status_value());
  ASSERT_TRUE(result->is_response());
  ASSERT_TRUE(result->response().result.has_value());
  ASSERT_TRUE(result->response().result->is_resources());
  auto &fidl = result->response().result->resources();
  ASSERT_EQ(fidl.count(), 1);
  ASSERT_TRUE(fidl[0].is_mmio());

  ASSERT_EQ(fidl[0].mmio().offset, 32);
  ASSERT_EQ(fidl[0].mmio().size, zx_system_get_page_size());
}

TEST_F(FidlEvaluateObjectTest, TestMethodReturnsNothing) {
  acpi::EvaluateObjectFidlHelper helper({}, &acpi_, acpi_.GetDeviceRoot(), "\\",
                                        EvaluateObjectMode::kPlainObject,
                                        fidl::VectorView<facpi::Object>());

  fidl::Arena<> arena;
  auto result = helper.EncodeReturnValue(arena, nullptr);
  ASSERT_EQ(result.status_value(), AE_OK);
  ASSERT_FALSE(result->response().result.has_value());
}
