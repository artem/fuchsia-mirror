// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef APPS_LEDGER_BENCHMARK_CONVERGENCE_CONVERGENCE_H_
#define APPS_LEDGER_BENCHMARK_CONVERGENCE_CONVERGENCE_H_

#include <memory>
#include <unordered_set>

#include "application/lib/app/application_context.h"
#include "apps/ledger/services/public/ledger.fidl.h"
#include "apps/ledger/src/test/data_generator.h"
#include "apps/ledger/src/test/fake_token_provider.h"
#include "lib/ftl/files/scoped_temp_dir.h"

namespace benchmark {

// Benchmark that measures the time it takes to sync and reconcile concurrent
// writes.
//
// In this scenario there is two devices. At each step, both devices make a
// concurrent write, and we measure the time until both changes are visible to
// both devices.
//
// Parameters:
//   --entry-count=<int> the number of entries to be put by each device
//   --value-size=<int> the size of a single value in bytes
//   --server-id=<string> the ID of the Firebase instance ot use for syncing
class ConvergenceBenchmark : public ledger::PageWatcher {
 public:
  ConvergenceBenchmark(int entry_count, int value_size, std::string server_id);

  void Run();

  // ledger::PageWatcher:
  void OnChange(ledger::PageChangePtr page_change,
                ledger::ResultState result_state,
                const OnChangeCallback& callback) override;

 private:
  void Start(int step);

  void ShutDown();

  test::DataGenerator generator_;
  std::unique_ptr<app::ApplicationContext> application_context_;
  test::FakeTokenProvider token_provider_impl_;
  const int entry_count_;
  const int value_size_;
  std::string server_id_;
  fidl::Binding<ledger::PageWatcher> alpha_watcher_binding_;
  fidl::Binding<ledger::PageWatcher> beta_watcher_binding_;
  files::ScopedTempDir alpha_tmp_dir_;
  files::ScopedTempDir beta_tmp_dir_;
  app::ApplicationControllerPtr alpha_controller_;
  app::ApplicationControllerPtr beta_controller_;
  ledger::LedgerPtr alpha_ledger_;
  ledger::LedgerPtr beta_ledger_;
  fidl::Array<uint8_t> page_id_;
  ledger::PagePtr alpha_page_;
  ledger::PagePtr beta_page_;
  std::unordered_multiset<std::string> remaining_keys_;
  int current_step_ = -1;

  FTL_DISALLOW_COPY_AND_ASSIGN(ConvergenceBenchmark);
};

}  // namespace benchmark

#endif  // APPS_LEDGER_BENCHMARK_CONVERGENCE_CONVERGENCE_H_
