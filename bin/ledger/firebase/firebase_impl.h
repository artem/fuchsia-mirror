// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef APPS_LEDGER_SRC_FIREBASE_FIREBASE_IMPL_H_
#define APPS_LEDGER_SRC_FIREBASE_FIREBASE_IMPL_H_

#include <functional>
#include <memory>
#include <string>
#include <vector>

#include "apps/ledger/src/firebase/event_stream.h"
#include "apps/ledger/src/firebase/firebase.h"
#include "apps/ledger/src/firebase/status.h"
#include "apps/ledger/src/firebase/watch_client.h"
#include "apps/network/services/network_service.fidl.h"
#include "apps/network/services/url_loader.fidl.h"

#include <rapidjson/document.h>

namespace firebase {

class FirebaseImpl : public Firebase {
 public:
  // |db_id| is the identifier of the Firebase Realtime Database instance. E.g.,
  // if the database is hosted at https://example.firebaseio.com/, its
  // identifier is "example".
  //
  // |prefix| is a url prefix against which all requests will be made, without a
  // leading or trailing slash. (possible with slashes inside) If empty,
  // requests will be made against root of the database.
  FirebaseImpl(network::NetworkServicePtr network_service,
               const std::string& db_id,
               const std::string& prefix);
  ~FirebaseImpl() override;

  // Firebase:
  void Get(
      const std::string& key,
      const std::string& query,
      const std::function<void(Status status, const rapidjson::Value& value)>&
          callback) override;
  void Put(const std::string& key,
           const std::string& data,
           const std::function<void(Status status)>& callback) override;
  void Delete(const std::string& key,
              const std::function<void(Status status)>& callback) override;
  void Watch(const std::string& key,
             const std::string& query,
             WatchClient* watch_client) override;
  void UnWatch(WatchClient* watch_client) override;

 private:
  std::string BuildApiUrl(const std::string& db_id, const std::string& prefix);

  std::string BuildRequestUrl(const std::string& key,
                              const std::string& query) const;

  void Request(
      const std::string& url,
      const std::string& method,
      const std::string& message,
      const std::function<void(Status status, std::string response)>& callback);

  void OnResponse(
      const std::function<void(Status status, std::string response)>& callback,
      network::URLLoader* url_loader,
      network::URLResponsePtr response);

  void StartWatchRequest(const std::string& url, WatchClient* watch_client);

  void OnWatchResponse(WatchClient* watch_client,
                       network::URLResponsePtr response);

  void OnStream(WatchClient* watch_client, mx::datapipe_consumer stream);

  void OnStreamComplete(WatchClient* watch_client);

  void OnStreamEvent(WatchClient* watch_client,
                     Status status,
                     const std::string& event,
                     const std::string& data);

  const network::NetworkServicePtr network_service_;
  // Api url against which requests are made, without a trailing slash.
  const std::string api_url_;

  struct RequestData;
  std::map<network::URLLoader*, std::unique_ptr<RequestData>> request_data_;

  struct WatchData;
  std::map<WatchClient*, std::unique_ptr<WatchData>> watch_data_;
};

}  // namespace firebase

#endif  // APPS_LEDGER_SRC_FIREBASE_FIREBASE_IMPL_H_
