// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "apps/modular/src/story_runner/story_storage_impl.h"

#include "apps/ledger/services/public/ledger.fidl.h"
#include "apps/modular/lib/fidl/array_to_string.h"
#include "apps/modular/lib/fidl/json_xdr.h"
#include "apps/modular/lib/fidl/operation.h"
#include "apps/modular/lib/ledger/storage.h"
#include "apps/modular/services/story/story_data.fidl.h"
#include "lib/mtl/vmo/strings.h"

namespace modular {

namespace {

void XdrLinkPath(XdrContext* const xdr, LinkPath* const data) {
  xdr->Field("module_path", &data->module_path);
  xdr->Field("link_name", &data->link_name);
}

void XdrModuleData(XdrContext* const xdr, ModuleData* const data) {
  xdr->Field("url", &data->url);
  xdr->Field("module_path", &data->module_path);
  xdr->Field("default_link_path", &data->default_link_path, XdrLinkPath);
  xdr->Field("module_source", &data->module_source);
}

void XdrPerDeviceStoryInfo(XdrContext* const xdr,
                           PerDeviceStoryInfo* const info) {
  xdr->Field("device", &info->device_id);
  xdr->Field("id", &info->story_id);
  xdr->Field("time", &info->timestamp);
  xdr->Field("state", &info->state);
}

void XdrStoryContextLog(XdrContext* const xdr, StoryContextLog* const data) {
  xdr->Field("context", &data->context);
  xdr->Field("device_id", &data->device_id);
  xdr->Field("time", &data->time);
  xdr->Field("signal", &data->signal);
}

}  // namespace

class StoryStorageImpl::ReadLinkDataCall : Operation<fidl::String> {
 public:
  ReadLinkDataCall(OperationContainer* const container,
                   ledger::Page* const page,
                   const LinkPathPtr& link_path,
                   ResultCall result_call)
      : Operation(container, std::move(result_call)),
        page_(page),
        link_key_(MakeLinkKey(link_path)) {
    Ready();
  }

 private:
  void Run() override {
    FlowToken flow{this, &result_};

    page_->GetSnapshot(page_snapshot_.NewRequest(), nullptr, nullptr,
                       [this, flow](ledger::Status status) {
                         if (status != ledger::Status::OK) {
                           FTL_LOG(ERROR) << "ReadLinkDataCall() " << link_key_
                                          << " Page.GetSnapshot() " << status;
                           return;
                         }

                         Cont(flow);
                       });
  }

  void Cont(FlowToken flow) {
    page_snapshot_->Get(to_array(link_key_), [this, flow](ledger::Status status,
                                                          mx::vmo value) {
      if (status != ledger::Status::OK) {
        if (status != ledger::Status::KEY_NOT_FOUND) {
          // It's expected that the key is not found when the link is
          // accessed for the first time. Don't log an error then.
          FTL_LOG(ERROR) << "ReadLinkDataCall() " << link_key_
                         << " PageSnapshot.Get() " << status;
        }
        return;
      }

      std::string value_as_string;
      if (value) {
        if (!mtl::StringFromVmo(value, &value_as_string)) {
          FTL_LOG(ERROR) << "ReadLinkDataCall() " << link_key_
                         << " Unable to extract data.";
          return;
        }
      }

      result_.Swap(&value_as_string);
    });
  }

  ledger::Page* const page_;  // not owned
  ledger::PageSnapshotPtr page_snapshot_;
  const std::string link_key_;
  fidl::String result_;

  FTL_DISALLOW_COPY_AND_ASSIGN(ReadLinkDataCall);
};

class StoryStorageImpl::WriteLinkDataCall : Operation<> {
 public:
  WriteLinkDataCall(OperationContainer* const container,
                    ledger::Page* const page,
                    const LinkPathPtr& link_path,
                    fidl::String data,
                    ResultCall result_call)
      : Operation(container, std::move(result_call)),
        page_(page),
        link_key_(MakeLinkKey(link_path)),
        data_(std::move(data)) {
    Ready();
  }

 private:
  void Run() override {
    FlowToken flow{this};

    page_->Put(to_array(link_key_), to_array(data_),
               [this, flow](ledger::Status status) {
                 if (status != ledger::Status::OK) {
                   FTL_LOG(ERROR)
                       << "WriteLinkDataCall() link key =" << link_key_
                       << ", Page.Put() " << status;
                 }
               });
  }

  ledger::Page* const page_;  // not owned
  const std::string link_key_;
  fidl::String data_;

  FTL_DISALLOW_COPY_AND_ASSIGN(WriteLinkDataCall);
};

template <typename Data,
          typename DataPtr = fidl::StructPtr<Data>,
          typename DataFilter = XdrFilterType<Data>>
class StoryStorageImpl::ReadDataCall : Operation<DataPtr> {
 public:
  using ResultCall = std::function<void(DataPtr)>;
  using FlowToken = typename Operation<DataPtr>::FlowToken;

  ReadDataCall(OperationContainer* const container,
               ledger::Page* const page,
               const std::string& key,
               DataFilter filter,
               ResultCall result_call)
      : Operation<DataPtr>(container, std::move(result_call)),
        page_(page),
        key_(key),
        filter_(filter) {
    this->Ready();
  }

 private:
  void Run() override {
    FlowToken flow{this, &result_};

    page_->GetSnapshot(page_snapshot_.NewRequest(), nullptr, nullptr,
                       [this, flow](ledger::Status status) {
                         if (status != ledger::Status::OK) {
                           FTL_LOG(ERROR) << "ReadDataCall() " << key_
                                          << " Page.GetSnapshot() " << status;
                           return;
                         }

                         Cont(flow);
                       });
  }

  void Cont(FlowToken flow) {
    page_snapshot_->Get(
        to_array(key_),
        [this, flow](ledger::Status status, mx::vmo value) {
          if (status != ledger::Status::OK) {
            FTL_LOG(ERROR) << "ReadDataCall() " << key_
                           << " PageSnapshot.Get() " << status;
            return;
          }

          if (!value) {
            FTL_LOG(ERROR) << "ReadDataCall() " << key_
                           << " PageSnapshot.Get() null vmo";
          }

          std::string value_as_string;
          if (!mtl::StringFromVmo(value, &value_as_string)) {
            FTL_LOG(ERROR) << "ReadDataCall() " << key_
                           << " Unable to extract data.";
            return;
          }

          if (!XdrRead(value_as_string, &result_, filter_)) {
            result_.reset();
            return;
          }

          FTL_DCHECK(!result_.is_null());
        });
  }

  ledger::Page* const page_;  // not owned
  const std::string key_;
  DataFilter const filter_;
  ledger::PageSnapshotPtr page_snapshot_;
  DataPtr result_;

  FTL_DISALLOW_COPY_AND_ASSIGN(ReadDataCall);
};

template <typename Data,
          typename DataPtr = fidl::StructPtr<Data>,
          typename DataArray = fidl::Array<DataPtr>,
          typename DataFilter = XdrFilterType<Data>>
class StoryStorageImpl::ReadAllDataCall : Operation<DataArray> {
 public:
  using ResultCall = std::function<void(DataArray)>;
  using FlowToken = typename Operation<DataArray>::FlowToken;

  ReadAllDataCall(OperationContainer* const container,
                  ledger::Page* const page,
                  const char* const prefix,
                  DataFilter const filter,
                  ResultCall result_call)
      : Operation<DataArray>(container, std::move(result_call)),
        page_(page),
        prefix_(prefix),
        filter_(filter) {
    data_.resize(0);
    this->Ready();
  }

 private:
  void Run() override {
    FlowToken flow{this, &data_};

    page_->GetSnapshot(page_snapshot_.NewRequest(), to_array(prefix_),
                       nullptr, [this, flow](ledger::Status status) {
                         if (status != ledger::Status::OK) {
                           FTL_LOG(ERROR) << "ReadAllDataCall() "
                                          << "Page.GetSnapshot() " << status;
                           return;
                         }

                         Cont1(flow);
                       });
  }

  void Cont1(FlowToken flow) {
    GetEntries(page_snapshot_.get(), &entries_,
               [this, flow](ledger::Status status) {
                 if (status != ledger::Status::OK) {
                   FTL_LOG(ERROR) << "ReadAllDataCall() "
                                  << "GetEntries() " << status;
                   return;
                 }

                 Cont2(flow);
               });
  }

  void Cont2(FlowToken flow) {
    for (auto& entry : entries_) {
      std::string value_as_string;
      if (!mtl::StringFromVmo(entry->value, &value_as_string)) {
        FTL_LOG(ERROR) << "ReadAllDataCall() "
                       << "Unable to extract data.";
        continue;
      }

      DataPtr data;
      if (!XdrRead(value_as_string, &data, filter_)) {
        continue;
      }

      FTL_DCHECK(!data.is_null());

      data_.push_back(std::move(data));
    }
  }

  ledger::Page* page_;  // not owned
  ledger::PageSnapshotPtr page_snapshot_;
  const char* const prefix_;
  DataFilter const filter_;
  std::vector<ledger::EntryPtr> entries_;
  DataArray data_;

  FTL_DISALLOW_COPY_AND_ASSIGN(ReadAllDataCall);
};

template <typename Data,
          typename DataPtr = fidl::StructPtr<Data>,
          typename DataFilter = XdrFilterType<Data>>
class StoryStorageImpl::WriteDataCall : Operation<> {
 public:
  WriteDataCall(OperationContainer* const container,
                ledger::Page* const page,
                const std::string& key,
                DataFilter filter,
                DataPtr data,
                ResultCall result_call)
      : Operation(container, std::move(result_call)),
        page_(page),
        key_(key),
        filter_(filter),
        data_(std::move(data)) {
    Ready();
  }

 private:
  void Run() override {
    FlowToken flow{this};

    std::string json;
    XdrWrite(&json, &data_, filter_);

    page_->Put(to_array(key_), to_array(json),
               [this, flow](ledger::Status status) {
                 if (status != ledger::Status::OK) {
                   FTL_LOG(ERROR)
                       << "WriteDataCall() key =" << key_
                       << ", Page.Put() " << status;
                 }
               });
  }

  ledger::Page* const page_;  // not owned
  const std::string key_;
  DataFilter const filter_;
  DataPtr data_;

  FTL_DISALLOW_COPY_AND_ASSIGN(WriteDataCall);
};

StoryStorageImpl::StoryStorageImpl(ledger::Page* const story_page)
    : PageClient("StoryStorageImpl", story_page, kLinkKeyPrefix),
      story_page_(story_page) {}

StoryStorageImpl::~StoryStorageImpl() = default;

void StoryStorageImpl::ReadLinkData(const LinkPathPtr& link_path,
                                    const DataCallback& callback) {
  new ReadLinkDataCall(&operation_queue_, story_page_, link_path, callback);
}

void StoryStorageImpl::WriteLinkData(const LinkPathPtr& link_path,
                                     const fidl::String& data,
                                     const SyncCallback& callback) {
  new WriteLinkDataCall(&operation_queue_, story_page_, link_path, data,
                        callback);
}

void StoryStorageImpl::ReadModuleData(
    const fidl::Array<fidl::String>& module_path,
    const ModuleDataCallback& callback) {
  new ReadDataCall<ModuleData>(
      &operation_queue_, story_page_,
      MakeModuleKey(module_path), XdrModuleData, callback);
}

void StoryStorageImpl::ReadAllModuleData(
    const AllModuleDataCallback& callback) {
  new ReadAllDataCall<ModuleData>(
      &operation_queue_, story_page_, kModuleKeyPrefix, XdrModuleData,
      callback);
}

void StoryStorageImpl::WriteModuleData(
    const fidl::Array<fidl::String>& module_path,
    const fidl::String& module_url,
    const LinkPathPtr& link_path,
    ModuleSource module_source,
    const SyncCallback& callback) {
  ModuleDataPtr data = ModuleData::New();
  data->url = module_url;
  data->module_path = module_path.Clone();
  data->default_link_path = link_path.Clone();
  data->module_source = module_source;

  new WriteDataCall<ModuleData>(
      &operation_queue_, story_page_, MakeModuleKey(module_path),
      XdrModuleData, std::move(data), callback);
}

void StoryStorageImpl::WriteDeviceData(const std::string& story_id,
                                       const std::string& device_id,
                                       StoryState state,
                                       const SyncCallback& callback) {
  PerDeviceStoryInfoPtr data = PerDeviceStoryInfo::New();
  data->device_id = device_id;
  data->story_id = story_id;
  data->timestamp = time(nullptr);
  data->state = state;

  new WriteDataCall<PerDeviceStoryInfo, PerDeviceStoryInfoPtr>(
      &operation_queue_, story_page_, MakePerDeviceKey(device_id),
      XdrPerDeviceStoryInfo, std::move(data),
      callback);
}

void StoryStorageImpl::Log(StoryContextLogPtr log_entry) {
  new WriteDataCall<StoryContextLog>(
      &operation_queue_, story_page_,
      MakeStoryContextLogKey(log_entry->signal, log_entry->time),
      XdrStoryContextLog, std::move(log_entry),
      []{});
}

void StoryStorageImpl::ReadLog(const LogCallback& callback) {
  new ReadAllDataCall<StoryContextLog>(
      &operation_queue_, story_page_, kStoryContextLogKeyPrefix, XdrStoryContextLog,
      callback);
}

void StoryStorageImpl::Sync(const SyncCallback& callback) {
  new SyncCall(&operation_queue_, callback);
}

void StoryStorageImpl::WatchLink(const LinkPathPtr& link_path,
                                 LinkImpl* const impl,
                                 const DataCallback& watcher) {
  watchers_.emplace_back(WatcherEntry{MakeLinkKey(link_path), impl, watcher});
}

void StoryStorageImpl::DropWatcher(LinkImpl* const impl) {
  auto f = std::find_if(watchers_.begin(), watchers_.end(),
                        [impl](auto& entry) { return entry.impl == impl; });
  FTL_DCHECK(f != watchers_.end());
  watchers_.erase(f);
}

void StoryStorageImpl::OnChange(const std::string& key,
                                const std::string& value) {
  for (auto& watcher_entry : watchers_) {
    if (key == watcher_entry.key) {
      watcher_entry.watcher(value);
    }
  }
}

}  // namespace modular
