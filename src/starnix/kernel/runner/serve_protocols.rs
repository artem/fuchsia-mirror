// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::{endpoints::ServerEnd, AsHandleRef};
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_element as felement;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fidl_fuchsia_starnix_device as fstardevice;
use fuchsia_async::{
    DurationExt, {self as fasync},
};
use futures::{channel::oneshot, AsyncReadExt, AsyncWriteExt, TryStreamExt};
use starnix_core::{
    execution::execute_task_with_prerun_result,
    fs::{devpts::create_main_and_replica, fuchsia::create_fuchsia_pipe},
    task::{CurrentTask, ExitStatus, Kernel},
    vfs::{
        buffers::{VecInputBuffer, VecOutputBuffer},
        file_server::serve_file_at,
        socket::VsockSocket,
        FdFlags, FileHandle,
    },
};
use starnix_logging::{log_error, log_warn};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, TaskRelease};
use starnix_uapi::{open_flags::OpenFlags, uapi};
use std::{ffi::CString, ops::DerefMut, sync::Arc};

use super::start_component;

pub fn expose_root<L>(
    locked: &mut Locked<'_, L>,
    system_task: &CurrentTask,
    server_end: ServerEnd<fio::DirectoryMarker>,
) -> Result<(), Error>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<DeviceOpen>,
{
    let root_file = system_task.open_file(locked, "/".into(), OpenFlags::RDONLY)?;
    serve_file_at(locked, server_end.into_channel().into(), system_task, &root_file)?;
    Ok(())
}

pub async fn serve_component_runner(
    request_stream: frunner::ComponentRunnerRequestStream,
    system_task: &CurrentTask,
) -> Result<(), Error> {
    request_stream
        .try_for_each_concurrent(None, |event| async {
            match event {
                frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                    if let Err(e) = start_component(start_info, controller, system_task).await {
                        log_error!("failed to start component: {:?}", e);
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(Error::from)
}

fn to_winsize(window_size: Option<fstarcontainer::ConsoleWindowSize>) -> uapi::winsize {
    window_size
        .map(|window_size| uapi::winsize {
            ws_row: window_size.rows,
            ws_col: window_size.cols,
            ws_xpixel: window_size.x_pixels,
            ws_ypixel: window_size.y_pixels,
        })
        .unwrap_or(uapi::winsize::default())
}

async fn spawn_console<L>(
    locked: &mut Locked<'_, L>,
    kernel: &Arc<Kernel>,
    payload: fstarcontainer::ControllerSpawnConsoleRequest,
) -> Result<Result<u8, fstarcontainer::SpawnConsoleError>, Error>
where
    L: LockBefore<TaskRelease>,
{
    if let (Some(console_in), Some(console_out), Some(binary_path)) =
        (payload.console_in, payload.console_out, payload.binary_path)
    {
        let binary_path = CString::new(binary_path)?;
        let argv = payload
            .argv
            .unwrap_or(vec![])
            .into_iter()
            .map(CString::new)
            .collect::<Result<Vec<_>, _>>()?;
        let environ = payload
            .environ
            .unwrap_or(vec![])
            .into_iter()
            .map(CString::new)
            .collect::<Result<Vec<_>, _>>()?;
        let window_size = to_winsize(payload.window_size);
        let current_task = CurrentTask::create_init_child_process(locked, kernel, &binary_path)?;
        let (sender, receiver) = oneshot::channel();
        let pty = execute_task_with_prerun_result(
            current_task,
            move |locked, current_task| {
                let executable = current_task.open_file(
                    locked,
                    binary_path.as_bytes().into(),
                    OpenFlags::RDONLY,
                )?;
                current_task.exec(locked, executable, binary_path, argv, environ)?;
                let (pty, pts) = create_main_and_replica(locked, &current_task, window_size)?;
                let fd_flags = FdFlags::empty();
                assert_eq!(0, current_task.add_file(pts.clone(), fd_flags)?.raw());
                assert_eq!(1, current_task.add_file(pts.clone(), fd_flags)?.raw());
                assert_eq!(2, current_task.add_file(pts, fd_flags)?.raw());
                Ok(pty)
            },
            move |result| {
                let _ = match result {
                    Ok(ExitStatus::Exit(exit_code)) => sender.send(Ok(exit_code)),
                    _ => sender.send(Err(fstarcontainer::SpawnConsoleError::Canceled)),
                };
            },
            None,
        )?;
        let _ = forward_to_pty(kernel, console_in, console_out, pty).map_err(|e| {
            log_error!("failed to forward to terminal {:?}", e);
        });

        Ok(receiver.await?)
    } else {
        Ok(Err(fstarcontainer::SpawnConsoleError::InvalidArgs))
    }
}

pub async fn serve_container_controller(
    request_stream: fstarcontainer::ControllerRequestStream,
    system_task: &CurrentTask,
) -> Result<(), Error> {
    request_stream
        .map_err(Error::from)
        .try_for_each_concurrent(None, |event| async {
            match event {
                fstarcontainer::ControllerRequest::VsockConnect {
                    payload:
                        fstarcontainer::ControllerVsockConnectRequest { port, bridge_socket, .. },
                    ..
                } => {
                    let Some(port) = port else {
                        log_warn!("vsock connection missing port");
                        return Ok(());
                    };
                    let Some(bridge_socket) = bridge_socket else {
                        log_warn!("vsock connection missing bridge_socket");
                        return Ok(());
                    };
                    connect_to_vsock(port, bridge_socket, system_task).await.unwrap_or_else(|e| {
                        log_error!("failed to connect to vsock {:?}", e);
                    });
                }
                fstarcontainer::ControllerRequest::SpawnConsole { payload, responder } => {
                    responder.send(
                        spawn_console(
                            system_task.kernel().kthreads.unlocked_for_async().deref_mut(),
                            system_task.kernel(),
                            payload,
                        )
                        .await?,
                    )?;
                }
                fstarcontainer::ControllerRequest::GetVmoReferences { payload, responder } => {
                    if let Some(koid) = payload.koid {
                        let thread_groups = system_task.kernel().pids.read().get_thread_groups();
                        let mut results = vec![];
                        for thread_group in thread_groups {
                            if let Some(leader) =
                                system_task.get_task(thread_group.leader).upgrade()
                            {
                                let fds = leader.files.get_all_fds();
                                for fd in fds {
                                    if let Ok(file) = leader.files.get(fd) {
                                        if let Ok(vmo) = file.get_vmo(
                                            system_task,
                                            None,
                                            starnix_core::mm::ProtectionFlags::READ,
                                        ) {
                                            let vmo_koid =
                                                vmo.info().expect("Failed to get vmo info").koid;
                                            if vmo_koid.raw_koid() == koid {
                                                let process_name = thread_group
                                                    .process
                                                    .get_name()
                                                    .unwrap_or_default();
                                                results.push(fstarcontainer::VmoReference {
                                                    process_name: Some(
                                                        process_name
                                                            .into_string()
                                                            .unwrap_or_default(),
                                                    ),
                                                    pid: Some(leader.get_pid() as u64),
                                                    fd: Some(fd.raw()),
                                                    koid: Some(koid),
                                                    ..Default::default()
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let _ =
                            responder.send(&fstarcontainer::ControllerGetVmoReferencesResponse {
                                references: Some(results),
                                ..Default::default()
                            });
                    }
                }
                fstarcontainer::ControllerRequest::_UnknownMethod { .. } => (),
            }
            Ok(())
        })
        .await
}

async fn connect_to_vsock(
    port: u32,
    bridge_socket: fidl::Socket,
    system_task: &CurrentTask,
) -> Result<(), Error> {
    let socket = loop {
        if let Ok(socket) = system_task.kernel().default_abstract_vsock_namespace.lookup(&port) {
            break socket;
        };
        fasync::Timer::new(fasync::Duration::from_millis(100).after_now()).await;
    };

    let pipe =
        create_fuchsia_pipe(system_task, bridge_socket, OpenFlags::RDWR | OpenFlags::NONBLOCK)?;
    socket.downcast_socket::<VsockSocket>().unwrap().remote_connection(
        &socket,
        system_task,
        pipe,
    )?;

    Ok(())
}

fn forward_to_pty(
    kernel: &Kernel,
    console_in: fidl::Socket,
    console_out: fidl::Socket,
    pty: FileHandle,
) -> Result<(), Error> {
    // Matches fuchsia.io.Transfer capacity, somewhat arbitrarily.
    const BUFFER_CAPACITY: usize = 8192;

    let mut rx = fuchsia_async::Socket::from_socket(console_in);
    let mut tx = fuchsia_async::Socket::from_socket(console_out);
    let pty_sink = pty.clone();
    kernel.kthreads.spawn({
        move |locked, current_task| {
            let _result: Result<(), Error> =
                fasync::LocalExecutor::new().run_singlethreaded(async {
                    let mut buffer = vec![0u8; BUFFER_CAPACITY];
                    loop {
                        let bytes = rx.read(&mut buffer[..]).await?;
                        if bytes == 0 {
                            return Ok(());
                        }
                        pty_sink.write(
                            locked,
                            current_task,
                            &mut VecInputBuffer::new(&buffer[..bytes]),
                        )?;
                    }
                });
        }
    });

    let pty_source = pty;
    kernel.kthreads.spawn({
        move |mut locked, current_task| {
            let _result: Result<(), Error> =
                fasync::LocalExecutor::new().run_singlethreaded(async {
                    let mut buffer = VecOutputBuffer::new(BUFFER_CAPACITY);
                    loop {
                        buffer.reset();
                        let bytes = pty_source.read(&mut locked, current_task, &mut buffer)?;
                        if bytes == 0 {
                            return Ok(());
                        }
                        tx.write_all(buffer.data()).await?;
                    }
                });
        }
    });

    Ok(())
}

pub async fn serve_graphical_presenter(
    request_stream: felement::GraphicalPresenterRequestStream,
    kernel: &Kernel,
) -> Result<(), Error> {
    request_stream
        .try_for_each_concurrent(None, |event| async {
            match event {
                felement::GraphicalPresenterRequest::PresentView {
                    view_spec,
                    annotation_controller: _,
                    view_controller_request: _,
                    responder,
                } => match view_spec.viewport_creation_token {
                    Some(token) => {
                        kernel.framebuffer.present_view(token);
                        let _ = responder.send(Ok(()));
                    }
                    None => {
                        let _ = responder.send(Err(felement::PresentViewError::InvalidArgs));
                    }
                },
            }
            Ok(())
        })
        .await
        .map_err(Error::from)
}

pub async fn serve_sync_fence_registry(
    request_stream: fstardevice::SyncFenceRegistryRequestStream,
    kernel: &Kernel,
) -> Result<(), Error> {
    let sync_fence_registry = kernel.sync_fence_registry.clone();
    kernel.kthreads.spawner().spawn(|_, _| {
        let mut executor = fasync::LocalExecutor::new();
        let _ = executor.run_singlethreaded(async move {
            request_stream
                .try_for_each_concurrent(None, |event| async {
                    match event {
                        fstardevice::SyncFenceRegistryRequest::CreateSyncFences {
                            num_fences,
                            responder,
                        } => {
                            let (fence_keys, events) =
                                sync_fence_registry.create_sync_fences(num_fences);
                            let _ = responder.send(fence_keys, events);
                        }

                        fstardevice::SyncFenceRegistryRequest::RegisterSignaledEvent {
                            fence_key,
                            event,
                            ..
                        } => {
                            sync_fence_registry.register_signaled_event(fence_key, event);
                        }
                    }
                    Ok(())
                })
                .await
                .map_err(Error::from)
        });
    });

    Ok(())
}
