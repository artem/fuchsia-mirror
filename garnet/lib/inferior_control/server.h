// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#include <memory>

#include <lib/async-loop/cpp/loop.h>
#include <src/lib/fxl/macros.h>
#include <src/lib/fxl/strings/string_view.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/zx/job.h>
#include <src/lib/files/unique_fd.h>

#include "garnet/lib/process/process_builder.h"

#include "delegate.h"
#include "exception_port.h"
#include "io_loop.h"
#include "process.h"
#include "thread.h"

namespace inferior_control {

// Server implements the main loop and handles client operations and inferior
// events (exceptions, etc).
//
// TODO(dje): It might be useful to separate out |Delegate| here.
// It is used as a "mixin" to simplify early clients.
//
// NOTE: This class is generally not thread safe. Care must be taken when
// calling methods such as set_current_process() and SetCurrentThread()
// which modify its internal state.
class Server : public Delegate {
 public:
  // Starts the main loop, and returns when the main loop exits.
  // Returns true if the main loop exits cleanly, or false in the case of an
  // error.
  // TODO(armansito): More clearly define the error scenario.
  // TODO(dje): This mightn't need to be virtual, but it provides consistency
  // among the uses.
  virtual bool Run() = 0;

  zx_handle_t job_for_search() const { return job_for_search_.get(); }
  zx_handle_t job_for_launch() const { return job_for_launch_.get(); }

  // Returns a raw pointer to the current inferior. The instance pointed to by
  // the returned pointer is owned by this Server instance and should not be
  // deleted.
  Process* current_process() const { return current_process_.get(); }

  // Sets the current process. This cleans up the current process (if any) and
  // takes ownership of |process|.
  void set_current_process(Process* process) { current_process_.reset(process); }

  // Returns a raw pointer to the current thread.
  Thread* current_thread() const { return current_thread_.get(); }

  // Assigns the current thread.
  void SetCurrentThread(Thread* thread);

  // Returns a mutable reference to the main message loop. The returned instance
  // is owned by this Server instance and should not be deleted.
  async::Loop& message_loop() { return message_loop_; }

  // Returns a mutable reference to the exception port. The returned instance is
  // owned by this Server instance and should not be deleted.
  ExceptionPort& exception_port() { return exception_port_; }

  // Accessor for the exception port's handle.
  // TODO(PT-105): Delete when exceptions have handles themselves.
  zx_handle_t exception_port_handle() const { return exception_port_.handle(); }

  // Utility to create a new inferior via |process::ProcessBuilder|.
  // Returns false if there is an error.
  // |*out_builder| is returned in case the caller wants to add anything
  // further to the new process. A typical example is to call
  // |builder->CloneAll()| as no cloning is done yet. |*out_builder| is
  // intended to be passed to |Process::InitializeFromBuilder|.
  // TODO(dje): InferiorManager class to manage multiple inferiors, and
  // creating them.
  bool CreateProcessViaBuilder(const std::string& path, const debugger_utils::Argv& argv,
                               std::unique_ptr<process::ProcessBuilder>* out_builder);

  // Return a handle to a running process that can be used for debugging.
  // Returns an invalid object if the process is not found or the handle is
  // unobtainable.
  // |pid| is looked up via |job_to_search()|.
  zx::process FindProcess(zx_koid_t pid);

  // Call this to schedule termination of the server.
  // N.B. The Server will exit its main loop asynchronously so any
  // subsequently posted tasks will be dropped.
  void PostQuitMessageLoop(bool status);

  // Register an async-wait for |thread| on the exception port loop.
  void WaitAsync(Thread* thread);

 private:
  void OnProcessException(const zx_port_packet_t& packet);
  void OnProcessSignal(const zx_port_packet_t& packet);

  // Returns a borrowed handle of the job whose processes we may attach to.
  // If this is ZX_HANDLE_INVALID then we may not attach to any process.
  zx::job job_for_search_;

  // Returns a borrowed handle of the job where processes are launched.
  // This is not necessarily |job_for_search_|, we may be able to attach to
  // any process but not necessarily want to start new processes in the root
  // job itself. If this is ZX_HANDLE_INVALID then starting new processes is
  // not allowed.
  zx::job job_for_launch_;

  // The services to pass to processes created with
  // |CreateProcessViaBuilder()|.
  std::shared_ptr<sys::ServiceDirectory> services_;

 protected:
  Server(zx::job job_for_search, zx::job job_for_launch,
         std::shared_ptr<sys::ServiceDirectory> services);

  virtual ~Server();

  // Sets the run status and quits the main message loop.
  void QuitMessageLoop(bool status);

  // The current thread under debug. We only keep a weak pointer here, since the
  // instance itself is owned by a Process and may get removed.
  fxl::WeakPtr<Thread> current_thread_;

  // The main loop.
  async::Loop message_loop_;

  // The ExceptionPort used by inferiors to receive exceptions.
  // (This is declared after |message_loop_| since that needs to have been
  // created before this can be initialized).
  ExceptionPort exception_port_;

  // Strong pointer to the current inferior process that is being debugged.
  // NOTE: This must be declared after |exception_port_| above, since the
  // process may do work in its destructor to detach itself from
  // |exception_port_|.
  std::unique_ptr<Process> current_process_;

  // Stores the global error state. This is used to determine the return value
  // for "Run()" when |message_loop_| exits.
  bool run_status_;

 private:
  FXL_DISALLOW_COPY_AND_ASSIGN(Server);
};

// Same as Server, but provides I/O support.
// An example use-case is debugserver for gdb.
class ServerWithIO : public Server, public IOLoop::Delegate {
 protected:
  ServerWithIO(zx::job job_for_search, zx::job job_for_launch,
               std::shared_ptr<sys::ServiceDirectory> services);
  virtual ~ServerWithIO();

  // The IOLoop used for blocking I/O operations over |client_sock_|.
  // |message_loop_| and |client_sock_| both MUST outlive |io_loop_|. We take
  // care to clean it up in the destructor.
  std::unique_ptr<IOLoop> io_loop_;

  // File descriptor for the socket (or terminal) used for communication.
  // TODO(dje): Rename from *sock* after things are working.
  fbl::unique_fd client_sock_;
};

}  // namespace inferior_control
