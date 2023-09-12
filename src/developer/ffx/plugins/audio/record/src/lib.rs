// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_audio_record_args::AudioCaptureUsageExtended;

use {
    anyhow::Result,
    async_trait::async_trait,
    blocking::Unblock,
    ffx_audio_record_args::RecordCommand,
    fho::{moniker, FfxContext, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_audio_controller::{
        CapturerConfig, RecordSource, RecorderProxy, RecorderRecordRequest, StandardCapturerConfig,
    },
    fidl_fuchsia_media::AudioStreamType,
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
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
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

        let (cancel_client, cancel_server) = fidl::endpoints::create_endpoints::<
            fidl_fuchsia_audio_controller::RecordCancelerMarker,
        >();

        let request = RecorderRecordRequest {
            source: Some(location),
            stream_type: Some(AudioStreamType::from(&self.cmd.format)),
            duration: self.cmd.duration.map(|duration| duration.as_nanos() as i64),
            canceler: Some(cancel_server),
            gain_settings,
            buffer_size: self.cmd.buffer_size,
            ..Default::default()
        };

        let (stdout_sock, stderr_sock) = {
            let response = self
                .controller
                .record(request)
                .await
                .user_message("Timeout awaiting record command response.")?
                .map_err(|e| {
                    anyhow::anyhow!(
                        "AudioDaemon Record request failed with error {}",
                        fuchsia_zircon_status::Status::from_raw(e)
                    )
                })?;

            (
                response.wav_data.ok_or(anyhow::anyhow!("No socket for wav data."))?,
                response.stderr.ok_or(anyhow::anyhow!("No stderr socket."))?,
            )
        };

        let mut stdout = Unblock::new(std::io::stdout());
        let mut stderr = Unblock::new(std::io::stderr());

        futures::future::try_join3(
            futures::io::copy(
                fidl::AsyncSocket::from_socket(stdout_sock)
                    .user_message("Could not create async socket for stdout.")?,
                &mut stdout,
            ),
            futures::io::copy(
                fidl::AsyncSocket::from_socket(stderr_sock)
                    .user_message("Could not create async socket for stderr.")?,
                &mut stderr,
            ),
            ffx_audio_common::wait_for_keypress(cancel_client),
        )
        .await
        .map(|_| ())
        .user_message("Error copying data from socket.")
    }
}
