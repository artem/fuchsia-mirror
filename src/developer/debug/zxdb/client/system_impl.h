// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef GARNET_BIN_ZXDB_CLIENT_SYSTEM_IMPL_H_
#define GARNET_BIN_ZXDB_CLIENT_SYSTEM_IMPL_H_

#include <map>
#include <memory>
#include <vector>

#include "src/developer/debug/zxdb/client/system.h"
#include "src/developer/debug/zxdb/symbols/system_symbols.h"
#include "src/lib/fxl/macros.h"
#include "src/lib/fxl/memory/weak_ptr.h"

namespace zxdb {

class BreakpointImpl;
class ProcessImpl;
class SystemSymbolsProxy;
class TargetImpl;
class JobContextImpl;

class SystemImpl final : public System,
                         public SettingStoreObserver,
                         public SystemSymbols::DownloadHandler {
 public:
  explicit SystemImpl(Session* session);
  ~SystemImpl() override;

  ProcessImpl* ProcessImplFromKoid(uint64_t koid) const;

  // Broadcasts the global process notifications.
  void NotifyDidCreateProcess(Process* process);
  void NotifyWillDestroyProcess(Process* process);

  std::vector<TargetImpl*> GetTargetImpls() const;

  // Like CreateNewTarget byt returns the implementation.
  TargetImpl* CreateNewTargetImpl(TargetImpl* clone);

  // System implementation:
  SystemSymbols* GetSymbols() override;
  std::vector<Target*> GetTargets() const override;
  std::vector<JobContext*> GetJobContexts() const override;
  std::vector<Breakpoint*> GetBreakpoints() const override;
  std::vector<SymbolServer*> GetSymbolServers() const override;
  Process* ProcessFromKoid(uint64_t koid) const override;
  void GetProcessTree(ProcessTreeCallback callback) override;
  Target* CreateNewTarget(Target* clone) override;
  JobContext* CreateNewJobContext(JobContext* clone) override;
  Breakpoint* CreateNewBreakpoint() override;
  Breakpoint* CreateNewInternalBreakpoint() override;
  void DeleteBreakpoint(Breakpoint* breakpoint) override;
  void Pause() override;
  void Continue() override;

  // DownloadHandler implementation:
  void RequestDownload(const std::string& build_id) override;

  // Notification that a connection has been made/terminated to a target
  // system.
  void DidConnect();
  void DidDisconnect();

  // Returns the breakpoint implementation for the given ID, or null if the
  // ID was not found in the map. This will include both internal and regular
  // breakpoints (it is used for notification dispatch).
  BreakpointImpl* BreakpointImplForId(uint32_t id);

  // SettingStoreObserver implementation.
  void OnSettingChanged(const SettingStore&,
                        const std::string& setting_name) override;

 private:
  void AddNewTarget(std::unique_ptr<TargetImpl> target);
  void AddNewJobContext(std::unique_ptr<JobContextImpl> job_context);

  // Called when we have attempted to download debug symbols and failed. If err
  // is set then something went wrong during the attempt, otherwise the symbols
  // simply weren't available from any of the servers.
  void NotifyFailedToFindDebugSymbols(const Err& err,
                                      const std::string& build_id);

  std::vector<std::unique_ptr<SymbolServer>> symbol_servers_;
  std::vector<std::unique_ptr<TargetImpl>> targets_;
  std::vector<std::unique_ptr<JobContextImpl>> job_contexts_;

  // The breakpoints are indexed by their unique backend ID. This is separate
  // from the index generated by the console frontend to describe the
  // breakpoint noun.
  std::map<uint32_t, std::unique_ptr<BreakpointImpl>> breakpoints_;

  SystemSymbols symbols_;

  fxl::WeakPtrFactory<SystemImpl> weak_factory_;

  FXL_DISALLOW_COPY_AND_ASSIGN(SystemImpl);
};

}  // namespace zxdb

#endif  // GARNET_BIN_ZXDB_CLIENT_SYSTEM_IMPL_H_
