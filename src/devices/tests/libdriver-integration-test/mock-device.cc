// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "mock-device.h"

#include <zircon/assert.h>

namespace libdriver_integration_test {

MockDevice::MockDevice(fidl::InterfaceRequest<Interface> request, async_dispatcher_t* dispatcher,
                       std::string path)
    : binding_(this, std::move(request), dispatcher), path_(std::move(path)) {}

void MockDevice::Bind(HookInvocation record, BindCallback callback) {
  hooks_->Bind(record, std::move(callback));
}

void MockDevice::Release(HookInvocation record) { hooks_->Release(record); }

void MockDevice::GetProtocol(HookInvocation record, uint32_t protocol_id,
                             GetProtocolCallback callback) {
  hooks_->GetProtocol(record, protocol_id, std::move(callback));
}

void MockDevice::Unbind(HookInvocation record, UnbindCallback callback) {
  hooks_->Unbind(record, std::move(callback));
}

void MockDevice::Suspend(HookInvocation record, uint8_t requested_state, bool enable_wake,
                         uint8_t suspend_reason, SuspendCallback callback) {
  hooks_->Suspend(record, requested_state, enable_wake, suspend_reason, std::move(callback));
}

void MockDevice::Resume(HookInvocation record, uint32_t requested_state, ResumeCallback callback) {
  hooks_->Resume(record, requested_state, std::move(callback));
}

void MockDevice::Message(HookInvocation record, MessageCallback callback) {
  hooks_->Message(record, std::move(callback));
}

void MockDevice::Rxrpc(HookInvocation record, RxrpcCallback callback) {
  hooks_->Rxrpc(record, std::move(callback));
}

void MockDevice::AddDeviceDone(uint64_t action_id) {
  // Check the list of pending actions and signal the corresponding completer
  auto itr = pending_actions_.find(action_id);
  ZX_ASSERT(itr != pending_actions_.end());
  itr->second.complete_ok();
  pending_actions_.erase(itr);
}

void MockDevice::UnbindReplyDone(uint64_t action_id) { AddDeviceDone(action_id); }
void MockDevice::SuspendReplyDone(uint64_t action_id) { AddDeviceDone(action_id); }
void MockDevice::ResumeReplyDone(uint64_t action_id) { AddDeviceDone(action_id); }

std::vector<ActionList::Action> MockDevice::FinalizeActionList(ActionList action_list) {
  return action_list.FinalizeActionList(&pending_actions_, &next_action_id_);
}

}  // namespace libdriver_integration_test
