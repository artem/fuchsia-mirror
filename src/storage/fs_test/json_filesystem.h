// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_FS_TEST_JSON_FILESYSTEM_H_
#define SRC_STORAGE_FS_TEST_JSON_FILESYSTEM_H_

#include <rapidjson/document.h>

#include "src/storage/fs_test/fs_test.h"

namespace fs_test {

// Helper function to return a member with a default if not present.
template <typename T>
T ConfigGetOrDefault(const rapidjson::Value& value, const char* member, T default_value) {
  auto iter = value.FindMember(member);
  return iter == value.MemberEnd() ? default_value : iter->value.Get<T>();
}

// Represents a filesystem that is configured using json.  See fs_test.schema.json for the schema.
class JsonFilesystem : public FilesystemImplWithDefaultMake<JsonFilesystem> {
 public:
  static zx::result<std::unique_ptr<JsonFilesystem>> NewFilesystem(
      const rapidjson::Document& config);

  JsonFilesystem(Traits traits, fs_management::DiskFormat format, uint16_t sectors_per_cluster)
      : traits_(std::move(traits)), format_(format), sectors_per_cluster_(sectors_per_cluster) {}
  virtual ~JsonFilesystem() = default;

  fs_management::DiskFormat format() const { return format_; }
  uint16_t sectors_per_cluster() const { return sectors_per_cluster_; }

  const Traits& GetTraits() const override { return traits_; }

  std::unique_ptr<FilesystemInstance> Create(RamDevice device,
                                             std::string device_path) const override;

  zx::result<std::unique_ptr<FilesystemInstance>> Open(
      const TestFilesystemOptions& options) const override;

 private:
  const Traits traits_;
  const fs_management::DiskFormat format_;
  const uint16_t sectors_per_cluster_ = 0;
};

}  // namespace fs_test

#endif  // SRC_STORAGE_FS_TEST_JSON_FILESYSTEM_H_
