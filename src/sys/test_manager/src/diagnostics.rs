// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_diagnostics as fdiagnostics,
    fidl_fuchsia_diagnostics::{
        BatchIteratorMarker, ClientSelectorConfiguration, DataType, Format, StreamMode,
        StreamParameters,
    },
    fidl_fuchsia_diagnostics_host as fhost, fidl_fuchsia_test_manager as ftest_manager,
    fuchsia_async as fasync,
    tracing::warn,
};

pub(crate) struct ServeSyslogOutcome {
    /// Task serving any protocols needed to proxy logs. For example, this is populated
    /// when logs are served over overnet using DiagnosticsBridge.
    pub logs_iterator_task: Option<fasync::Task<Result<(), Error>>>,
}

/// Connect to archivist and starting serving syslog.
/// TODO(https://fxbug.dev/133153): Only take one ArchiveAccessorProxy, not both.
pub(crate) fn serve_syslog(
    accessor: fdiagnostics::ArchiveAccessorProxy,
    host_accessor: fhost::ArchiveAccessorProxy,
    log_iterator: ftest_manager::LogsIterator,
) -> Result<ServeSyslogOutcome, anyhow::Error> {
    let logs_iterator_task = match log_iterator {
        ftest_manager::LogsIterator::Stream(iterator) => {
            let iterator_fut = run_iterator_socket(&host_accessor, iterator);
            Some(fasync::Task::spawn(async move {
                iterator_fut.await?;
                Ok(())
            }))
        }
        ftest_manager::LogsIterator::Batch(iterator) => {
            IsolatedLogsProvider::new(&accessor).start_streaming_logs(iterator)?;
            None
        }
        _ => None,
    };
    Ok(ServeSyslogOutcome { logs_iterator_task })
}

fn run_iterator_socket(
    host_accessor: &fhost::ArchiveAccessorProxy,
    socket: fuchsia_zircon::Socket,
) -> fidl::client::QueryResponseFut<()> {
    host_accessor.stream_diagnostics(
        &StreamParameters {
            stream_mode: Some(StreamMode::SnapshotThenSubscribe),
            data_type: Some(DataType::Logs),
            format: Some(Format::Json),
            client_selector_configuration: Some(ClientSelectorConfiguration::SelectAll(true)),
            batch_retrieval_timeout_seconds: None,
            ..Default::default()
        },
        socket,
    )
}

/// Type alias for &'a ArchiveAccessorProxy
struct IsolatedLogsProvider<'a> {
    accessor: &'a fdiagnostics::ArchiveAccessorProxy,
}

impl<'a> IsolatedLogsProvider<'a> {
    fn new(accessor: &'a fdiagnostics::ArchiveAccessorProxy) -> Self {
        Self { accessor }
    }

    fn start_streaming_logs(
        &self,
        iterator: ServerEnd<BatchIteratorMarker>,
    ) -> Result<(), anyhow::Error> {
        let stream_parameters = StreamParameters {
            stream_mode: Some(StreamMode::SnapshotThenSubscribe),
            data_type: Some(DataType::Logs),
            format: Some(Format::Json),
            client_selector_configuration: Some(ClientSelectorConfiguration::SelectAll(true)),
            batch_retrieval_timeout_seconds: None,
            ..Default::default()
        };
        self.accessor.stream_diagnostics(&stream_parameters, iterator).map_err(|err| {
            warn!(%err, "Failed to subscribe to isolated logs");
            err
        })?;
        Ok(())
    }
}
