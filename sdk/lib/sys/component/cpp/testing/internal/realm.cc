// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/component/cpp/fidl.h>
#include <lib/fit/function.h>
#include <lib/sys/component/cpp/testing/internal/errors.h>
#include <lib/sys/component/cpp/testing/internal/realm.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/service_directory.h>

#include <cstddef>

namespace component_testing {
namespace internal {

fuchsia::component::RealmSyncPtr CreateRealmPtr(const sys::ComponentContext* context) {
  if (context) {
    return CreateRealmPtr(context->svc());
  }

  return CreateRealmPtr(nullptr);
}

fuchsia::component::RealmSyncPtr CreateRealmPtr(std::shared_ptr<sys::ServiceDirectory> svc) {
  if (svc == nullptr) {
    svc = sys::ServiceDirectory::CreateFromNamespace();
  }

  fuchsia::component::RealmSyncPtr realm;
  svc->Connect(realm.NewRequest());
  return realm;
}

fidl::InterfaceHandle<fuchsia::io::Directory> OpenExposedDir(
    fuchsia::component::Realm_Sync* realm, const fuchsia::component::decl::ChildRef& child_ref) {
  ZX_SYS_ASSERT_NOT_NULL(realm);
  fidl::InterfaceHandle<fuchsia::io::Directory> exposed_dir;
  fuchsia::component::Realm_OpenExposedDir_Result result;
  ZX_COMPONENT_ASSERT_STATUS_AND_RESULT_OK(
      "Realm/OpenExposedDir", realm->OpenExposedDir(child_ref, exposed_dir.NewRequest(), &result),
      result);
  return exposed_dir;
}

void CreateChild(fuchsia::component::Realm_Sync* realm,
#if __Fuchsia_API_level__ >= 14
                 fidl::InterfaceRequest<fuchsia::component::Controller> controller,
#endif
                 std::string collection, std::string name, std::string url) {
  ZX_SYS_ASSERT_NOT_NULL(realm);
  fuchsia::component::decl::CollectionRef collection_ref = {
      .name = collection,
  };
  fuchsia::component::decl::Child child_decl;
  child_decl.set_name(name);
  child_decl.set_url(url);
  child_decl.set_startup(fuchsia::component::decl::StartupMode::LAZY);
  fuchsia::component::Realm_CreateChild_Result result;
  fuchsia::component::CreateChildArgs child_args{};
#if __Fuchsia_API_level__ >= 14
  child_args.set_controller(std::move(controller));
#endif
  ZX_COMPONENT_ASSERT_STATUS_AND_RESULT_OK(
      "Realm/CreateChild",
      realm->CreateChild(std::move(collection_ref), std::move(child_decl), std::move(child_args),
                         &result),
      result);
}

void DestroyChild(fuchsia::component::Realm_Sync* realm,
                  fuchsia::component::decl::ChildRef child_ref) {
  ZX_SYS_ASSERT_NOT_NULL(realm);
  fuchsia::component::Realm_DestroyChild_Result result;
  ZX_COMPONENT_ASSERT_STATUS_AND_RESULT_OK(
      "Realm/DestroyChild", realm->DestroyChild(std::move(child_ref), &result), result);
}

void DestroyChild(fuchsia::component::Realm* realm, fuchsia::component::decl::ChildRef child_ref,
                  fit::function<void(fuchsia::component::Realm_DestroyChild_Result)> callback) {
  ZX_SYS_ASSERT_NOT_NULL(realm);
  realm->DestroyChild(std::move(child_ref), std::move(callback));
}

}  // namespace internal
}  // namespace component_testing
