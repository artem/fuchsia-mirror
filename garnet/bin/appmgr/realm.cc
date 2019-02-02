// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "garnet/bin/appmgr/realm.h"

#include <fcntl.h>
#include <lib/async/default.h>
#include <lib/fdio/namespace.h>
#include <lib/fdio/spawn.h>
#include <lib/fdio/util.h>
#include <lib/zx/process.h>
#include <trace/event.h>
#include <unistd.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/status.h>

#include <utility>

#include "garnet/bin/appmgr/dynamic_library_loader.h"
#include "garnet/bin/appmgr/hub/realm_hub.h"
#include "garnet/bin/appmgr/namespace_builder.h"
#include "garnet/bin/appmgr/scheme_map.h"
#include "garnet/bin/appmgr/util.h"
#include "garnet/lib/cmx/cmx.h"
#include "garnet/lib/cmx/program.h"
#include "garnet/lib/cmx/runtime.h"
#include "garnet/lib/cmx/sandbox.h"
#include "lib/component/cpp/connect.h"
#include "lib/fsl/handles/object_info.h"
#include "lib/fsl/io/fd.h"
#include "lib/fsl/vmo/file.h"
#include "lib/fsl/vmo/strings.h"
#include "lib/fxl/files/directory.h"
#include "lib/fxl/files/file.h"
#include "lib/fxl/strings/concatenate.h"
#include "lib/fxl/strings/string_printf.h"
#include "lib/fxl/strings/substitute.h"
#include "lib/json/json_parser.h"
#include "lib/pkg_url/fuchsia_pkg_url.h"
#include "lib/pkg_url/url_resolver.h"
#include "lib/svc/cpp/services.h"

namespace component {
namespace {

constexpr char kAppPath[] = "bin/app";
constexpr char kDataPathPrefix[] = "data/";
constexpr char kDataKey[] = "data";
constexpr char kAppArgv0Prefix[] = "/pkg/";
constexpr char kLegacyFlatExportedDirPath[] = "meta/legacy_flat_exported_dir";
constexpr zx_status_t kComponentCreationFailed = -1;

using fuchsia::sys::TerminationReason;

std::vector<const char*> GetArgv(const std::string& argv0,
                                 const fuchsia::sys::LaunchInfo& launch_info) {
  std::vector<const char*> argv;
  argv.reserve(launch_info.arguments->size() + 2);
  argv.push_back(argv0.c_str());
  for (const auto& argument : *launch_info.arguments)
    argv.push_back(argument.c_str());
  argv.push_back(nullptr);
  return argv;
}

void PushHandle(uint32_t id, zx_handle_t handle,
                std::vector<fdio_spawn_action_t>* actions) {
  actions->push_back({.action = FDIO_SPAWN_ACTION_ADD_HANDLE,
                      .h = {.id = id, .handle = handle}});
}

void PushFileDescriptor(fuchsia::sys::FileDescriptorPtr fd, int target_fd,
                        std::vector<fdio_spawn_action_t>* actions) {
  if (!fd) {
    actions->push_back({.action = FDIO_SPAWN_ACTION_CLONE_FD,
                        .fd = {.local_fd = target_fd, .target_fd = target_fd}});
    return;
  }
  if (fd->type0) {
    uint32_t id = PA_HND(PA_HND_TYPE(fd->type0), target_fd);
    PushHandle(id, fd->handle0.release(), actions);
  }
  if (fd->type1) {
    uint32_t id = PA_HND(PA_HND_TYPE(fd->type1), target_fd);
    PushHandle(id, fd->handle1.release(), actions);
  }
  if (fd->type2) {
    uint32_t id = PA_HND(PA_HND_TYPE(fd->type2), target_fd);
    PushHandle(id, fd->handle2.release(), actions);
  }
}

zx::process CreateProcess(const zx::job& job, fsl::SizedVmo data,
                          const std::string& argv0,
                          fuchsia::sys::LaunchInfo launch_info,
                          zx::channel loader_service,
                          fdio_flat_namespace_t* flat) {
  TRACE_DURATION("appmgr", "Realm::CreateProcess", "launch_info.url",
                 launch_info.url);
  if (!data)
    return zx::process();

  zx::job duplicate_job;
  zx_status_t status = job.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate_job);
  if (status != ZX_OK)
    return zx::process();

  std::string label = Util::GetLabelFromURL(launch_info.url);
  std::vector<const char*> argv = GetArgv(argv0, launch_info);

  uint32_t flags = 0u;

  std::vector<fdio_spawn_action_t> actions;

  PushHandle(PA_JOB_DEFAULT, duplicate_job.release(), &actions);

  if (loader_service) {
    PushHandle(PA_LDSVC_LOADER, loader_service.release(), &actions);
  } else {
    // TODO(CP-62): Processes that don't have their own package use the appmgr's
    // dynamic library loader, which doesn't make much sense. We need to find an
    // appropriate loader service for each executable.
    flags |= FDIO_SPAWN_DEFAULT_LDSVC;
  }

  zx::channel directory_request = std::move(launch_info.directory_request);
  if (directory_request)
    PushHandle(PA_DIRECTORY_REQUEST, directory_request.release(), &actions);

  PushFileDescriptor(nullptr, STDIN_FILENO, &actions);
  PushFileDescriptor(std::move(launch_info.out), STDOUT_FILENO, &actions);
  PushFileDescriptor(std::move(launch_info.err), STDERR_FILENO, &actions);

  actions.push_back(
      {.action = FDIO_SPAWN_ACTION_SET_NAME, .name = {.data = label.c_str()}});

  for (size_t i = 0; i < flat->count; ++i) {
    actions.push_back(
        {.action = FDIO_SPAWN_ACTION_ADD_NS_ENTRY,
         .ns = {.prefix = flat->path[i], .handle = flat->handle[i]}});
  }

  data.vmo().set_property(ZX_PROP_NAME, label.data(), label.size());

  zx::process process;
  char err_msg[FDIO_SPAWN_ERR_MSG_MAX_LENGTH];

  status = fdio_spawn_vmo(job.get(), flags, data.vmo().release(), argv.data(),
                          nullptr, actions.size(), actions.data(),
                          process.reset_and_get_address(), err_msg);

  if (status != ZX_OK) {
    FXL_LOG(ERROR) << "Cannot run executable " << label << " due to error "
                   << status << " (" << zx_status_get_string(status)
                   << "): " << err_msg;
  }

  return process;
}

}  // namespace

// static
RealmArgs RealmArgs::Make(
    Realm* parent, std::string label,
    const std::shared_ptr<component::Services>& env_services,
    bool run_virtual_console, bool inherit_parent_services, bool kill_on_oom) {
  return {.parent = parent,
          .label = label,
          .environment_services = env_services,
          .run_virtual_console = run_virtual_console,
          .additional_services = nullptr,
          .inherit_parent_services = inherit_parent_services,
          .kill_on_oom = kill_on_oom};
}

RealmArgs RealmArgs::MakeWithAdditionalServices(
    Realm* parent, std::string label,
    const std::shared_ptr<component::Services>& env_services,
    bool run_virtual_console, fuchsia::sys::ServiceListPtr additional_services,
    bool inherit_parent_services, bool kill_on_oom) {
  return {.parent = parent,
          .label = label,
          .environment_services = env_services,
          .run_virtual_console = run_virtual_console,
          .additional_services = std::move(additional_services),
          .inherit_parent_services = inherit_parent_services,
          .kill_on_oom = kill_on_oom};
}

Realm::Realm(RealmArgs args)
    : parent_(args.parent),
      run_virtual_console_(args.run_virtual_console),
      hub_(fbl::AdoptRef(new fs::PseudoDir())),
      info_vfs_(async_get_default_dispatcher()),
      environment_services_(args.environment_services),
      allow_parent_runners_(args.allow_parent_runners) {
  // parent_ is null if this is the root application environment. if so, we
  // derive from the application manager's job.
  zx::unowned<zx::job> parent_job;
  if (parent_) {
    parent_job = zx::unowned<zx::job>(parent_->job_);
  } else {
    parent_job = zx::unowned<zx::job>(zx::job::default_job());
  }

  // init svc service channel for root application environment
  if (parent_ == nullptr) {
    FXL_CHECK(zx::channel::create(0, &svc_channel_server_,
                                  &svc_channel_client_) == ZX_OK);
  }
  FXL_CHECK(zx::job::create(*parent_job, 0u, &job_) == ZX_OK);

  koid_ = std::to_string(fsl::GetKoid(job_.get()));
  FXL_CHECK(!args.label.empty());
  label_ = args.label.substr(0, fuchsia::sys::kLabelMaxLength);

  if (args.kill_on_oom) {
    size_t property_value = 1;
    job_.set_property(ZX_PROP_JOB_KILL_ON_OOM, &property_value,
                      sizeof(property_value));
  }

  if (args.inherit_parent_services) {
    default_namespace_ = fxl::MakeRefCounted<Namespace>(
        parent_->default_namespace_, this, std::move(args.additional_services),
        nullptr);
  } else {
    default_namespace_ = fxl::MakeRefCounted<Namespace>(
        nullptr, this, std::move(args.additional_services), nullptr);
  }

  fsl::SetObjectName(job_.get(), label_);
  hub_.SetName(label_);
  hub_.SetJobId(koid_);
  hub_.AddServices(default_namespace_->services());
  hub_.AddJobProvider(fbl::AdoptRef(new fs::Service([this](
                                                        zx::channel channel) {
    default_namespace_->job_provider()->AddBinding(
        fidl::InterfaceRequest<fuchsia::sys::JobProvider>(std::move(channel)));
    return ZX_OK;
  })));

  if (!parent_) {
    // Set up Loader service for root realm.
    package_loader_.reset(new component::PackageLoader);
    default_namespace_->services()->AddService(
        fuchsia::sys::Loader::Name_,
        fbl::AdoptRef(new fs::Service([this](zx::channel channel) {
          package_loader_->AddBinding(
              fidl::InterfaceRequest<fuchsia::sys::Loader>(std::move(channel)));
          return ZX_OK;
        })));
  }

  fuchsia::sys::ServiceProviderPtr service_provider;
  default_namespace_->services()->AddBinding(service_provider.NewRequest());
  loader_ =
      component::ConnectToService<fuchsia::sys::Loader>(service_provider.get());

  std::string error;
  if (!files::IsDirectory(SchemeMap::kConfigDirPath)) {
    FXL_LOG(FATAL) << "Could not find scheme map config dir: "
                   << SchemeMap::kConfigDirPath;
  }
  if (!scheme_map_.ParseFromDirectory(SchemeMap::kConfigDirPath)) {
    FXL_LOG(FATAL) << "Could not parse scheme map config dir: "
                   << scheme_map_.error_str();
  }
}

Realm::~Realm() { job_.kill(); }

zx::channel Realm::OpenInfoDir() {
  return Util::OpenAsDirectory(&info_vfs_, hub_dir());
}

HubInfo Realm::HubInfo() {
  return component::HubInfo(label_, koid_, hub_.dir());
}

zx::job Realm::DuplicateJob() const {
  zx::job duplicate_job;
  zx_status_t status =
      job_.duplicate(ZX_RIGHTS_BASIC | ZX_RIGHT_WRITE, &duplicate_job);
  if (status == ZX_ERR_INVALID_ARGS) {
    // In the process of removing WRITE for processes; if duplicate with WRITE
    // failed, try the new rights. TODO(ZX-2967): Once the transition is
    // complete, only duplicate with MANAGE_PROCESS.
    status = job_.duplicate(ZX_RIGHTS_BASIC | ZX_RIGHT_MANAGE_PROCESS,
                            &duplicate_job);
  }
  if (status != ZX_OK) {
    return zx::job();
  }
  return duplicate_job;
}

void Realm::CreateNestedEnvironment(
    fidl::InterfaceRequest<fuchsia::sys::Environment> environment,
    fidl::InterfaceRequest<fuchsia::sys::EnvironmentController>
        controller_request,
    std::string label, fuchsia::sys::ServiceListPtr additional_services,
    fuchsia::sys::EnvironmentOptions options) {
  TRACE_DURATION("appmgr", "Realm::CreateNestedEnvironment", "label", label);

  // Check that label is non-empty and unique among existing children.
  if (label.empty()) {
    environment.Close(ZX_ERR_INVALID_ARGS);
    controller_request.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  for (const auto& child : children_) {
    if (label == child.first->label_) {
      FXL_LOG(ERROR) << "Attempt to create nested environment '"
                     << label.c_str() << "' under '" << label_
                     << "' but label matches existing environment";
      environment.Close(ZX_ERR_BAD_STATE);
      controller_request.Close(ZX_ERR_BAD_STATE);
      return;
    }
  }

  if (additional_services && !additional_services->host_directory) {
    FXL_LOG(ERROR) << label.c_str()
                   << ": |additional_services.provider| is not supported for "
                   << "CreateNestedEnvironment. Use "
                   << "|additional_services.host_directory| instead.";
    environment.Close(ZX_ERR_INVALID_ARGS);
    controller_request.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  RealmArgs args;
  if (additional_services) {
    args = RealmArgs::MakeWithAdditionalServices(
        this, label, environment_services_, /*run_virtual_console=*/false,
        std::move(additional_services), options.inherit_parent_services,
        options.kill_on_oom);
  } else {
    args =
        RealmArgs::Make(this, label, environment_services_,
                        /*run_virtual_console=*/false,
                        options.inherit_parent_services, options.kill_on_oom);
  }
  args.allow_parent_runners = options.allow_parent_runners;
  auto controller = std::make_unique<EnvironmentControllerImpl>(
      std::move(controller_request), std::make_unique<Realm>(std::move(args)));
  Realm* child = controller->realm();
  child->AddBinding(std::move(environment));

  // update hub
  hub_.AddRealm(child->HubInfo());

  Realm* root_realm = this;
  while (root_realm->parent() != nullptr) {
    root_realm = root_realm->parent();
  }
  child->default_namespace_->ServeServiceDirectory(
      std::move(root_realm->svc_channel_server_));

  controller->OnCreated();
  children_.emplace(child, std::move(controller));

  if (run_virtual_console_) {
    // TODO(anmittal): remove svc hardcoding once we no longer need to launch
    // shell with sysmgr services, i.e once we have chrealm.
    CreateShell("/boot/bin/run-vc",
                child->default_namespace_->OpenServicesAsDirectory());
    CreateShell("/boot/bin/run-vc",
                child->default_namespace_->OpenServicesAsDirectory());
    CreateShell("/boot/bin/run-vc",
                child->default_namespace_->OpenServicesAsDirectory());
  }
}

void Realm::Resolve(fidl::StringPtr name,
                    fuchsia::process::Resolver::ResolveCallback callback) {
  TRACE_DURATION("appmgr", "Realm::ResolveLoader", "name", name.get());

  zx::vmo binary;
  fidl::InterfaceHandle<fuchsia::ldsvc::Loader> loader;

  if (name->empty()) {
    FXL_LOG(ERROR) << "Cannot resolve loader because requested name is empty";
    callback(ZX_ERR_NOT_FOUND, std::move(binary), std::move(loader));
    return;
  }

  // XXX(raggi): canonicalize url doesn't clean out invalid url chars or fail on
  // them (e.g. \n)
  const std::string canon_url = CanonicalizeURL(name);
  if (canon_url.empty()) {
    FXL_LOG(ERROR) << "Cannot resolve " << name
                   << " because the url could not be canonicalized";
    callback(ZX_ERR_INVALID_ARGS, std::move(binary), std::move(loader));
    return;
  }
  std::string scheme = GetSchemeFromURL(canon_url);

  const std::string launcher_type = scheme_map_.LookUp(scheme);
  if (launcher_type != "package") {
    FXL_LOG(ERROR) << "Cannot resolve non-packages";
    callback(ZX_ERR_NOT_FOUND, std::move(binary), std::move(loader));
    return;
  }

  loader_->LoadUrl(canon_url, [this, callback = std::move(callback)](
                                  fuchsia::sys::PackagePtr package) mutable {
    zx::vmo binary;
    fidl::InterfaceHandle<fuchsia::ldsvc::Loader> loader;
    if (!package) {
      callback(ZX_ERR_NOT_FOUND, std::move(binary), std::move(loader));
      return;
    }
    if (!package->data) {
      callback(ZX_ERR_NOT_FOUND, std::move(binary), std::move(loader));
      return;
    }
    binary.swap(package->data->vmo);

    if (!package->directory) {
      callback(ZX_ERR_NOT_FOUND, std::move(binary), std::move(loader));
      return;
    }
    fxl::UniqueFD dirfd =
        fsl::OpenChannelAsFileDescriptor(std::move(package->directory));

    zx::channel chan;
    if (DynamicLibraryLoader::Start(std::move(dirfd), &chan) != ZX_OK) {
      callback(ZX_ERR_INTERNAL, std::move(binary), std::move(loader));
      return;
    }
    loader.set_channel(std::move(chan));
    callback(ZX_OK, std::move(binary), std::move(loader));
  });
}

void Realm::CreateComponent(
    fuchsia::sys::LaunchInfo launch_info,
    fidl::InterfaceRequest<fuchsia::sys::ComponentController> controller,
    ComponentObjectCreatedCallback callback) {
  TRACE_DURATION("appmgr", "Realm::CreateComponent", "launch_info.url",
                 launch_info.url);
  ComponentRequestWrapper component_request(
      std::move(controller), MakeForwardingTerminationCallback());
  if (launch_info.additional_services &&
      launch_info.additional_services->host_directory) {
    FXL_LOG(ERROR) << "|host_directory| is not yet supported for "
                   << "CreateComponent. Use |provider| until it's supported.";
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::UNSUPPORTED);
    return;
  }
  if (launch_info.url.empty()) {
    FXL_LOG(ERROR) << "Cannot create application because launch_info contains"
                      " an empty url";
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::URL_INVALID);
    return;
  }
  std::string canon_url = CanonicalizeURL(launch_info.url);
  if (canon_url.empty()) {
    FXL_LOG(ERROR) << "Cannot run " << launch_info.url
                   << " because the url could not be canonicalized";
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::URL_INVALID);
    return;
  }
  launch_info.url = canon_url;
  std::string scheme = GetSchemeFromURL(canon_url);

  const std::string launcher_type = scheme_map_.LookUp(scheme);
  if (launcher_type == "") {
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::URL_INVALID);
  } else if (launcher_type == "package") {
    // "package" type doesn't use a runner.

    // launch_info is moved before LoadUrl() gets at its first argument.
    fidl::StringPtr url = launch_info.url;
    loader_->LoadUrl(url, [this, launch_info = std::move(launch_info),
                           component_request = std::move(component_request),
                           callback = std::move(callback)](
                              fuchsia::sys::PackagePtr package) mutable {
      if (package && package->directory) {
        CreateComponentFromPackage(std::move(package), std::move(launch_info),
                                   std::move(component_request),
                                   std::move(callback));
      } else {
        component_request.SetReturnValues(kComponentCreationFailed,
                                          TerminationReason::PACKAGE_NOT_FOUND);
      }
    });
  } else {
    // Component from scheme that maps to a runner.
    CreateComponentWithRunnerForScheme(launcher_type, std::move(launch_info),
                                       std::move(component_request),
                                       std::move(callback));
  }
}

void Realm::CreateShell(const std::string& path, zx::channel svc) {
  TRACE_DURATION("appmgr", "Realm::CreateShell", "path", path);
  if (!svc)
    return;

  SandboxMetadata sandbox;
  sandbox.AddFeature("shell");

  NamespaceBuilder builder;
  builder.AddServices(std::move(svc));
  builder.AddSandbox(sandbox, [this] { return OpenInfoDir(); });

  fsl::SizedVmo executable;
  if (!fsl::VmoFromFilename(path, &executable))
    return;

  zx::job child_job;
  zx_status_t status = zx::job::create(job_, 0u, &child_job);
  if (status != ZX_OK)
    return;

  fuchsia::sys::LaunchInfo launch_info;
  launch_info.url = path;
  zx::process process =
      CreateProcess(child_job, std::move(executable), path,
                    std::move(launch_info), zx::channel(), builder.Build());
}

std::unique_ptr<EnvironmentControllerImpl> Realm::ExtractChild(Realm* child) {
  auto it = children_.find(child);
  if (it == children_.end()) {
    return nullptr;
  }
  auto controller = std::move(it->second);

  // update hub
  hub_.RemoveRealm(child->HubInfo());

  children_.erase(it);
  return controller;
}

std::unique_ptr<ComponentControllerImpl> Realm::ExtractComponent(
    ComponentControllerImpl* controller) {
  auto it = applications_.find(controller);
  if (it == applications_.end()) {
    return nullptr;
  }
  auto application = std::move(it->second);

  // update hub
  hub_.RemoveComponent(application->HubInfo());

  applications_.erase(it);
  return application;
}

void Realm::AddBinding(
    fidl::InterfaceRequest<fuchsia::sys::Environment> environment) {
  default_namespace_->AddBinding(std::move(environment));
}

void Realm::CreateComponentWithRunnerForScheme(
    std::string runner_url, fuchsia::sys::LaunchInfo launch_info,
    ComponentRequestWrapper component_request,
    ComponentObjectCreatedCallback callback) {
  TRACE_DURATION("appmgr", "Realm::CreateComponentWithRunnerForScheme",
                 "runner_url", runner_url, "launch_info.url", launch_info.url);

  fuchsia::sys::Package package;
  package.resolved_url = launch_info.url;

  fuchsia::sys::StartupInfo startup_info;
  startup_info.launch_info = std::move(launch_info);
  NamespaceBuilder builder;
  startup_info.flat_namespace = builder.BuildForRunner();

  auto* runner = GetOrCreateRunner(runner_url);
  if (runner == nullptr) {
    FXL_LOG(ERROR) << "Cannot create " << runner_url << " to run "
                   << launch_info.url;
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::RUNNER_FAILED);
    return;
  }

  fxl::RefPtr<Namespace> ns = fxl::MakeRefCounted<Namespace>(
      default_namespace_, this, nullptr, nullptr);

  fidl::InterfaceRequest<fuchsia::sys::ComponentController> controller;
  TerminationCallback termination_callback;
  component_request.Extract(&controller, &termination_callback);
  runner->StartComponent(std::move(package), std::move(startup_info),
                         std::move(ns), std::move(controller),
                         std::move(termination_callback));
}

void Realm::CreateComponentFromPackage(
    fuchsia::sys::PackagePtr package, fuchsia::sys::LaunchInfo launch_info,
    ComponentRequestWrapper component_request,
    ComponentObjectCreatedCallback callback) {
  TRACE_DURATION("appmgr", "Realm::CreateComponentFromPackage",
                 "package.resolved_url", package->resolved_url,
                 "launch_info.url", launch_info.url);
  fxl::UniqueFD fd =
      fsl::OpenChannelAsFileDescriptor(std::move(package->directory));

  // Parse cmx manifest file, if it's there.
  CmxMetadata cmx;
  std::string cmx_path;
  FuchsiaPkgUrl fp;
  bool is_fuchsia_pkg_url = false;
  if (fp.Parse(package->resolved_url)) {
    if (!fp.resource_path().empty()) {
      // If the url has a resource, assume that's the cmx.
      cmx_path = fp.resource_path();

      // The URL is fuchsia-pkg iff it has a resource.
      // TODO(CF-156): Remove this logic once all URLs are fuchsia-pkg.
      is_fuchsia_pkg_url = true;
    } else {
      // It's possible the url does not have a resource, in which case either
      // the cmx exists at meta/<package_name.cmx> or it does not exist.
      cmx_path = fp.GetDefaultComponentCmxPath();
    }
  } else {
    FXL_LOG(ERROR) << "invalid component url: " << package->resolved_url;
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::INTERNAL_ERROR);
    return;
  }
  TRACE_DURATION_BEGIN("appmgr", "Realm::CreateComponentFromPackage:IsFileAt",
                       "cmx_path", cmx_path);
  if (!cmx_path.empty() && files::IsFileAt(fd.get(), cmx_path)) {
    TRACE_DURATION_END("appmgr", "Realm::CreateComponentFromPackage:IsFileAt");
    json::JSONParser json_parser;
    if (!cmx.ParseFromFileAt(fd.get(), cmx_path, &json_parser)) {
      FXL_LOG(ERROR) << "cmx file failed to parse: " << json_parser.error_str();
      component_request.SetReturnValues(kComponentCreationFailed,
                                        TerminationReason::INTERNAL_ERROR);
      return;
    }
  } else {
    TRACE_DURATION_END("appmgr", "Realm::CreateComponentFromPackage:IsFileAt");
    FXL_LOG(ERROR) << "Component " << package->resolved_url
                   << " does not have a component manifest (a.k.a. cmx file)! "
                   << "Please add a cmx file to your component. "
                   << "https://fuchsia.googlesource.com/fuchsia/+/master/docs/the-book/"
                   << "package_metadata.md#Component-manifest.";
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::INTERNAL_ERROR);
    return;
  }

  // TODO(CF-156): Remove this logic once all URLs are fuchsia-pkg.
  if (!is_fuchsia_pkg_url) {
    FXL_LOG(ERROR)
        << "Component " << fp.GetDefaultComponentName()
        << " was launched without using fuchsia-pkg URLs! Use "
        << package->resolved_url << "#" << fp.GetDefaultComponentCmxPath()
        << " instead. See https://fuchsia.googlesource.com/fuchsia/+/master/docs/"
        << "glossary.md#fuchsia_pkg-url for more information. The component "
        << "was not whitelisted to launch with bare package URLs, and it "
        << "cannot be launched!";
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::INTERNAL_ERROR);
    return;
  }

  RuntimeMetadata runtime;
  std::string runtime_parse_error;
  // If meta/*.cmx has runtime data, get it.
  if (!cmx.runtime_meta().IsNull()) {
    runtime = cmx.runtime_meta();
  }

  fsl::SizedVmo app_data;
  std::string app_argv0;
  fidl::VectorPtr<fuchsia::sys::ProgramMetadata> program_metadata;
  const ProgramMetadata program = cmx.program_meta();
  if (runtime.IsNull()) {
    // If we cannot parse a runtime from either .cmx or deprecated_runtime, then
    // we fall back to the default runner, which is running an ELF binary or
    // shell script.
    const std::string bin_path =
        program.IsBinaryNull() ? kAppPath : program.binary();

    launch_info.arguments->insert(launch_info.arguments->begin(),
                                  program.args().begin(), program.args().end());

    app_argv0 = fxl::Concatenate({kAppArgv0Prefix, bin_path});
    TRACE_DURATION_BEGIN("appmgr",
                         "Realm::CreateComponentFromPackage:VmoFromFilenameAt",
                         "bin_path", bin_path);
    VmoFromFilenameAt(fd.get(), bin_path, &app_data);
    TRACE_DURATION_END("appmgr",
                       "Realm::CreateComponentFromPackage:VmoFromFilenameAt");
    if (!app_data) {
      FXL_LOG(ERROR) << "component '" << package->resolved_url
                     << "' has neither runner (error: '" << runtime_parse_error
                     << "') nor elf binary: '" << bin_path << "'";
      component_request.SetReturnValues(kComponentCreationFailed,
                                        TerminationReason::INTERNAL_ERROR);
      return;
    }
  } else {
    // Read 'data' path from cmx, or assume to be /pkg/data/<component-name>.
    std::string data_path = program.IsDataNull()
                                ? kDataPathPrefix + fp.GetDefaultComponentName()
                                : program.data();
    // Pass a {"data", "data/<component-name>"} pair through StartupInfo, so
    // components can identify their directory under /pkg/data.
    program_metadata = fidl::VectorPtr<fuchsia::sys::ProgramMetadata>::New(1);
    fuchsia::sys::ProgramMetadata pg;
    pg.key = kDataKey;
    pg.value = data_path;
    program_metadata->at(0) = pg;
  }

  ExportedDirType exported_dir_layout =
      files::IsFileAt(fd.get(), kLegacyFlatExportedDirPath)
          ? ExportedDirType::kLegacyFlatLayout
          : ExportedDirType::kPublicDebugCtrlLayout;
  // TODO(abarth): We shouldn't need to clone the channel here. Instead, we
  // should be able to tear down the file descriptor in a way that gives us
  // the channel back.
  zx::channel pkg = fsl::CloneChannelFromFileDescriptor(fd.get());
  zx::channel loader_service;
  if (DynamicLibraryLoader::Start(std::move(fd), &loader_service) != ZX_OK) {
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::INTERNAL_ERROR);
    return;
  }

  // Note that |builder| is only used in the else block below. It is left here
  // because we would like to use it everywhere once US-313 is fixed.
  NamespaceBuilder builder;
  builder.AddPackage(std::move(pkg));

  // If meta/*.cmx exists, attempt to read sandbox data from it.
  const std::vector<std::string>* service_whitelist = nullptr;
  if (!cmx.sandbox_meta().IsNull()) {
    const auto& sandbox = cmx.sandbox_meta();
    service_whitelist = &sandbox.services();

    // If an app has the "shell" feature, then we use the libraries from the
    // system rather than from the package because programs spawned from the
    // shell will need the system-provided libraries to run.
    if (sandbox.HasFeature("shell")) {
      loader_service.reset();
    }

    builder.AddSandbox(sandbox, [this] { return OpenInfoDir(); });
  }

  fxl::RefPtr<Namespace> ns = fxl::MakeRefCounted<Namespace>(
      default_namespace_, this, std::move(launch_info.additional_services),
      service_whitelist);
  ns->set_component_url(launch_info.url);
  zx::channel svc = ns->OpenServicesAsDirectory();
  if (!svc) {
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::INTERNAL_ERROR);
    return;
  }
  builder.AddServices(std::move(svc));

  // Add the custom namespace.
  // Note that this must be the last |builder| step adding entries to the
  // namespace so that we can filter out entries already added in previous
  // steps.
  builder.AddFlatNamespace(std::move(launch_info.flat_namespace));

  if (runtime.IsNull()) {
    // Use the default runner: ELF binaries.
    CreateElfBinaryComponentFromPackage(
        std::move(launch_info), app_data, app_argv0, exported_dir_layout,
        std::move(loader_service), builder.Build(),
        std::move(component_request), std::move(ns), std::move(callback));
  } else {
    // Use other component runners.
    CreateRunnerComponentFromPackage(
        std::move(package), std::move(launch_info), runtime,
        builder.BuildForRunner(), std::move(component_request), std::move(ns),
        std::move(program_metadata));
  }
}

void Realm::CreateElfBinaryComponentFromPackage(
    fuchsia::sys::LaunchInfo launch_info, fsl::SizedVmo& app_data,
    const std::string& app_argv0, ExportedDirType exported_dir_layout,
    zx::channel loader_service, fdio_flat_namespace_t* flat,
    ComponentRequestWrapper component_request, fxl::RefPtr<Namespace> ns,
    ComponentObjectCreatedCallback callback) {
  TRACE_DURATION("appmgr", "Realm::CreateElfBinaryComponentFromPackage",
                 "launch_info.url", launch_info.url);

  zx::job child_job;
  zx_status_t status = zx::job::create(job_, 0u, &child_job);
  if (status != ZX_OK)
    return;

  const std::string args = Util::GetArgsString(launch_info.arguments);
  const std::string url = launch_info.url;  // Keep a copy before moving it.
  auto channels = Util::BindDirectory(&launch_info);
  zx::process process =
      CreateProcess(child_job, std::move(app_data), app_argv0,
                    std::move(launch_info), std::move(loader_service), flat);

  if (process) {
    fidl::InterfaceRequest<fuchsia::sys::ComponentController> controller;
    TerminationCallback termination_callback;
    component_request.Extract(&controller, &termination_callback);
    auto application = std::make_unique<ComponentControllerImpl>(
        std::move(controller), this, std::move(child_job), std::move(process),
        url, std::move(args), Util::GetLabelFromURL(url), std::move(ns),
        exported_dir_layout, std::move(channels.exported_dir),
        std::move(channels.client_request), std::move(termination_callback));
    // update hub
    hub_.AddComponent(application->HubInfo());
    ComponentControllerImpl* key = application.get();
    if (callback != nullptr) {
      callback(key);
    }
    applications_.emplace(key, std::move(application));
  }
}

void Realm::CreateRunnerComponentFromPackage(
    fuchsia::sys::PackagePtr package, fuchsia::sys::LaunchInfo launch_info,
    RuntimeMetadata& runtime, fuchsia::sys::FlatNamespace flat,
    ComponentRequestWrapper component_request, fxl::RefPtr<Namespace> ns,
    fidl::VectorPtr<fuchsia::sys::ProgramMetadata> program_metadata) {
  TRACE_DURATION("appmgr", "Realm::CreateRunnerComponentFromPackage",
                 "package.resolved_url", package->resolved_url,
                 "launch_info.url", launch_info.url);

  fuchsia::sys::Package inner_package;
  inner_package.resolved_url = package->resolved_url;

  fuchsia::sys::StartupInfo startup_info;
  startup_info.launch_info = std::move(launch_info);
  startup_info.flat_namespace = std::move(flat);
  startup_info.program_metadata = std::move(program_metadata);

  auto* runner = GetOrCreateRunner(runtime.runner());
  if (runner == nullptr) {
    FXL_LOG(ERROR) << "Cannot create " << runner << " to run "
                   << launch_info.url;
    component_request.SetReturnValues(kComponentCreationFailed,
                                      TerminationReason::INTERNAL_ERROR);
    return;
  }

  fidl::InterfaceRequest<fuchsia::sys::ComponentController> controller;
  TerminationCallback termination_callback;
  component_request.Extract(&controller, &termination_callback);
  runner->StartComponent(std::move(inner_package), std::move(startup_info),
                         std::move(ns), std::move(controller),
                         std::move(termination_callback));
}

RunnerHolder* Realm::GetOrCreateRunner(const std::string& runner) {
  // We create the entry in |runners_| before calling ourselves
  // recursively to detect cycles.
  auto found = GetRunnerRecursive(runner);
  if (found) {
    return found;
  }
  auto result = runners_.emplace(runner, nullptr);
  if (result.second) {
    Services runner_services;
    fuchsia::sys::ComponentControllerPtr runner_controller;
    fuchsia::sys::LaunchInfo runner_launch_info;
    runner_launch_info.url = runner;
    runner_launch_info.directory_request = runner_services.NewRequest();
    result.first->second = std::make_unique<RunnerHolder>(
        std::move(runner_services), std::move(runner_controller),
        std::move(runner_launch_info), this,
        [this, runner] { runners_.erase(runner); });

  } else if (!result.first->second) {
    // There was a cycle in the runner graph.
    FXL_LOG(ERROR) << "Detected a cycle in the runner graph for " << runner
                   << ".";
    return nullptr;
  }

  return result.first->second.get();
}

RunnerHolder* Realm::GetRunnerRecursive(const std::string& runner) const {
  auto it = runners_.find(runner);
  if (it != runners_.end()) {
    return it->second.get();
  } else if (parent_ && allow_parent_runners_) {
    return parent_->GetRunnerRecursive(runner);
  }

  return nullptr;
}

zx_status_t Realm::BindSvc(zx::channel channel) {
  Realm* root_realm = this;
  while (root_realm->parent() != nullptr) {
    root_realm = root_realm->parent();
  }
  return fdio_service_clone_to(root_realm->svc_channel_client_.get(),
                               channel.release());
}

}  // namespace component
