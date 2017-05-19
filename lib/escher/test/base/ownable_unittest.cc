// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "escher/base/ownable.h"
#include "escher/base/owner.h"
#include "escher/base/type_info.h"

#include "ftl/memory/ref_ptr.h"
#include "gtest/gtest.h"

namespace {

enum class OwnableTypes {
  kOwnable1 = 1,
  kOwnable2 = 1 << 1,
  kSubOwnable1 = 1 << 2,
  kSubOwnable2 = 1 << 3,
};

typedef escher::TypeInfo<OwnableTypes> OwnableTypeInfo;

class OwnableBase : public escher::Ownable<OwnableTypeInfo> {
 public:
  static const TypeInfo kTypeInfo;
};

class Ownable1 : public OwnableBase {
 public:
  static const TypeInfo kTypeInfo;
  const TypeInfo& type_info() const override { return Ownable1::kTypeInfo; }

  explicit Ownable1(size_t& destroyed_count)
      : destroyed_count_(destroyed_count) {}

  ~Ownable1() override { ++destroyed_count_; }

 private:
  size_t& destroyed_count_;
};

class Ownable2 : public OwnableBase {
 public:
  static const TypeInfo kTypeInfo;
  const TypeInfo& type_info() const override { return Ownable2::kTypeInfo; }
};

class SubOwnable1 : public Ownable1 {
 public:
  static const TypeInfo kTypeInfo;
  const TypeInfo& type_info() const override { return SubOwnable1::kTypeInfo; }

  explicit SubOwnable1(size_t& destroyed_count) : Ownable1(destroyed_count) {}
};

class SubOwnable2 : public Ownable2 {
 public:
  static const TypeInfo kTypeInfo;
  const TypeInfo& type_info() const override { return SubOwnable2::kTypeInfo; }
};

const OwnableTypeInfo OwnableBase::kTypeInfo("OwnableBase");
const OwnableTypeInfo Ownable1::kTypeInfo("Ownable1", OwnableTypes::kOwnable1);
const OwnableTypeInfo Ownable2::kTypeInfo("Ownable2", OwnableTypes::kOwnable2);
const OwnableTypeInfo SubOwnable1::kTypeInfo("SubOwnable1",
                                             OwnableTypes::kOwnable1,
                                             OwnableTypes::kSubOwnable1);
const OwnableTypeInfo SubOwnable2::kTypeInfo("SubOwnable2",
                                             OwnableTypes::kOwnable2,
                                             OwnableTypes::kSubOwnable2);

typedef escher::Owner<OwnableTypeInfo> OwnerBase;

class TestOwner : public OwnerBase {
 public:
  explicit TestOwner(size_t& destroyed_count)
      : destroyed_count_(destroyed_count) {}

  ftl::RefPtr<Ownable1> NewOwnable1() {
    auto result = escher::Make<Ownable1>(destroyed_count_);
    BecomeOwnerOf(result.get());
    return result;
  }

  void OnReceiveOwnable(std::unique_ptr<OwnableType> unreffed) override {
    unreffed_.push_back(ftl::RefPtr<OwnableType>(unreffed.release()));
  }

  size_t GetUnreffedCount() const { return unreffed_.size(); }
  void ClearUnreffed() {
    for (auto& owned : unreffed_) {
      // So that they are destroyed.
      RelinquishOwnershipOf(owned.get());
    }
    unreffed_.clear();
  }

 private:
  size_t& destroyed_count_;
  std::vector<ftl::RefPtr<OwnableType>> unreffed_;
};

TEST(Ownable, ReceiveOwnables) {
  size_t destroyed_count = 0;
  auto owner = escher::Make<TestOwner>(destroyed_count);
  EXPECT_EQ(0U, owner->ownable_count());

  auto ownable1 = owner->NewOwnable1();
  auto ownable2 = owner->NewOwnable1();
  EXPECT_EQ(ownable1->owner(), owner.get());
  EXPECT_EQ(ownable2->owner(), owner.get());
  EXPECT_EQ(2U, owner->ownable_count());
  EXPECT_EQ(0U, owner->GetUnreffedCount());
  EXPECT_EQ(0U, destroyed_count);
  EXPECT_NE(ownable1.get(), ownable2.get());

  ownable1 = ownable2;
  EXPECT_EQ(ownable1.get(), ownable2.get());
  EXPECT_EQ(2U, ownable1->ref_count());
  EXPECT_EQ(2U, owner->ownable_count());
  EXPECT_EQ(1U, owner->GetUnreffedCount());
  EXPECT_EQ(0U, destroyed_count);

  owner->ClearUnreffed();
  EXPECT_EQ(1U, owner->ownable_count());
  EXPECT_EQ(0U, owner->GetUnreffedCount());
  EXPECT_EQ(1U, destroyed_count);

  ownable2 = nullptr;
  EXPECT_EQ(1U, owner->ownable_count());
  EXPECT_EQ(0U, owner->GetUnreffedCount());
  EXPECT_EQ(1U, destroyed_count);

  ownable1 = nullptr;
  EXPECT_EQ(1U, owner->ownable_count());
  EXPECT_EQ(1U, owner->GetUnreffedCount());
  EXPECT_EQ(1U, destroyed_count);

  owner->ClearUnreffed();
  EXPECT_EQ(0U, owner->ownable_count());
  EXPECT_EQ(0U, owner->GetUnreffedCount());
  EXPECT_EQ(2U, destroyed_count);
}

}  // namespace
