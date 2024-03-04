// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    async_trait::async_trait,
    ffx_audio_listdevices_args::ListDevicesCommand,
    fho::{moniker, FfxMain, FfxTool, MachineWriter},
    fidl_fuchsia_audio_controller::DeviceControlProxy,
    fuchsia_zircon_status::Status,
    itertools::Itertools,
    serde::{Deserialize, Serialize},
    std::io::Write,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct ListDeviceResult {
    pub devices: Vec<DeviceSelectorWrapper>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceSelectorWrapper {
    device_id: Option<String>,
    is_input: Option<bool>,
    device_type: DeviceTypeWrapper,
    path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DeviceTypeWrapper {
    DAI,
    CODEC,
    STREAMCONFIG,
}
#[derive(FfxTool)]
pub struct ListDevicesTool {
    #[command]
    _cmd: ListDevicesCommand,
    #[with(moniker("/core/audio_ffx_daemon"))]
    audio_proxy: DeviceControlProxy,
}

fho::embedded_plugin!(ListDevicesTool);
#[async_trait(?Send)]
impl FfxMain for ListDevicesTool {
    type Writer = MachineWriter<ListDeviceResult>;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        list_devices_impl(self.audio_proxy, writer).await.map_err(Into::into)
    }
}

async fn list_devices_impl(
    audio_proxy: DeviceControlProxy,
    mut writer: MachineWriter<ListDeviceResult>,
) -> Result<(), anyhow::Error> {
    // TODO(https://fxbug.dev/298683668): Move ListDevices implementation to ffx client
    let response = audio_proxy
        .list_devices()
        .await
        .context("List devices failed")?
        .map_err(Status::from_raw)
        .context("Error from daemon for list devices request")?;
    if let Some(devices) = response.devices {
        writer
            .machine_or_else(
                &ListDeviceResult {
                    devices: devices
                        .clone()
                        .into_iter()
                        .map(|device| DeviceSelectorWrapper {
                            device_id: device.id.clone(),
                            // TODO(https://fxbug.dev/327490666): Fix incorrect STREAMCONFIG device_type
                            device_type: DeviceTypeWrapper::STREAMCONFIG,
                            is_input: device.is_input,
                            path: format_utils::path_for_selector(&device).map(String::from).ok(),
                        })
                        .collect(),
                },
                || {
                    devices
                        .iter()
                        .map(|device| {
                            let in_out = match device.is_input {
                                Some(is_input) => {
                                    if is_input {
                                        format!("Input")
                                    } else {
                                        format!("Output")
                                    }
                                }
                                None => format!("Input/Output not specified"),
                            };

                            format!(
                                "{:?} Device id: {:?}, Device type: {:?}, {in_out}",
                                format_utils::path_for_selector(&device),
                                device.id,
                                device.device_type
                            )
                        })
                        .join("\n")
                },
            )
            .map_err(Into::into)
    } else {
        writeln!(writer, "No devices found.").map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_writer::{Format, TestBuffers};
    use fidl_fuchsia_audio_controller::{
        DeviceControlListDevicesResponse, DeviceControlRequest, DeviceSelector,
    };
    use fidl_fuchsia_hardware_audio::DeviceType;

    fn fake_audio_daemon() -> DeviceControlProxy {
        let devices = vec![
            DeviceSelector {
                is_input: Some(true),
                id: Some("abc123".to_string()),
                device_type: Some(DeviceType::StreamConfig),
                ..Default::default()
            },
            DeviceSelector {
                is_input: Some(false),
                id: Some("abc123".to_string()),
                device_type: Some(DeviceType::StreamConfig),
                ..Default::default()
            },
        ];
        let callback = move |req| match req {
            DeviceControlRequest::ListDevices { responder, .. } => {
                let response = DeviceControlListDevicesResponse {
                    devices: Some(devices.clone()),
                    ..Default::default()
                };
                responder.send(Ok(response)).unwrap();
            }
            _ => {}
        };
        fho::testing::fake_proxy(callback)
    }

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn test_list_devices() -> Result<(), fho::Error> {
        let audio_daemon = fake_audio_daemon();
        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<ListDeviceResult> = MachineWriter::new_test(None, &test_buffers);
        let result = list_devices_impl(audio_daemon, writer).await;
        result.unwrap();

        let stdout = test_buffers.into_stdout_str();
        let stdout_expected = format!(
            "Ok(\"/dev/class/audio-input/abc123\") Device id: Some(\"abc123\"), Device type: Some(StreamConfig), Input\n\
            Ok(\"/dev/class/audio-output/abc123\") Device id: Some(\"abc123\"), Device type: Some(StreamConfig), Output\n");

        assert_eq!(stdout, stdout_expected);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn test_list_devices_machine() -> Result<(), fho::Error> {
        let audio_daemon = fake_audio_daemon();
        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<ListDeviceResult> =
            MachineWriter::new_test(Some(Format::Json), &test_buffers);
        let result = list_devices_impl(audio_daemon, writer).await;
        result.unwrap();

        let stdout = test_buffers.into_stdout_str();
        let stdout_content = format!(
            "\
        {{\
            \"devices\":[\
                {{\
                    \"device_id\":\"abc123\",\
                    \"is_input\":true,\
                    \"device_type\":\"STREAMCONFIG\",\
                    \"path\":\"/dev/class/audio-input/abc123\"\
                }},\
                {{\
                    \"device_id\":\"abc123\",\
                    \"is_input\":false,\
                    \"device_type\":\"STREAMCONFIG\",\
                    \"path\":\"/dev/class/audio-output/abc123\"\
                }}\
            ]\
        }}"
        );

        let stdout_expected = format!("{}\n", stdout_content);
        assert_eq!(stdout, stdout_expected);
        Ok(())
    }
}
