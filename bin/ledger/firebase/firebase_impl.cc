// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "apps/ledger/src/firebase/firebase_impl.h"

#include <utility>

#include "apps/ledger/src/glue/data_pipe/data_pipe.h"
#include "apps/ledger/src/glue/data_pipe/data_pipe_drainer_client.h"
#include "apps/ledger/src/glue/data_pipe/data_pipe_writer.h"
#include "lib/ftl/logging.h"
#include "lib/ftl/strings/ascii.h"

namespace firebase {

namespace {

network::URLRequestPtr MakeRequest(const std::string& url,
                                   const std::string& method,
                                   const std::string& message) {
  network::URLRequestPtr request(network::URLRequest::New());
  request->url = url;
  request->method = method;
  request->auto_follow_redirects = true;
  if (!message.empty()) {
    glue::DataPipe data_pipe;
    // Deletes itself.
    glue::DataPipeWriter* writer = new glue::DataPipeWriter();
    writer->Start(message, std::move(data_pipe.producer_handle));
    request->body = network::URLBody::New();
    request->body->set_stream(std::move(data_pipe.consumer_handle));
  }
  return request;
}

}  // namespace

struct FirebaseImpl::RequestData {
  RequestData();
  ~RequestData();

  network::URLLoaderPtr url_loader;
  std::unique_ptr<glue::DataPipeDrainerClient> drainer;
};

FirebaseImpl::RequestData::RequestData() {}
FirebaseImpl::RequestData::~RequestData() {}

struct FirebaseImpl::WatchData {
  WatchData();
  ~WatchData();

  network::URLLoaderPtr url_loader;
  std::unique_ptr<EventStream> event_stream;
  std::unique_ptr<glue::DataPipeDrainerClient> drainer;
};

FirebaseImpl::WatchData::WatchData() {}
FirebaseImpl::WatchData::~WatchData() {}

FirebaseImpl::FirebaseImpl(network::NetworkServicePtr network_service,
                           const std::string& db_id,
                           const std::string& prefix)
    : network_service_(std::move(network_service)),
      api_url_(BuildApiUrl(db_id, prefix)) {}

FirebaseImpl::~FirebaseImpl() {}

void FirebaseImpl::Get(
    const std::string& key,
    const std::string& query,
    const std::function<void(Status status, const rapidjson::Value& value)>&
        callback) {
  auto request_callback = [callback](Status status,
                                     const std::string& response) {
    if (status != Status::OK) {
      callback(status, rapidjson::Value());
      return;
    }

    rapidjson::Document document;
    document.Parse(response.c_str(), response.size());
    if (document.HasParseError()) {
      callback(Status::UNKNOWN_ERROR, rapidjson::Value());
      return;
    }

    callback(Status::OK, document);
  };

  Request(BuildRequestUrl(key, query), "GET", "", request_callback);
}

void FirebaseImpl::Put(const std::string& key,
                       const std::string& data,
                       const std::function<void(Status status)>& callback) {
  Request(BuildRequestUrl(key, ""), "PUT", data,
          [callback](Status status, const std::string& response) {
            // Ignore the response body, which is the same data we sent to the
            // server.
            callback(status);
          });
}

void FirebaseImpl::Delete(const std::string& key,
                          const std::function<void(Status status)>& callback) {
  Request(BuildRequestUrl(key, ""), "DELETE", "",
          [callback](Status status, const std::string& response) {
            callback(status);
          });
}

void FirebaseImpl::Watch(const std::string& key,
                         const std::string& query,
                         WatchClient* watch_client) {
  watch_data_[watch_client] = std::unique_ptr<WatchData>(new WatchData());
  StartWatchRequest(BuildRequestUrl(key, query), watch_client);
}

void FirebaseImpl::UnWatch(WatchClient* watch_client) {
  watch_data_.erase(watch_client);
}

std::string FirebaseImpl::BuildApiUrl(const std::string& db_id,
                                      const std::string& prefix) {
  std::string api_url = "https://" + db_id + ".firebaseio.com";

  if (!prefix.empty()) {
    FTL_DCHECK(prefix.front() != '/');
    FTL_DCHECK(prefix.back() != '/');
    api_url.append("/");
    api_url.append(prefix);
  }

  FTL_DCHECK(api_url.back() != '/');
  return api_url;
}

std::string FirebaseImpl::BuildRequestUrl(const std::string& key,
                                          const std::string& query) const {
  std::string url = api_url_;
  url.append("/");
  url.append(key);
  url.append(".json");
  if (!query.empty()) {
    url.append("?");
    url.append(query);
  }
  return url;
}

void FirebaseImpl::Request(
    const std::string& url,
    const std::string& method,
    const std::string& message,
    const std::function<void(Status status, std::string response)>& callback) {
  network::URLLoaderPtr url_loader;
  network_service_->CreateURLLoader(GetProxy(&url_loader));
  network::URLRequestPtr request = MakeRequest(url, method, message);

  network::URLLoader* url_loader_ptr = url_loader.get();
  url_loader->Start(std::move(request), [this, callback, url_loader_ptr](
                                            network::URLResponsePtr response) {
    OnResponse(callback, url_loader_ptr, std::move(response));
  });
  request_data_[url_loader.get()] = std::make_unique<RequestData>();
  request_data_[url_loader.get()]->url_loader = std::move(url_loader);
}

void FirebaseImpl::OnResponse(
    const std::function<void(Status status, std::string response)>& callback,
    network::URLLoader* url_loader,
    network::URLResponsePtr response) {
  // No need to hang onto the URLLoaderPtr anymore.
  request_data_[url_loader]->url_loader.reset();

  if (response->error) {
    FTL_LOG(ERROR) << response->url << " error "
                   << response->error->description;
    callback(Status::UNKNOWN_ERROR, "");
    return;
  }

  if (response->status_code != 200 && response->status_code != 204) {
    const std::string& url = response->url;
    const std::string& status_line = response->status_line;
    request_data_[url_loader]->drainer =
        std::make_unique<glue::DataPipeDrainerClient>();
    FTL_DCHECK(response->body->is_stream());
    request_data_[url_loader]->drainer->Start(
        std::move(response->body->get_stream()),
        [this, callback, url_loader, url,
         status_line](const std::string& body) {
          FTL_LOG(ERROR) << url << " error " << status_line << ":" << std::endl
                         << body;
          callback(Status::UNKNOWN_ERROR, "");
          request_data_.erase(url_loader);
        });
    return;
  }

  FTL_DCHECK(response->body->is_stream());
  request_data_[url_loader]->drainer =
      std::make_unique<glue::DataPipeDrainerClient>();
  request_data_[url_loader]->drainer->Start(
      std::move(response->body->get_stream()),
      [this, callback, url_loader](const std::string& body) {
        callback(Status::OK, body);
        request_data_.erase(url_loader);
      });
}

void FirebaseImpl::StartWatchRequest(const std::string& url,
                                     WatchClient* watch_client) {
  network::URLLoaderPtr url_loader;
  network_service_->CreateURLLoader(GetProxy(&url_loader));
  network::URLRequestPtr request = MakeRequest(url, "GET", "");

  auto accept_header = network::HttpHeader::New();
  accept_header->name = "Accept";
  accept_header->value = "text/event-stream";
  request->headers.push_back(std::move(accept_header));

  url_loader->Start(std::move(request),
                    [this, watch_client](network::URLResponsePtr response) {
                      OnWatchResponse(watch_client, std::move(response));
                    });
  watch_data_[watch_client]->url_loader = std::move(url_loader);
}

void FirebaseImpl::OnWatchResponse(WatchClient* watch_client,
                                   network::URLResponsePtr response) {
  // No need to hang onto the URLLoaderPtr anymore.
  watch_data_[watch_client]->url_loader.reset();

  if (response->error) {
    FTL_LOG(ERROR) << response->url << " error "
                   << response->error->description;
    watch_client->OnError();
    watch_client->OnDone();
    watch_data_.erase(watch_client);
    return;
  }

  FTL_DCHECK(response->body->is_stream());

  // Handle redirect.
  if (response->status_code == 307) {
    for (const auto& header : response->headers) {
      if (ftl::EqualsCaseInsensitiveASCII(header->name.get(), "location")) {
        StartWatchRequest(header->value, watch_client);
        return;
      }
    }
    // If location is not found, fall back to the error case below.
  }

  if (response->status_code != 200 && response->status_code != 204) {
    const std::string& url = response->url;
    const std::string& status_line = response->status_line;
    watch_data_[watch_client]->drainer =
        std::make_unique<glue::DataPipeDrainerClient>();
    watch_data_[watch_client]->drainer->Start(
        std::move(response->body->get_stream()),
        [this, watch_client, url, status_line](const std::string& body) {
          FTL_LOG(ERROR) << url << " error " << status_line << ":" << std::endl
                         << body;
          watch_client->OnError();
          watch_client->OnDone();
          watch_data_.erase(watch_client);
        });
    return;
  }

  OnStream(watch_client, std::move(response->body->get_stream()));
}

void FirebaseImpl::OnStream(WatchClient* watch_client,
                            mx::datapipe_consumer stream) {
  watch_data_[watch_client]->event_stream = std::make_unique<EventStream>();
  watch_data_[watch_client]->event_stream->Start(
      std::move(stream),
      [this, watch_client](Status status, const std::string& event,
                           const std::string& data) {
        OnStreamEvent(watch_client, status, event, data);
      },
      [this, watch_client]() { OnStreamComplete(watch_client); });
}

void FirebaseImpl::OnStreamComplete(WatchClient* watch_client) {
  watch_data_[watch_client]->event_stream.reset();
  watch_client->OnDone();
  watch_data_.erase(watch_client);
}

void FirebaseImpl::OnStreamEvent(WatchClient* watch_client,
                                 Status status,
                                 const std::string& event,
                                 const std::string& data) {
  if (event == "put" || event == "patch") {
    rapidjson::Document document;
    document.Parse(data.c_str(), data.size());
    if (document.HasParseError()) {
      watch_client->OnError();
      return;
    }

    // Both 'put' and 'patch' events must carry a dictionary of "path" and
    // "data".
    if (!document.IsObject() || !document.HasMember("path") ||
        !document["path"].IsString() || !document.HasMember("data")) {
      watch_client->OnError();
      return;
    }

    if (event == "put") {
      watch_client->OnPut(document["path"].GetString(), document["data"]);
    } else if (event == "patch") {
      // In case of patch, data must be a dictionary itself.
      if (!document["data"].IsObject()) {
        watch_client->OnError();
        return;
      }

      watch_client->OnPatch(document["path"].GetString(), document["data"]);
    } else {
      FTL_NOTREACHED();
    }
  } else if (event == "keep-alive") {
    // Do nothing.
  } else if (event == "cancel") {
    watch_client->OnCancel();
  } else if (event == "auth_revoked") {
    rapidjson::Document document;
    document.Parse(data.c_str(), data.size());
    if (document.HasParseError()) {
      watch_client->OnError();
      return;
    }

    std::string reason;
    if (!document.IsString()) {
      watch_client->OnError();
      return;
    }
    watch_client->OnAuthRevoked(document.GetString());
  } else {
    watch_client->OnError();
  }
}

}  // namespace firebase
