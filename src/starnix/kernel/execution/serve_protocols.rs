// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_element as felement;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fuchsia_async::{self as fasync, DurationExt};
use fuchsia_zircon as zx;
use futures::{AsyncReadExt, AsyncWriteExt, TryStreamExt};
use std::{ffi::CString, sync::Arc};

use crate::{
    execution::{execute_task, Container},
    fs::{
        buffers::*, devpts::create_main_and_replica, file_server::serve_file_at,
        fuchsia::create_fuchsia_pipe, socket::VsockSocket, *,
    },
    logging::log_error,
    task::*,
    types::*,
};

use super::*;

pub fn expose_root(
    container: &Container,
    server_end: ServerEnd<fio::DirectoryMarker>,
) -> Result<(), Error> {
    let system_task = container.kernel.kthreads.system_task();
    let root_file = system_task.open_file(b"/", OpenFlags::RDONLY)?;
    serve_file_at(server_end.into_channel().into(), system_task, &root_file)?;
    Ok(())
}

pub async fn serve_component_runner(
    request_stream: frunner::ComponentRunnerRequestStream,
    container: &Container,
) -> Result<(), Error> {
    request_stream
        .try_for_each_concurrent(None, |event| async {
            match event {
                frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                    if let Err(e) = start_component(start_info, controller, container).await {
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

pub async fn serve_container_controller(
    request_stream: fstarcontainer::ControllerRequestStream,
    container: &Container,
) -> Result<(), Error> {
    request_stream
        .map_err(Error::from)
        .try_for_each_concurrent(None, |event| async {
            match event {
                fstarcontainer::ControllerRequest::VsockConnect { port, bridge_socket, .. } => {
                    connect_to_vsock(port, bridge_socket, container).await.unwrap_or_else(|e| {
                        log_error!("failed to connect to vsock {:?}", e);
                    });
                }
                fstarcontainer::ControllerRequest::SpawnConsole { payload, responder } => {
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
                        match create_task_with_pty(
                            &container.kernel,
                            binary_path,
                            argv,
                            environ,
                            to_winsize(payload.window_size),
                        ) {
                            Ok((current_task, pty)) => {
                                execute_task(current_task, move |result| {
                                    let _ = match result {
                                        Ok(ExitStatus::Exit(exit_code)) => {
                                            responder.send(Ok(exit_code))
                                        }
                                        _ => responder.send(Err(zx::Status::CANCELED.into_raw())),
                                    };
                                });
                                let _ = forward_to_pty(container, console_in, console_out, pty)
                                    .map_err(|e| {
                                        log_error!("failed to forward to terminal {:?}", e);
                                    });
                            }
                            Err(errno) => {
                                log_error!("failed to create task with pty {:?}", errno);
                                responder.send(Err(zx::Status::IO.into_raw()))?;
                            }
                        }
                    } else {
                        responder.send(Err(zx::Status::INVALID_ARGS.into_raw()))?;
                    }
                }
            }
            Ok(())
        })
        .await
}

async fn connect_to_vsock(
    port: u32,
    bridge_socket: fidl::Socket,
    container: &Container,
) -> Result<(), Error> {
    let socket = loop {
        if let Ok(socket) = container.kernel.default_abstract_vsock_namespace.lookup(&port) {
            break socket;
        };
        fasync::Timer::new(fasync::Duration::from_millis(100).after_now()).await;
    };

    let system_task = container.kernel.kthreads.system_task();
    let pipe =
        create_fuchsia_pipe(system_task, bridge_socket, OpenFlags::RDWR | OpenFlags::NONBLOCK)?;
    socket.downcast_socket::<VsockSocket>().unwrap().remote_connection(
        &socket,
        system_task,
        pipe,
    )?;

    Ok(())
}

fn create_task_with_pty(
    kernel: &Arc<Kernel>,
    binary_path: CString,
    argv: Vec<CString>,
    environ: Vec<CString>,
    window_size: uapi::winsize,
) -> Result<(CurrentTask, FileHandle), Errno> {
    let mut current_task = Task::create_init_child_process(kernel, &binary_path)?;
    let pty = release_on_error!(current_task, &(), {
        let executable = current_task.open_file(binary_path.as_bytes(), OpenFlags::RDONLY)?;
        current_task.exec(executable, binary_path, argv, environ)?;
        let (pty, pts) = create_main_and_replica(&current_task, window_size)?;
        let fd_flags = FdFlags::empty();
        assert_eq!(0, current_task.add_file(pts.clone(), fd_flags)?.raw());
        assert_eq!(1, current_task.add_file(pts.clone(), fd_flags)?.raw());
        assert_eq!(2, current_task.add_file(pts, fd_flags)?.raw());
        Ok(pty)
    });
    Ok((current_task, pty))
}

fn forward_to_pty(
    container: &Container,
    console_in: fidl::Socket,
    console_out: fidl::Socket,
    pty: FileHandle,
) -> Result<(), Error> {
    // Matches fuchsia.io.Transfer capacity, somewhat arbitrarily.
    const BUFFER_CAPACITY: usize = 8192;

    let mut rx = fuchsia_async::Socket::from_socket(console_in)?;
    let mut tx = fuchsia_async::Socket::from_socket(console_out)?;
    let kernel = &container.kernel;
    let pty_sink = pty.clone();
    kernel.kthreads.pool.dispatch({
        let read_task = container.kernel.kthreads.new_system_thread()?;
        move || {
            let _result = fasync::LocalExecutor::new().run_singlethreaded(async {
                async_release_after!(read_task, &(), || -> Result<(), Error> {
                    let mut buffer = vec![0u8; BUFFER_CAPACITY];
                    loop {
                        let bytes = rx.read(&mut buffer[..]).await?;
                        if bytes == 0 {
                            return Ok(());
                        }
                        pty_sink.write(&read_task, &mut VecInputBuffer::new(&buffer[..bytes]))?;
                    }
                })
            });
        }
    });

    let pty_source = pty;
    kernel.kthreads.pool.dispatch({
        let write_task = container.kernel.kthreads.new_system_thread()?;
        move || {
            let _result = fasync::LocalExecutor::new().run_singlethreaded(async {
                async_release_after!(write_task, &(), || -> Result<(), Error> {
                    let mut buffer = VecOutputBuffer::new(BUFFER_CAPACITY);
                    loop {
                        buffer.reset();
                        let bytes = pty_source.read(&write_task, &mut buffer)?;
                        if bytes == 0 {
                            return Ok(());
                        }
                        tx.write_all(buffer.data()).await?;
                    }
                })
            });
        }
    });

    Ok(())
}

pub async fn serve_graphical_presenter(
    request_stream: felement::GraphicalPresenterRequestStream,
    container: &Container,
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
                        container.kernel.framebuffer.present_view(token);
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
