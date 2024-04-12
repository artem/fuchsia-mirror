// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ffx_audio_common::PlayResult;
use ffx_audio_play_args::{AudioRenderUsageExtended, PlayCommand};
use fho::{moniker, FfxMain, FfxTool, MachineWriter};
use fidl::HandleBased;
use fidl_fuchsia_audio_controller as fac;
use std::io::Read;

#[derive(FfxTool)]
pub struct PlayTool {
    #[command]
    cmd: PlayCommand,

    #[with(moniker("/core/audio_ffx_daemon"))]
    controller: fac::PlayerProxy,
}

fho::embedded_plugin!(PlayTool);
#[async_trait(?Send)]
impl FfxMain for PlayTool {
    type Writer = MachineWriter<PlayResult>;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let (play_remote, play_local) = fidl::Socket::create_datagram();
        let reader: Box<dyn Read + Send + 'static> = match &self.cmd.file {
            Some(input_file_path) => {
                let file = std::fs::File::open(&input_file_path)
                    .map_err(|e| anyhow!("Error trying to open file \"{input_file_path}\": {e}"))?;
                Box::new(file)
            }
            None => Box::new(std::io::stdin()),
        };

        play_impl(self.controller, play_local, play_remote, self.cmd, reader, writer)
            .await
            .map_err(Into::into)
    }
}

async fn play_impl(
    controller: fac::PlayerProxy,
    wav_local: fidl::Socket,
    wav_remote: fidl::Socket,
    command: PlayCommand,
    input_reader: Box<dyn Read + Send + 'static>, // Input generalized to stdin, file, or test buffer.
    mut writer: MachineWriter<PlayResult>,
) -> Result<(), anyhow::Error> {
    let renderer = match command.usage {
        AudioRenderUsageExtended::Ultrasound => {
            fac::RendererConfig::UltrasoundRenderer(fac::UltrasoundRendererConfig {
                packet_count: command.packet_count,
                ..Default::default()
            })
        }

        AudioRenderUsageExtended::Background(usage)
        | AudioRenderUsageExtended::Media(usage)
        | AudioRenderUsageExtended::SystemAgent(usage)
        | AudioRenderUsageExtended::Communication(usage)
        | AudioRenderUsageExtended::Interruption(usage) => {
            fac::RendererConfig::StandardRenderer(fac::StandardRendererConfig {
                usage: Some(usage),
                clock: Some(command.clock),
                ..Default::default()
            })
        }
    };

    // Duplicate socket handle so that connection stays alive in real + testing scenarios.
    let remote_socket = wav_remote
        .duplicate_handle(fidl::Rights::SAME_RIGHTS)
        .map_err(|e| anyhow!("Error duplicating socket: {e}"))?;

    let request = fac::PlayerPlayRequest {
        wav_source: Some(remote_socket),
        destination: Some(fac::PlayDestination::Renderer(renderer)),
        gain_settings: Some(fac::GainSettings {
            mute: Some(command.mute),
            gain: Some(command.gain),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = ffx_audio_common::play(request, controller, wav_local, input_reader).await?;
    writer
        .machine_or_else(&result, || {
            format!("Successfully processed all audio data. Bytes processed: {:?}", {
                result
                    .bytes_processed
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| format!("Unavailable"))
            })
        })
        .map_err(Into::<anyhow::Error>::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_core::macro_deps::futures::AsyncWriteExt;
    use ffx_writer::TestBuffers;
    use fidl_fuchsia_media as fmedia;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn test_play() -> Result<(), fho::Error> {
        let controller = ffx_audio_common::tests::fake_audio_player();
        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<PlayResult> = MachineWriter::new_test(None, &test_buffers);

        let stdin_command = PlayCommand {
            usage: AudioRenderUsageExtended::Media(fmedia::AudioRenderUsage::Media),
            buffer_size: Some(48000),
            packet_count: None,
            file: None,
            gain: 0.0,
            mute: false,
            clock: fac::ClockType::Flexible(fac::Flexible),
        };

        let (play_remote, play_local) = fidl::Socket::create_datagram();
        let mut async_play_local = fidl::AsyncSocket::from_socket(
            play_local.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap(),
        );

        async_play_local.write_all(ffx_audio_common::tests::WAV_HEADER_EXT).await.unwrap();
        let result = play_impl(
            controller.clone(),
            play_local,
            play_remote,
            stdin_command,
            Box::new(&ffx_audio_common::tests::WAV_HEADER_EXT[..]),
            writer,
        )
        .await;

        result.unwrap();
        // TODO(b/300279107): Calculate total bytes sent to an AudioRenderer.
        // The test audio controller always returns 1 for bytes processed value.
        let expected_output =
            format!("Successfully processed all audio data. Bytes processed: \"1\"\n");
        let stdout = test_buffers.into_stdout_str();
        assert_eq!(stdout, expected_output);

        // Test reading from a file.
        let test_dir = TempDir::new().unwrap();
        let test_dir_path = test_dir.path().to_path_buf();
        let test_wav_path = test_dir_path.join("sine.wav");
        let wav_path = test_wav_path
            .clone()
            .into_os_string()
            .into_string()
            .map_err(|_e| anyhow!("Error turning path into string"))?;

        // Create valid WAV file.
        fs::File::create(&test_wav_path)
            .unwrap()
            .write_all(ffx_audio_common::tests::SINE_WAV)
            .unwrap();
        fs::set_permissions(&test_wav_path, fs::Permissions::from_mode(0o770)).unwrap();

        let file_reader = std::fs::File::open(&test_wav_path)
            .map_err(|e| anyhow!("Error trying to open file \"{}\": {e}", wav_path))?;

        let file_command = PlayCommand {
            usage: AudioRenderUsageExtended::Media(fmedia::AudioRenderUsage::Media),
            buffer_size: Some(48000),
            packet_count: None,
            file: Some(wav_path),
            gain: 0.0,
            mute: false,
            clock: fac::ClockType::Flexible(fac::Flexible),
        };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<PlayResult> = MachineWriter::new_test(None, &test_buffers);

        let (play_remote, play_local) = fidl::Socket::create_datagram();
        let result = play_impl(
            controller,
            play_local,
            play_remote,
            file_command,
            Box::new(file_reader),
            writer,
        )
        .await;
        result.unwrap();
        // TODO(b/300279107): Calculate total bytes sent to an AudioRenderer.
        // The test audio controller always returns 1 for bytes processed value.
        let expected_output =
            format!("Successfully processed all audio data. Bytes processed: \"1\"\n");
        let stdout = test_buffers.into_stdout_str();
        assert_eq!(stdout, expected_output);

        Ok(())
    }
}
