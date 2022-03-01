// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/network/mdns/service/agents/instance_responder.h"

#include <lib/syslog/cpp/macros.h>

#include <algorithm>

#include "src/connectivity/network/mdns/service/common/mdns_names.h"

namespace mdns {

InstanceResponder::InstanceResponder(MdnsAgent::Owner* owner, const std::string& service_name,
                                     const std::string& instance_name, Media media,
                                     Mdns::Publisher* publisher)
    : MdnsAgent(owner),
      service_name_(service_name),
      instance_name_(instance_name),
      instance_full_name_(MdnsNames::InstanceFullName(instance_name, service_name)),
      media_(media),
      publisher_(publisher) {}

InstanceResponder::~InstanceResponder() {}

void InstanceResponder::Start(const std::string& local_host_full_name) {
  FX_DCHECK(!local_host_full_name.empty());

  MdnsAgent::Start(local_host_full_name);

  host_full_name_ = local_host_full_name;

  Reannounce();
}

void InstanceResponder::ReceiveQuestion(const DnsQuestion& question,
                                        const ReplyAddress& reply_address,
                                        const ReplyAddress& sender_address) {
  std::string name = question.name_.dotted_string_;
  std::string subtype;

  if (media_ != Media::kBoth && sender_address.media() != media_) {
    // Question received on unsupported medium. Ignore.
    return;
  }

  // We infer publication cause from the reply address. Announcements don't use this path, and
  // there are cases in which we send unicast even though the unicast bit is not set on the question
  // (specifically, when the reply address port isn't 5353). A V4 multicast reply address indicates
  // V4 and V6 multicast.
  auto publication_cause = reply_address.socket_address() == MdnsAddresses::v4_multicast()
                               ? PublicationCause::kQueryMulticastResponse
                               : PublicationCause::kQueryUnicastResponse;

  switch (question.type_) {
    case DnsType::kPtr:
      if (MdnsNames::MatchServiceName(name, service_name_, &subtype)) {
        LogSenderAddress(sender_address);
        MaybeGetAndSendPublication(publication_cause, subtype, reply_address);
      } else if (question.name_.dotted_string_ == MdnsNames::kAnyServiceFullName) {
        SendAnyServiceResponse(reply_address);
      }
      break;
    case DnsType::kSrv:
    case DnsType::kTxt:
      if (question.name_.dotted_string_ == instance_full_name_) {
        LogSenderAddress(sender_address);
        MaybeGetAndSendPublication(publication_cause, "", reply_address);
      }
      break;
    case DnsType::kAny:
      if (question.name_.dotted_string_ == instance_full_name_ ||
          MdnsNames::MatchServiceName(name, service_name_, &subtype)) {
        LogSenderAddress(sender_address);
        MaybeGetAndSendPublication(publication_cause, subtype, reply_address);
      }
      break;
    default:
      break;
  }
}

void InstanceResponder::Quit() {
  if (started()) {
    SendGoodbye();
  }

  publisher_ = nullptr;

  MdnsAgent::Quit();
}

void InstanceResponder::SetSubtypes(std::vector<std::string> subtypes) {
  if (!started()) {
    // This agent isn't started, so we can't announce yet. There's no need to
    // remove old subtypes, because no subtypes have been announced yet.
    // |Reannounce| will be called by |Start|.
    subtypes_ = std::move(subtypes);
    return;
  }

  // Initiate four announcements with intervals of 1, 2 and 4 seconds. If we
  // were already announcing, the sequence restarts now. The first announcement
  // contains PTR records for the removed subtypes with TTL of zero.
  for (const std::string& subtype : subtypes_) {
    if (std::find(subtypes.begin(), subtypes.end(), subtype) == subtypes.end()) {
      SendSubtypePtrRecord(subtype, 0, multicast_reply());
    }
  }

  subtypes_ = std::move(subtypes);

  Reannounce();
}

void InstanceResponder::Reannounce() {
  if (!started()) {
    // This agent isn't started, so we can't announce yet. |Reannounce| will be called by |Start|.
    return;
  }

  // Initiate four announcements with intervals of 1, 2 and 4 seconds. If we
  // were already announcing, the sequence restarts now.
  announcement_interval_ = kInitialAnnouncementInterval;
  SendAnnouncement();
}

void InstanceResponder::LogSenderAddress(const ReplyAddress& sender_address) {
  if (sender_addresses_.size() == kMaxSenderAddresses) {
    // We only record up to 64 addresses. This limit should rarely be hit, because sender addresses
    // only accumulate when we're throttling over a one-second interval. Most of the time, we'll log
    // only one sender address before calling |GetPublication| and clearing the list.
    return;
  }

  sender_addresses_.push_back(sender_address.socket_address());
}

void InstanceResponder::SendAnnouncement() {
  GetAndSendPublication(PublicationCause::kAnnouncement, "", multicast_reply());

  for (const std::string& subtype : subtypes_) {
    SendSubtypePtrRecord(subtype, DnsResource::kShortTimeToLive, multicast_reply());
  }

  if (announcement_interval_ > kMaxAnnouncementInterval) {
    return;
  }

  PostTaskForTime([this]() { SendAnnouncement(); }, now() + announcement_interval_);

  announcement_interval_ = announcement_interval_ * 2;
}

void InstanceResponder::SendAnyServiceResponse(const ReplyAddress& reply_address) {
  auto ptr_resource = std::make_shared<DnsResource>(MdnsNames::kAnyServiceFullName, DnsType::kPtr);
  ptr_resource->ptr_.pointer_domain_name_ = MdnsNames::ServiceFullName(service_name_);
  SendResource(ptr_resource, MdnsResourceSection::kAnswer, reply_address);
}

void InstanceResponder::MaybeGetAndSendPublication(PublicationCause publication_cause,
                                                   const std::string& subtype,
                                                   const ReplyAddress& reply_address) {
  if (publisher_ == nullptr) {
    return;
  }

  // We only throttle multicast sends.
  if (publication_cause == PublicationCause::kQueryMulticastResponse) {
    zx::time throttle_state = kThrottleStateIdle;
    auto iter = throttle_state_by_subtype_.find(subtype);
    if (iter != throttle_state_by_subtype_.end()) {
      throttle_state = iter->second;
    }

    if (throttle_state == kThrottleStatePending) {
      // The send is already happening.
      return;
    }

    // We're either going to send now or schedule a send. In either case, a send is pending.
    throttle_state_by_subtype_[subtype] = kThrottleStatePending;

    if (throttle_state + kMinMulticastInterval > now()) {
      // A multicast publication of this subtype was sent less than a second ago, and no send is
      // currently scheduled. We need to schedule a multicast send for one second after the
      // previous one.
      PostTaskForTime(
          [this, publication_cause, subtype, reply_address]() {
            GetAndSendPublication(publication_cause, subtype, reply_address);
          },
          throttle_state + kMinMulticastInterval);
      return;
    }

    // We fall through here when throttling isn't in effect, and the response should be sent
    // immediately.
  }

  GetAndSendPublication(publication_cause, subtype, reply_address);
}

void InstanceResponder::GetAndSendPublication(PublicationCause publication_cause,
                                              const std::string& subtype,
                                              const ReplyAddress& reply_address) {
  if (publisher_ == nullptr) {
    return;
  }

  bool query = publication_cause != PublicationCause::kAnnouncement;

  publisher_->GetPublication(
      publication_cause, subtype, sender_addresses_,
      [this, query, subtype, reply_address](std::unique_ptr<Mdns::Publication> publication) {
        if (publication) {
          SendPublication(*publication, subtype, reply_address);
          // Make sure messages get sent immediately if this callback happens asynchronously
          // with respect to |ReceiveQuestion| or posted task execution.
          FlushSentItems();

          // A V4 multicast reply address indicates V4 and V6 multicast.
          if (query && reply_address.socket_address() == MdnsAddresses::v4_multicast()) {
            throttle_state_by_subtype_[subtype] = now();
            // Remove the entry from |throttle_state_by_subtype_| later to prevent the map from
            // growing indefinitely.
            PostTaskForTime([this, subtype]() { IdleCheck(subtype); }, now() + kIdleCheckInterval);
          }
        }
      });

  sender_addresses_.clear();
}

void InstanceResponder::SendPublication(const Mdns::Publication& publication,
                                        const std::string& subtype,
                                        const ReplyAddress& reply_address) const {
  if (!subtype.empty()) {
    SendSubtypePtrRecord(subtype, publication.ptr_ttl_seconds_, reply_address);
  }

  auto ptr_resource =
      std::make_shared<DnsResource>(MdnsNames::ServiceFullName(service_name_), DnsType::kPtr);
  ptr_resource->time_to_live_ = publication.ptr_ttl_seconds_;
  ptr_resource->ptr_.pointer_domain_name_ = instance_full_name_;
  SendResource(ptr_resource, MdnsResourceSection::kAnswer, reply_address);

  auto srv_resource = std::make_shared<DnsResource>(instance_full_name_, DnsType::kSrv);
  srv_resource->time_to_live_ = publication.srv_ttl_seconds_;
  srv_resource->srv_.priority_ = publication.srv_priority_;
  srv_resource->srv_.weight_ = publication.srv_weight_;
  srv_resource->srv_.port_ = publication.port_;
  srv_resource->srv_.target_ = host_full_name_;
  SendResource(srv_resource, MdnsResourceSection::kAdditional, reply_address);

  auto txt_resource = std::make_shared<DnsResource>(instance_full_name_, DnsType::kTxt);
  txt_resource->time_to_live_ = publication.txt_ttl_seconds_;
  txt_resource->txt_.strings_ = publication.text_;
  SendResource(txt_resource, MdnsResourceSection::kAdditional, reply_address);

  SendAddresses(MdnsResourceSection::kAdditional, reply_address);
}

void InstanceResponder::SendSubtypePtrRecord(const std::string& subtype, uint32_t ttl,
                                             const ReplyAddress& reply_address) const {
  FX_DCHECK(!subtype.empty());

  auto ptr_resource = std::make_shared<DnsResource>(
      MdnsNames::ServiceSubtypeFullName(service_name_, subtype), DnsType::kPtr);
  ptr_resource->time_to_live_ = ttl;
  ptr_resource->ptr_.pointer_domain_name_ = instance_full_name_;
  SendResource(ptr_resource, MdnsResourceSection::kAnswer, reply_address);
}

void InstanceResponder::SendGoodbye() const {
  Mdns::Publication publication;
  publication.ptr_ttl_seconds_ = 0;
  publication.srv_ttl_seconds_ = 0;
  publication.txt_ttl_seconds_ = 0;

  SendPublication(publication, "", multicast_reply());
}

void InstanceResponder::IdleCheck(const std::string& subtype) {
  auto iter = throttle_state_by_subtype_.find(subtype);
  if (iter != throttle_state_by_subtype_.end() && iter->second + kMinMulticastInterval < now()) {
    throttle_state_by_subtype_.erase(iter);
  }
}

ReplyAddress InstanceResponder::multicast_reply() const {
  return ReplyAddress::Multicast(media_, IpVersions::kBoth);
}

}  // namespace mdns
