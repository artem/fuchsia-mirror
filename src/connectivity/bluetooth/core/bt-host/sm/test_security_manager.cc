// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/test_security_manager.h"

#include <memory>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/assert.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/connection.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/smp.h"

namespace bt::sm::testing {

TestSecurityManager::TestSecurityManager(hci::LowEnergyConnection::WeakPtr link,
                                         l2cap::Channel::WeakPtr smp,
                                         IOCapability io_capability,
                                         Delegate::WeakPtr delegate,
                                         BondableMode bondable_mode,
                                         gap::LESecurityMode security_mode)
    : SecurityManager(bondable_mode, security_mode),
      role_(link->role() == pw::bluetooth::emboss::ConnectionRole::CENTRAL
                ? Role::kInitiator
                : Role::kResponder),
      weak_self_(this) {}

bool TestSecurityManager::AssignLongTermKey(const LTK& ltk) {
  current_ltk_ = ltk;
  if (role_ == Role::kInitiator) {
    set_security(ltk.security());
  }
  return true;
}

void TestSecurityManager::UpgradeSecurity(SecurityLevel level,
                                          PairingCallback callback) {
  last_requested_upgrade_ = level;
  set_security(SecurityProperties(
      level, kMaxEncryptionKeySize, /*secure_connections=*/true));
  callback(fit::ok(), security());
}

void TestSecurityManager::Reset(IOCapability io_capability) {}
void TestSecurityManager::Abort(ErrorCode ecode) {}

std::unique_ptr<SecurityManager> TestSecurityManagerFactory::CreateSm(
    hci::LowEnergyConnection::WeakPtr link,
    l2cap::Channel::WeakPtr smp,
    IOCapability io_capability,
    Delegate::WeakPtr delegate,
    BondableMode bondable_mode,
    gap::LESecurityMode security_mode,
    pw::async::Dispatcher& /*dispatcher*/) {
  hci_spec::ConnectionHandle conn = link->handle();
  auto test_sm = std::unique_ptr<TestSecurityManager>(
      new TestSecurityManager(std::move(link),
                              std::move(smp),
                              io_capability,
                              std::move(delegate),
                              bondable_mode,
                              security_mode));
  test_sms_[conn] = test_sm->GetWeakPtr();
  return test_sm;
}

WeakSelf<TestSecurityManager>::WeakPtr TestSecurityManagerFactory::GetTestSm(
    hci_spec::ConnectionHandle conn_handle) {
  auto iter = test_sms_.find(conn_handle);
  BT_ASSERT(iter != test_sms_.end());
  return iter->second;
}

}  // namespace bt::sm::testing
