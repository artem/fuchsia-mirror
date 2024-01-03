// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-remote-process-tests.h"

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/abi.h>
#include <lib/ld/remote-load-module.h>
#include <lib/ld/testing/test-vmo.h>
#include <lib/zx/job.h>
#include <zircon/process.h>

#include <string_view>

namespace ld::testing {

constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;
constexpr auto kLinkerName = ld::abi::Abi<>::kSoname;
class LdRemoteProcessTests::MockLoader {
 public:
  MOCK_METHOD(zx::vmo, LoadObject, (std::string));

  void ExpectLoadObject(std::string_view name) {
    const std::string path = std::filesystem::path("test") / "lib" / kLibprefix / name;
    EXPECT_CALL(*this, LoadObject(std::string{name}))
        .WillOnce(::testing::Return(elfldltl::testing::GetTestLibVmo(path)));
  }

 private:
  ::testing::InSequence sequence_guard_;
};

LdRemoteProcessTests::LdRemoteProcessTests() = default;

LdRemoteProcessTests::~LdRemoteProcessTests() = default;

void LdRemoteProcessTests::Init(std::initializer_list<std::string_view> args,
                                std::initializer_list<std::string_view> env) {
  mock_loader_ = std::make_unique<MockLoader>();

  std::string_view name = process_name();
  zx::process process;
  ASSERT_EQ(zx::process::create(*zx::job::default_job(), name.data(),
                                static_cast<uint32_t>(name.size()), 0, &process, &root_vmar_),
            ZX_OK);
  set_process(std::move(process));

  // Initialize a log to pass ExpectLog statements in load-tests.cc.
  fbl::unique_fd log_fd;
  ASSERT_NO_FATAL_FAILURE(InitLog(log_fd));

  ASSERT_EQ(zx::thread::create(this->process(), name.data(), static_cast<uint32_t>(name.size()), 0,
                               &thread_),
            ZX_OK);
}

// Set the expectations that these dependencies will be loaded in the given order.
void LdRemoteProcessTests::Needed(std::initializer_list<std::string_view> names) {
  for (std::string_view name : names) {
    // The linker and vdso should not be included in any `Needed` list for a
    // test, because load requests for them bypass the loader.
    ASSERT_TRUE(name != kLinkerName.str() && name != GetVdsoSoname().str())
        << std::string{name} + " should not be included in Needed list.";
    mock_loader_->ExpectLoadObject(name);
  }
}

void LdRemoteProcessTests::Load(std::string_view executable_name) {
  using RemoteModule = RemoteLoadModule<>;

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  const std::string executable_path = std::filesystem::path("test") / "bin" / executable_name;

  zx::vmo vmo = elfldltl::testing::GetTestLibVmo(executable_path);

  zx::vmo vdso_vmo;
  zx_status_t status = GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
  EXPECT_EQ(status, ZX_OK) << zx_status_get_string(status);

  zx::vmo stub_ld_vmo;
  stub_ld_vmo = elfldltl::testing::GetTestLibVmo("ld-stub.so");

  auto get_dep_vmo = [this, vdso_name = GetVdsoSoname(), &vdso_vmo,
                      &stub_ld_vmo](const elfldltl::Soname<>& soname) -> zx::vmo {
    // Executables may depend on the stub linker and vdso implicitly, so return
    // pre-fetched VMOs, asserting that these deps are requested once at most.
    if (soname == vdso_name) {
      EXPECT_TRUE(vdso_vmo) << vdso_name << " for passive ABI should not be looked up twice.";
      return std::exchange(vdso_vmo, {});
    }
    if (soname == kLinkerName) {
      EXPECT_TRUE(stub_ld_vmo) << kLinkerName << " for passive ABI should not be looked up twice.";
      return std::exchange(stub_ld_vmo, {});
    }
    return mock_loader_->LoadObject(std::string{soname.str()});
  };

  auto decode_result = RemoteModule::DecodeModules(diag, std::move(vmo), get_dep_vmo);
  EXPECT_TRUE(decode_result);
  set_stack_size(decode_result->main_exec.stack_size);

  auto& modules = decode_result->modules;
  ASSERT_FALSE(modules.is_empty());
  EXPECT_TRUE(RemoteModule::AllocateModules(diag, modules, root_vmar().borrow()));
  EXPECT_TRUE(RemoteModule::RelocateModules(diag, modules));
  EXPECT_TRUE(RemoteModule::LoadModules(diag, modules));
  RemoteModule::CommitModules(modules);

  // The executable will always be the first module, retrieve it to set the
  // loaded entry point.
  set_entry(decode_result->main_exec.relative_entry + modules.front().load_bias());

  // Locate the loaded VDSO to set the vdso base pointer for the test.
  auto loaded_vdso = std::find(modules.begin(), modules.end(), GetVdsoSoname());
  ASSERT_TRUE(loaded_vdso != modules.end());
  set_vdso_base(loaded_vdso->module().vaddr_start());
}

int64_t LdRemoteProcessTests::Run() {
  return LdLoadZirconProcessTestsBase::Run(nullptr, stack_size_, thread_, entry_, vdso_base_,
                                           root_vmar());
}

}  // namespace ld::testing
