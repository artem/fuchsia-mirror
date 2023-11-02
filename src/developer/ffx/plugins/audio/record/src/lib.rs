// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    async_trait::async_trait,
    blocking::Unblock,
    ffx_audio_record_args::{AudioCaptureUsageExtended, RecordCommand},
    fho::ToolIO,
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_audio_controller::{
        CapturerConfig, RecordSource, RecorderProxy, RecorderRecordRequest, StandardCapturerConfig,
    },
    fidl_fuchsia_media::AudioStreamType,
    futures::AsyncWrite,
    futures::FutureExt,
};

#[derive(FfxTool)]
pub struct RecordTool {
    #[command]
    cmd: RecordCommand,

    #[with(moniker("/core/audio_ffx_daemon"))]
    controller: RecorderProxy,
}
fho::embedded_plugin!(RecordTool);
#[async_trait(?Send)]
impl FfxMain for RecordTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let capturer_usage = match self.cmd.usage {
            AudioCaptureUsageExtended::Background(usage)
            | AudioCaptureUsageExtended::Foreground(usage)
            | AudioCaptureUsageExtended::Communication(usage)
            | AudioCaptureUsageExtended::SystemAgent(usage) => Some(usage),
            _ => None,
        };

        let (location, gain_settings) = match self.cmd.usage {
            AudioCaptureUsageExtended::Loopback => {
                (RecordSource::Loopback(fidl_fuchsia_audio_controller::Loopback {}), None)
            }
            AudioCaptureUsageExtended::Ultrasound => (
                RecordSource::Capturer(CapturerConfig::UltrasoundCapturer(
                    fidl_fuchsia_audio_controller::UltrasoundCapturer {},
                )),
                None,
            ),
            _ => (
                RecordSource::Capturer(CapturerConfig::StandardCapturer(StandardCapturerConfig {
                    usage: capturer_usage,
                    ..Default::default()
                })),
                Some(fidl_fuchsia_audio_controller::GainSettings {
                    mute: Some(self.cmd.mute),
                    gain: Some(self.cmd.gain),
                    ..Default::default()
                }),
            ),
        };

        let (record_remote, record_local) = fidl::Socket::create_datagram();
        let (cancel_proxy, cancel_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_audio_controller::RecordCancelerMarker>()
                .map_err(|e| anyhow::anyhow!("FIDL Error creating canceler proxy: {e}"))?;

        let request = RecorderRecordRequest {
            source: Some(location),
            stream_type: Some(AudioStreamType::from(&self.cmd.format)),
            duration: self.cmd.duration.map(|duration| duration.as_nanos() as i64),
            canceler: Some(cancel_server),
            gain_settings,
            buffer_size: self.cmd.buffer_size,
            wav_data: Some(record_remote),
            ..Default::default()
        };

        let mut stdout = Unblock::new(std::io::stdout());
        let keypress_waiter = ffx_audio_common::cancel_on_keypress(
            cancel_proxy,
            ffx_audio_common::get_stdin_waiter().fuse(),
        );

        record_impl(self.controller, request, keypress_waiter, record_local, &mut stdout, writer)
            .await?;
        Ok(())
    }
}

async fn record_impl<W>(
    controller: RecorderProxy,
    request: RecorderRecordRequest,
    keypress_waiter: impl futures::Future<Output = Result<(), std::io::Error>>,
    record_local: fidl::Socket,
    mut wav_writer: W, // Output generalized to stdout or a test buffer. Forward data
    // from daemon to this writer.
    mut output_result_writer: SimpleWriter,
) -> Result<()>
where
    W: AsyncWrite + std::marker::Unpin,
{
    let result = ffx_audio_common::record(
        controller,
        request,
        record_local,
        &mut wav_writer,
        keypress_waiter,
    )
    .await;

    let message = ffx_audio_common::format_record_result(result);
    writeln!(output_result_writer.stderr(), "{}", message)
        .map_err(|e| anyhow::anyhow!("Writing result failed with error {e}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_audio_common::tests::SINE_WAV;
    use ffx_writer::TestBuffers;
    use fho::macro_deps::ffx_writer::TestBuffer;
    use fho::SimpleWriter;
    use futures::FutureExt;

    #[fuchsia::test]
    pub async fn test_record_no_cancel() -> Result<(), fho::Error> {
        // Test without sending a cancel message. Still set up the canceling proxy and server,
        // but never send the message from proxy to daemon to cancel. Test daemon should
        // exit after duration (real daemon exits after sending all duration amount of packets).
        let controller = ffx_audio_common::tests::fake_audio_recorder();
        let test_buffers = TestBuffers::default();
        let result_writer: SimpleWriter = SimpleWriter::new_test(&test_buffers);

        let (cancel_proxy, cancel_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_audio_controller::RecordCancelerMarker>()
                .unwrap();

        let test_stdout = TestBuffer::default();

        let (record_remote, record_local) = fidl::Socket::create_datagram();
        let request = RecorderRecordRequest {
            source: None,
            stream_type: None,
            duration: Some(500),
            canceler: Some(cancel_server),
            gain_settings: None,
            buffer_size: None,
            wav_data: Some(record_remote),
            ..Default::default()
        };

        // Pass a future that will never complete as an input waiter.
        let keypress_waiter =
            ffx_audio_common::cancel_on_keypress(cancel_proxy, futures::future::pending().fuse());

        let _res = record_impl(
            controller,
            request,
            keypress_waiter,
            record_local,
            test_stdout.clone(),
            result_writer,
        )
        .await?;

        let expected_result_output =
            format!("Successfully recorded 123 bytes of audio. \nPackets processed: 123 \nLate wakeups: Unavailable\n");
        let stderr = test_buffers.into_stderr_str();
        assert_eq!(stderr, expected_result_output);

        let stdout = test_stdout.into_inner();
        let expected_wav_output = Vec::from(SINE_WAV);
        assert_eq!(stdout, expected_wav_output);
        Ok(())
    }

    #[fuchsia::test]
    pub async fn test_record_immediate_cancel() -> Result<(), fho::Error> {
        let controller = ffx_audio_common::tests::fake_audio_recorder();
        let test_buffers = TestBuffers::default();
        let result_writer: SimpleWriter = SimpleWriter::new_test(&test_buffers);

        let (cancel_proxy, cancel_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_audio_controller::RecordCancelerMarker>()
                .unwrap();

        let test_stdout = TestBuffer::default();

        let (record_remote, record_local) = fidl::Socket::create_datagram();
        let request = RecorderRecordRequest {
            source: None,
            stream_type: None,
            duration: None,
            canceler: Some(cancel_server),
            gain_settings: None,
            buffer_size: None,
            wav_data: Some(record_remote),
            ..Default::default()
        };

        // Test canceler signaling. Not concerned with how much data gets back through socket.
        // Test failing is never finishing execution before timeout.
        let keypress_waiter =
            ffx_audio_common::cancel_on_keypress(cancel_proxy, futures::future::ready(Ok(())));

        let _res = record_impl(
            controller,
            request,
            keypress_waiter,
            record_local,
            test_stdout.clone(),
            result_writer,
        )
        .await?;

        Ok(())
    }
}
