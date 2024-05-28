// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/inspect/cpp/vmo/types.h"

#include <lib/inspect/cpp/inspect.h>
#include <lib/stdcompat/optional.h>
#include <lib/zx/event.h>

#include <cstdint>

using inspect::internal::ArrayBlockFormat;

namespace inspect {

template <>
internal::NumericProperty<int64_t>::~NumericProperty<int64_t>() {
  if (state_) {
    state_->FreeIntProperty(this);
  }
}

template <>
internal::NumericProperty<int64_t>& internal::NumericProperty<int64_t>::operator=(
    internal::NumericProperty<int64_t>&& other) noexcept {
  if (state_) {
    state_->FreeIntProperty(this);
  }
  state_ = std::move(other.state_);
  name_index_ = other.name_index_;
  value_index_ = other.value_index_;
  return *this;
}

template <>
void internal::NumericProperty<int64_t>::Set(int64_t value) {
  if (state_) {
    state_->SetIntProperty(this, value);
  }
}

template <>
void internal::NumericProperty<int64_t>::Add(int64_t value) {
  if (state_) {
    state_->AddIntProperty(this, value);
  }
}

template <>
void internal::NumericProperty<int64_t>::Subtract(int64_t value) {
  if (state_) {
    state_->SubtractIntProperty(this, value);
  }
}

template <>
internal::NumericProperty<uint64_t>::~NumericProperty<uint64_t>() {
  if (state_) {
    state_->FreeUintProperty(this);
  }
}

template <>
internal::NumericProperty<uint64_t>& internal::NumericProperty<uint64_t>::operator=(
    internal::NumericProperty<uint64_t>&& other) noexcept {
  if (state_) {
    state_->FreeUintProperty(this);
  }
  state_ = std::move(other.state_);
  name_index_ = std::move(other.name_index_);
  value_index_ = std::move(other.value_index_);
  return *this;
}

template <>
void internal::NumericProperty<uint64_t>::Set(uint64_t value) {
  if (state_) {
    state_->SetUintProperty(this, value);
  }
}

template <>
void internal::NumericProperty<uint64_t>::Add(uint64_t value) {
  if (state_) {
    state_->AddUintProperty(this, value);
  }
}

template <>
void internal::NumericProperty<uint64_t>::Subtract(uint64_t value) {
  if (state_) {
    state_->SubtractUintProperty(this, value);
  }
}

template <>
internal::NumericProperty<double>::~NumericProperty<double>() {
  if (state_) {
    state_->FreeDoubleProperty(this);
  }
}

template <>
internal::NumericProperty<double>& internal::NumericProperty<double>::operator=(
    internal::NumericProperty<double>&& other) noexcept {
  if (state_) {
    state_->FreeDoubleProperty(this);
  }
  state_ = std::move(other.state_);
  name_index_ = std::move(other.name_index_);
  value_index_ = std::move(other.value_index_);
  return *this;
}

template <>
void internal::NumericProperty<double>::Set(double value) {
  if (state_) {
    state_->SetDoubleProperty(this, value);
  }
}

template <>
void internal::NumericProperty<double>::Add(double value) {
  if (state_) {
    state_->AddDoubleProperty(this, value);
  }
}

template <>
void internal::NumericProperty<double>::Subtract(double value) {
  if (state_) {
    state_->SubtractDoubleProperty(this, value);
  }
}

template <>
internal::ArrayValue<BorrowedStringValue>::~ArrayValue<BorrowedStringValue>() {
  if (state_) {
    state_->FreeStringArray(this);
  }
}

template <>
internal::ArrayValue<BorrowedStringValue>& internal::ArrayValue<BorrowedStringValue>::operator=(
    internal::ArrayValue<BorrowedStringValue>&& other) noexcept {
  if (state_) {
    state_->FreeStringArray(this);
  }
  state_ = std::move(other.state_);
  name_index_ = other.name_index_;
  value_index_ = other.value_index_;
  return *this;
}

template <>
void internal::ArrayValue<BorrowedStringValue>::Set(size_t index, BorrowedStringValue value) {
  if (state_) {
    state_->SetStringArray(this, index, value);
  }
}

template <>
internal::ArrayValue<int64_t>::~ArrayValue<int64_t>() {
  if (state_) {
    state_->FreeIntArray(this);
  }
}

template <>
internal::ArrayValue<int64_t>& internal::ArrayValue<int64_t>::operator=(
    internal::ArrayValue<int64_t>&& other) noexcept {
  if (state_) {
    state_->FreeIntArray(this);
  }
  state_ = std::move(other.state_);
  name_index_ = std::move(other.name_index_);
  value_index_ = std::move(other.value_index_);
  return *this;
}

template <>
void internal::ArrayValue<int64_t>::Set(size_t index, int64_t value) {
  if (state_) {
    state_->SetIntArray(this, index, value);
  }
}

template <>
template <>
void internal::ArrayValue<int64_t>::Add(size_t index, int64_t value) {
  if (state_) {
    state_->AddIntArray(this, index, value);
  }
}

template <>
template <>
void internal::ArrayValue<int64_t>::Subtract(size_t index, int64_t value) {
  if (state_) {
    state_->SubtractIntArray(this, index, value);
  }
}

template <>
internal::ArrayValue<uint64_t>::~ArrayValue<uint64_t>() {
  if (state_) {
    state_->FreeUintArray(this);
  }
}

template <>
internal::ArrayValue<uint64_t>& internal::ArrayValue<uint64_t>::operator=(
    internal::ArrayValue<uint64_t>&& other) noexcept {
  if (state_) {
    state_->FreeUintArray(this);
  }
  state_ = std::move(other.state_);
  name_index_ = std::move(other.name_index_);
  value_index_ = std::move(other.value_index_);
  return *this;
}

template <>
void internal::ArrayValue<uint64_t>::Set(size_t index, uint64_t value) {
  if (state_) {
    state_->SetUintArray(this, index, value);
  }
}

template <>
template <>
void internal::ArrayValue<uint64_t>::Add(size_t index, uint64_t value) {
  if (state_) {
    state_->AddUintArray(this, index, value);
  }
}

template <>
template <>
void internal::ArrayValue<uint64_t>::Subtract(size_t index, uint64_t value) {
  if (state_) {
    state_->SubtractUintArray(this, index, value);
  }
}

template <>
internal::ArrayValue<double>::~ArrayValue<double>() {
  if (state_) {
    state_->FreeDoubleArray(this);
  }
}

template <>
internal::ArrayValue<double>& internal::ArrayValue<double>::operator=(
    internal::ArrayValue<double>&& other) noexcept {
  if (state_) {
    state_->FreeDoubleArray(this);
  }
  state_ = std::move(other.state_);
  name_index_ = std::move(other.name_index_);
  value_index_ = std::move(other.value_index_);
  return *this;
}

template <>
void internal::ArrayValue<double>::Set(size_t index, double value) {
  if (state_) {
    state_->SetDoubleArray(this, index, value);
  }
}

template <>
template <>
void internal::ArrayValue<double>::Add(size_t index, double value) {
  if (state_) {
    state_->AddDoubleArray(this, index, value);
  }
}

template <>
template <>
void internal::ArrayValue<double>::Subtract(size_t index, double value) {
  if (state_) {
    state_->SubtractDoubleArray(this, index, value);
  }
}

namespace {
cpp17::optional<zx_koid_t> GetKoidForStringReferenceId() {
  zx::event event;
  zx::event::create(0, &event);
  zx_info_handle_basic_t info;
  zx_status_t status = event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? cpp17::optional<zx_koid_t>(info.koid) : cpp17::nullopt;
}
}  // namespace

StringReference::StringReference(const char* data)
    : data_(data), reference_id_(*GetKoidForStringReferenceId()) {}

cpp17::string_view StringReference::Data() const { return data_; }

uint64_t StringReference::ID() const { return reference_id_; }

#define PROPERTY_METHODS(NAME, TYPE)                                                         \
  template <>                                                                                \
  internal::Property<TYPE>::~Property() {                                                    \
    if (state_) {                                                                            \
      state_->Free##NAME##Property(this);                                                    \
    }                                                                                        \
  }                                                                                          \
                                                                                             \
  template <>                                                                                \
  internal::Property<TYPE>& internal::Property<TYPE>::operator=(Property&& other) noexcept { \
    if (state_) {                                                                            \
      state_->Free##NAME##Property(this);                                                    \
    }                                                                                        \
    state_ = std::move(other.state_);                                                        \
    name_index_ = other.name_index_;                                                         \
    value_index_ = other.value_index_;                                                       \
    return *this;                                                                            \
  }                                                                                          \
                                                                                             \
  template <>                                                                                \
  void internal::Property<TYPE>::Set(const TYPE& value) {                                    \
    if (state_) {                                                                            \
      state_->Set##NAME##Property(this, value);                                              \
    }                                                                                        \
  }

PROPERTY_METHODS(String, std::string)
PROPERTY_METHODS(ByteVector, std::vector<uint8_t>)
PROPERTY_METHODS(Bool, bool)

Node::~Node() {
  if (state_) {
    state_->FreeNode(this);
  }
}

Node& Node::operator=(Node&& other) noexcept {
  if (state_) {
    state_->FreeNode(this);
  }
  state_ = std::move(other.state_);
  name_index_ = std::move(other.name_index_);
  value_index_ = std::move(other.value_index_);
  value_list_ = std::move(other.value_list_);
  return *this;
}

Node Node::CreateChild(BorrowedStringValue name) {
  if (state_) {
    return state_->CreateNode(name, value_index_);
  }
  return Node();
}

void Node::RecordChild(BorrowedStringValue name, RecordChildCallbackFn callback) {
  auto node = CreateChild(name);
  callback(node);
  value_list_.emplace(std::move(node));
}

IntProperty Node::CreateInt(BorrowedStringValue name, int64_t value) {
  if (state_) {
    return state_->CreateIntProperty(name, value_index_, value);
  }
  return IntProperty();
}

void Node::RecordInt(BorrowedStringValue name, int64_t value) {
  value_list_.emplace(CreateInt(name, value));
}

UintProperty Node::CreateUint(BorrowedStringValue name, uint64_t value) {
  if (state_) {
    return state_->CreateUintProperty(name, value_index_, value);
  }
  return UintProperty();
}

void Node::RecordUint(BorrowedStringValue name, uint64_t value) {
  value_list_.emplace(CreateUint(name, value));
}

DoubleProperty Node::CreateDouble(BorrowedStringValue name, double value) {
  if (state_) {
    return state_->CreateDoubleProperty(name, value_index_, value);
  }
  return DoubleProperty();
}

void Node::RecordDouble(BorrowedStringValue name, double value) {
  value_list_.emplace(CreateDouble(name, value));
}

BoolProperty Node::CreateBool(BorrowedStringValue name, bool value) {
  if (state_) {
    return state_->CreateBoolProperty(name, value_index_, value);
  }
  return BoolProperty();
}

void Node::RecordBool(BorrowedStringValue name, bool value) {
  value_list_.emplace(CreateBool(name, value));
}

StringProperty Node::CreateString(BorrowedStringValue name, const std::string& value) {
  if (state_) {
    return state_->CreateStringProperty(name, value_index_, value);
  }
  return StringProperty();
}

void Node::RecordString(BorrowedStringValue name, const std::string& value) {
  value_list_.emplace(CreateString(name, value));
}

ByteVectorProperty Node::CreateByteVector(BorrowedStringValue name,
                                          cpp20::span<const uint8_t> value) {
  if (state_) {
    return state_->CreateByteVectorProperty(name, value_index_, value);
  }
  return ByteVectorProperty();
}

void Node::RecordByteVector(BorrowedStringValue name, cpp20::span<const uint8_t> value) {
  value_list_.emplace(CreateByteVector(name, value));
}

IntArray Node::CreateIntArray(BorrowedStringValue name, size_t slots) {
  if (state_) {
    return state_->CreateIntArray(name, value_index_, slots, ArrayBlockFormat::kDefault);
  }
  return IntArray();
}

UintArray Node::CreateUintArray(BorrowedStringValue name, size_t slots) {
  if (state_) {
    return state_->CreateUintArray(name, value_index_, slots, ArrayBlockFormat::kDefault);
  }
  return UintArray();
}

DoubleArray Node::CreateDoubleArray(BorrowedStringValue name, size_t slots) {
  if (state_) {
    return state_->CreateDoubleArray(name, value_index_, slots, ArrayBlockFormat::kDefault);
  }
  return DoubleArray();
}

StringArray Node::CreateStringArray(BorrowedStringValue name, size_t slots) {
  if (state_) {
    return state_->CreateStringArray(name, value_index_, slots, ArrayBlockFormat::kDefault);
  }

  return StringArray();
}

namespace {
const size_t kExtraSlotsForLinearHistogram = 4;
const size_t kExtraSlotsForExponentialHistogram = 5;
}  // namespace

LinearIntHistogram Node::CreateLinearIntHistogram(BorrowedStringValue name, int64_t floor,
                                                  int64_t step_size, size_t buckets) {
  if (state_) {
    const size_t slots = buckets + kExtraSlotsForLinearHistogram;
    auto array =
        state_->CreateIntArray(name, value_index_, slots, ArrayBlockFormat::kLinearHistogram);
    return LinearIntHistogram(floor, step_size, slots, std::move(array));
  }
  return LinearIntHistogram();
}

LinearUintHistogram Node::CreateLinearUintHistogram(BorrowedStringValue name, uint64_t floor,
                                                    uint64_t step_size, size_t buckets) {
  if (state_) {
    const size_t slots = buckets + kExtraSlotsForLinearHistogram;
    auto array =
        state_->CreateUintArray(name, value_index_, slots, ArrayBlockFormat::kLinearHistogram);
    return LinearUintHistogram(floor, step_size, slots, std::move(array));
  }
  return LinearUintHistogram();
}

LinearDoubleHistogram Node::CreateLinearDoubleHistogram(BorrowedStringValue name, double floor,
                                                        double step_size, size_t buckets) {
  if (state_) {
    const size_t slots = buckets + kExtraSlotsForLinearHistogram;
    auto array =
        state_->CreateDoubleArray(name, value_index_, slots, ArrayBlockFormat::kLinearHistogram);
    return LinearDoubleHistogram(floor, step_size, slots, std::move(array));
  }
  return LinearDoubleHistogram();
}

ExponentialIntHistogram Node::CreateExponentialIntHistogram(BorrowedStringValue name, int64_t floor,
                                                            int64_t initial_step,
                                                            int64_t step_multiplier,
                                                            size_t buckets) {
  if (state_) {
    const size_t slots = buckets + kExtraSlotsForExponentialHistogram;
    auto array =
        state_->CreateIntArray(name, value_index_, slots, ArrayBlockFormat::kExponentialHistogram);
    return ExponentialIntHistogram(floor, initial_step, step_multiplier, slots, std::move(array));
  }
  return ExponentialIntHistogram();
}

ExponentialUintHistogram Node::CreateExponentialUintHistogram(BorrowedStringValue name,
                                                              uint64_t floor, uint64_t initial_step,
                                                              uint64_t step_multiplier,
                                                              size_t buckets) {
  if (state_) {
    const size_t slots = buckets + kExtraSlotsForExponentialHistogram;
    auto array =
        state_->CreateUintArray(name, value_index_, slots, ArrayBlockFormat::kExponentialHistogram);
    return ExponentialUintHistogram(floor, initial_step, step_multiplier, slots, std::move(array));
  }
  return ExponentialUintHistogram();
}

ExponentialDoubleHistogram Node::CreateExponentialDoubleHistogram(BorrowedStringValue name,
                                                                  double floor, double initial_step,
                                                                  double step_multiplier,
                                                                  size_t buckets) {
  if (state_) {
    const size_t slots = buckets + kExtraSlotsForExponentialHistogram;
    auto array = state_->CreateDoubleArray(name, value_index_, slots,
                                           ArrayBlockFormat::kExponentialHistogram);
    return ExponentialDoubleHistogram(floor, initial_step, step_multiplier, slots,
                                      std::move(array));
  }
  return ExponentialDoubleHistogram();
}

std::string Node::UniqueName(const std::string& prefix) {
  if (state_) {
    return state_->UniqueName(prefix);
  }
  return "";
}

LazyNode Node::CreateLazyNode(BorrowedStringValue name, LazyNodeCallbackFn callback) {
  if (state_) {
    return state_->CreateLazyNode(name, value_index_, std::move(callback));
  }
  return LazyNode();
}

void Node::RecordLazyNode(BorrowedStringValue name, LazyNodeCallbackFn callback) {
  auto node = CreateLazyNode(name, std::move(callback));
  value_list_.emplace(std::move(node));
}

LazyNode Node::CreateLazyValues(BorrowedStringValue name, LazyNodeCallbackFn callback) {
  if (state_) {
    return state_->CreateLazyValues(name, value_index_, std::move(callback));
  }
  return LazyNode();
}

void Node::RecordLazyValues(BorrowedStringValue name, LazyNodeCallbackFn callback) {
  auto node = CreateLazyValues(name, std::move(callback));
  value_list_.emplace(std::move(node));
}

void Node::AtomicUpdate(AtomicUpdateCallbackFn callback) {
  if (state_) {
    state_->BeginTransaction();
    callback(*this);
    state_->EndTransaction();
  }
}

Link& Link::operator=(Link&& other) noexcept {
  if (this == &other) {
    return *this;
  }

  DeallocateFromVmo();

  state_ = std::move(other.state_);
  name_index_ = other.name_index_;
  value_index_ = other.value_index_;
  content_index_ = other.content_index_;

  return *this;
}

void Link::DeallocateFromVmo() {
  if (state_ == nullptr) {
    return;
  }

  state_->FreeLink(this);
  state_ = nullptr;
}

Link::~Link() { DeallocateFromVmo(); }

LazyNode& LazyNode::operator=(LazyNode&& other) noexcept {
  if (this == &other) {
    return *this;
  }

  DeallocateFromVmo();

  state_ = std::move(other.state_);
  content_value_ = std::move(other.content_value_);
  link_ = std::move(other.link_);

  return *this;
}

void LazyNode::DeallocateFromVmo() {
  if (state_ == nullptr) {
    return;
  }

  state_->FreeLazyNode(this);
  state_ = nullptr;
}

LazyNode::~LazyNode() { DeallocateFromVmo(); }

}  // namespace inspect
