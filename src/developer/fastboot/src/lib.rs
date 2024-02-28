// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::reply::Reply,
    anyhow::{anyhow, bail, Result},
    async_trait::async_trait,
    chrono::Duration,
    command::Command,
    fuchsia_async::TimeoutExt,
    futures::{
        io::{AsyncRead, AsyncWrite},
        lock::Mutex,
        AsyncReadExt, AsyncWriteExt,
    },
    lazy_static::lazy_static,
    std::io::Read,
    thiserror::Error,
};

pub mod command;
pub mod reply;
pub mod test_transport;

const MAX_PACKET_SIZE: usize = 64;
const DEFAULT_READ_TIMEOUT_SECS: i64 = 30;

lazy_static! {
    static ref SEND_LOCK: Mutex<()> = Mutex::new(());
    static ref TRANSFER_LOCK: Mutex<()> = Mutex::new(());
}

#[derive(Debug, Error)]
pub enum SendError {
    #[error("timed out reading a reply from device")]
    Timeout,
}

#[async_trait]
pub trait InfoListener {
    async fn on_info(&self, info: String) -> Result<()> {
        tracing::info!("Fastboot Info: \"{}\"", info);
        Ok(())
    }
}

struct LogInfoListener {}
impl InfoListener for LogInfoListener {}

#[async_trait]
pub trait UploadProgressListener {
    async fn on_started(&self, size: usize) -> Result<()>;
    async fn on_progress(&self, bytes_written: u64) -> Result<()>;
    async fn on_error(&self, error: &str) -> Result<()>;
    async fn on_finished(&self) -> Result<()>;
}

async fn read_from_interface<T: AsyncRead + Unpin>(interface: &mut T) -> Result<Reply> {
    let mut buf: [u8; MAX_PACKET_SIZE] = [0; MAX_PACKET_SIZE];
    let size = interface.read(&mut buf).await?;
    let (trimmed, _) = buf.split_at(size);
    let trimmed = trimmed.to_vec();
    match Reply::try_from(trimmed.as_slice()) {
        Ok(r) => {
            tracing::debug!("fastboot: received {r:?}: {}", String::from_utf8_lossy(&trimmed));
            return Ok(r);
        }
        Err(e) => {
            tracing::debug!(
                "fastboot: could not parse reply: {}",
                String::from_utf8_lossy(&trimmed),
            );
            bail!(e);
        }
    }
}

async fn read<T: AsyncRead + Unpin>(
    interface: &mut T,
    listener: &(impl InfoListener + Sync),
) -> Result<Reply> {
    read_with_timeout(interface, listener, Duration::seconds(DEFAULT_READ_TIMEOUT_SECS)).await
}

async fn read_and_log_info<T: AsyncRead + Unpin>(interface: &mut T) -> Result<Reply> {
    read_with_timeout(interface, &LogInfoListener {}, Duration::seconds(DEFAULT_READ_TIMEOUT_SECS))
        .await
}

async fn read_with_timeout<T: AsyncRead + Unpin>(
    interface: &mut T,
    listener: &(impl InfoListener + Sync),
    timeout: Duration,
) -> Result<Reply> {
    let std_timeout = timeout.to_std().expect("converting chrono Duration to std");
    let end_time = std::time::Instant::now() + std_timeout;
    loop {
        match read_from_interface(interface)
            .on_timeout(end_time, || Err(anyhow!(SendError::Timeout)))
            .await
        {
            Ok(Reply::Info(msg)) => listener.on_info(msg).await?,
            #[cfg(target_os = "linux")]
            Err(e) => {
                // If we get a TIMEDOUT response, keep reading -- that's just the usb_bulk crate
                // not willing to spend more than 800ms waiting for a result
                // Desired code:
                // if let Some(ioe) = e.downcast_ref::<std::io::Error>() {
                //     if ioe.kind() != std::io::ErrorKind::TimedOut {
                //         ...
                //     }
                // }
                // Unfortunately usb_bulk does not try to interpret the
                // type of the error, but instead always sets the kind to
                // ErrorKind::Other.  So we can't check if the kind is
                // Timeout.  So instead, let's just read the text of
                // the error, ugh.
                if e.to_string() != "Read error: -110" {
                    bail!(e);
                }
            }
            #[cfg(target_os = "macos")]
            Err(_) => {
                // usb_bulk returns different values on mac vs. linux. On Linux it
                // returns ETIMEDOUT, but on the Mac it's just a generic -1. (And
                // Apple doesn't actually document how to determine whether a read
                // has timed out.)  So on Mac, we'll ignore _all_ errors, and cross
                // our fingers.
            }
            other => return other,
        }
        // We can't actually rely on `on_timeout()` to time out, because while
        // `usb_bulk` claims that it implements `AsyncRead`, it's not actually
        // async.  As a result, on_timeout() doesn't work.  We'll leave it in
        // to avoid problems in the future, and so our unit tests can remain
        // asynchronous.
        if std::time::Instant::now() > end_time {
            bail!(SendError::Timeout);
        }
    }
}

#[allow(clippy::unused_io_amount)] // TODO(https://fxbug.dev/42177040)
pub async fn send_with_listener<T: AsyncRead + AsyncWrite + Unpin>(
    cmd: Command,
    interface: &mut T,
    listener: &(impl InfoListener + Sync),
) -> Result<Reply> {
    let _lock = SEND_LOCK.lock().await;
    let bytes = Vec::<u8>::try_from(&cmd)?;
    tracing::debug!("Fastboot: writing command {cmd:?}: {}", String::from_utf8_lossy(&bytes));
    interface.write(&bytes).await?;
    read(interface, listener).await
}

#[tracing::instrument(skip(interface))]
#[allow(clippy::unused_io_amount)] // TODO(https://fxbug.dev/42177040)
pub async fn send<T: AsyncRead + AsyncWrite + Unpin>(
    cmd: Command,
    interface: &mut T,
) -> Result<Reply> {
    let _lock = SEND_LOCK.lock().await;
    let bytes = Vec::<u8>::try_from(&cmd)?;
    tracing::debug!("Fastboot: writing command {cmd:?}: {}", String::from_utf8_lossy(&bytes));
    interface.write(&bytes).await?;
    read_and_log_info(interface).await
}

#[allow(clippy::unused_io_amount)] // TODO(https://fxbug.dev/42177040)
pub async fn send_with_timeout<T: AsyncRead + AsyncWrite + Unpin>(
    cmd: Command,
    interface: &mut T,
    timeout: Duration,
) -> Result<Reply> {
    let _lock = SEND_LOCK.lock().await;
    let bytes = Vec::<u8>::try_from(&cmd)?;
    tracing::debug!("Fastboot: writing command {cmd:?}: {}", String::from_utf8_lossy(&bytes));
    interface.write(&bytes).await?;
    read_with_timeout(interface, &LogInfoListener {}, timeout).await
}

#[tracing::instrument(skip(interface, listener, buf))]
pub async fn upload<T: AsyncRead + AsyncWrite + Unpin, R: Read>(
    size: usize,
    buf: &mut R,
    interface: &mut T,
    listener: &impl UploadProgressListener,
) -> Result<Reply> {
    let _lock = TRANSFER_LOCK.lock().await;
    // We are sending "Download" in our "upload" function because we are the
    // host -- from the device's point of view, it is a download
    let size = u32::try_from(size)?;
    let reply = send(Command::Download(size), interface).await?;
    match reply {
        Reply::Data(s) => {
            if s != size {
                let err = format!(
                    "Target responded with wrong data size - received:{} expected:{}",
                    s, size
                );
                tracing::error!(%err);
                listener.on_error(&err).await?;
                bail!(err);
            }
            listener.on_started(size.try_into().unwrap()).await?;
            tracing::debug!("fastboot: writing {} bytes", size);

            let mut bytes = [0; 4096];
            loop {
                match buf.read(&mut bytes) {
                    Ok(n) => {
                        if n == 0 {
                            break;
                        }
                        match interface.write(&bytes[..n]).await {
                            Err(e) => {
                                let err = format!("Could not write to interface: {:?}", e);
                                tracing::error!(%err);
                                listener.on_error(&err).await?;
                                bail!(err);
                            }
                            _ => {
                                listener.on_progress(n.try_into().unwrap()).await?;
                                tracing::trace!("fastboot: wrote {} bytes", n);
                            }
                        }
                    }
                    Err(e) => {
                        let err = format!("Could not read bytes to upload: {:?}", e);
                        tracing::error!(%err);
                        listener.on_error(&err).await?;
                        bail!(err);
                    }
                }
            }
            tracing::debug!("fastboot: completed writing {} bytes", size);
            match read_and_log_info(interface).await {
                Ok(reply) => {
                    listener.on_finished().await?;
                    Ok(reply)
                }
                Err(e) => {
                    let err = format!("Could not verify upload: {:?}", e);
                    tracing::error!(%err);
                    listener.on_error(&err).await?;
                    bail!(err);
                }
            }
        }
        _ => bail!("Did not get expected Data reply: {:?}", reply),
    }
}

pub async fn download<T: AsyncRead + AsyncWrite + Unpin>(
    path: &String,
    interface: &mut T,
) -> Result<Reply> {
    let _lock = TRANSFER_LOCK.lock().await;
    // We are sending "Upload" in our "download" function because we are the
    // host -- from the device's point of view, it is an upload
    let reply = send(Command::Upload, interface).await?;
    tracing::debug!("got reply from upload command: {:?}", reply);
    match reply {
        Reply::Data(s) => {
            let size = usize::try_from(s)?;
            let mut buffer: [u8; 100] = [0; 100];
            let mut bytes_read: usize = 0;
            let mut file = async_fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .await?;
            while bytes_read != size {
                match interface.read(&mut buffer[..]).await {
                    Err(e) => bail!("Could not read to interface: {:?}", e),
                    Ok(len) => {
                        tracing::debug!("fastboot: upload got {bytes_read}/{size} bytes");
                        bytes_read += len;
                        file.write_all(&buffer[..len]).await?;
                    }
                }
            }
            file.flush().await?;
            read_and_log_info(interface)
                .await
                .map_err(|e| anyhow!("Could not verify download: {:?}", e))
        }
        _ => bail!("Did not get expected Data reply: {:?}", reply),
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::command::ClientVariable;
    use crate::test_transport::TestTransport;
    use std::io::Cursor;
    use std::sync::Arc;

    #[derive(Debug, PartialEq)]
    enum UploadEvent {
        OnStarted(usize),
        OnProgress(u64),
        OnError(String),
        OnFinished,
    }

    struct PushEventsUploadProgressListener {
        event_queue: Arc<Mutex<Vec<UploadEvent>>>,
    }

    #[async_trait]
    impl UploadProgressListener for PushEventsUploadProgressListener {
        async fn on_started(&self, size: usize) -> Result<()> {
            let mut queue = self.event_queue.lock().await;
            queue.push(UploadEvent::OnStarted(size));
            Ok(())
        }
        async fn on_progress(&self, bytes_written: u64) -> Result<()> {
            let mut queue = self.event_queue.lock().await;
            queue.push(UploadEvent::OnProgress(bytes_written));
            Ok(())
        }
        async fn on_error(&self, error: &str) -> Result<()> {
            let mut queue = self.event_queue.lock().await;
            queue.push(UploadEvent::OnError(error.to_string()));
            Ok(())
        }
        async fn on_finished(&self) -> Result<()> {
            let mut queue = self.event_queue.lock().await;
            queue.push(UploadEvent::OnFinished);
            Ok(())
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_send_does_not_return_info_replies() {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("0.4".to_string()));
        let response = send(Command::GetVar(ClientVariable::Version), &mut test_transport).await;
        assert!(!response.is_err());
        assert_eq!(response.unwrap(), Reply::Okay("0.4".to_string()));

        test_transport.push(Reply::Okay("0.4".to_string()));
        test_transport.push(Reply::Info("Test".to_string()));
        let response_with_info =
            send(Command::GetVar(ClientVariable::Version), &mut test_transport).await;
        assert!(!response_with_info.is_err());
        assert_eq!(response_with_info.unwrap(), Reply::Okay("0.4".to_string()));

        test_transport.push(Reply::Okay("0.4".to_string()));
        for i in 0..10 {
            test_transport.push(Reply::Info(format!("Test {}", i).to_string()));
        }
        let response_with_info =
            send(Command::GetVar(ClientVariable::Version), &mut test_transport).await;
        assert!(!response_with_info.is_err());
        assert_eq!(response_with_info.unwrap(), Reply::Okay("0.4".to_string()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_uploading_data_to_partition() {
        let data: [u8; 14336] = [0; 14336];
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("Done Writing".to_string()));
        test_transport.push(Reply::Info("Writing".to_string()));
        test_transport.push(Reply::Data(14336));

        let events = Arc::new(Mutex::new(Vec::<UploadEvent>::new()));
        let listener = PushEventsUploadProgressListener { event_queue: events.clone() };

        let response =
            upload(data.len(), &mut Cursor::new(data), &mut test_transport, &listener).await;
        assert!(!response.is_err());
        assert_eq!(response.unwrap(), Reply::Okay("Done Writing".to_string()));

        let queue = events.lock().await;
        assert_eq!(
            *queue,
            vec![
                UploadEvent::OnStarted(14336),
                UploadEvent::OnProgress(4096),
                UploadEvent::OnProgress(4096),
                UploadEvent::OnProgress(4096),
                UploadEvent::OnProgress(2048),
                UploadEvent::OnFinished,
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_uploading_data_with_unexpected_reply() {
        let data: [u8; 1024] = [0; 1024];
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Info("Writing".to_string()));

        let events = Arc::new(Mutex::new(Vec::<UploadEvent>::new()));
        let listener = PushEventsUploadProgressListener { event_queue: events.clone() };
        let response =
            upload(data.len(), &mut Cursor::new(data), &mut test_transport, &listener).await;
        assert!(response.is_err());
        let queue = events.lock().await;
        assert_eq!(*queue, vec![]);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_uploading_data_with_unexpected_data_size_reply() {
        let data: [u8; 1024] = [0; 1024];
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1000));

        let events = Arc::new(Mutex::new(Vec::<UploadEvent>::new()));
        let listener = PushEventsUploadProgressListener { event_queue: events.clone() };
        let response =
            upload(data.len(), &mut Cursor::new(data), &mut test_transport, &listener).await;
        assert!(response.is_err());
        let queue = events.lock().await;
        assert_eq!(
            *queue,
            vec![UploadEvent::OnError(
                "Target responded with wrong data size - received:1000 expected:1024".to_string()
            ),]
        );
    }
}
