// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::control::{self as control, DeviceControl};
use ffx_command::{bug, FfxContext};
use fho::return_bug;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_audio_device as fadevice;
use fidl_fuchsia_hardware_audio as fhaudio;
use fidl_fuchsia_io as fio;
use fuchsia_audio::device::Selector;

/// Connect to an instance of a FIDL protocol hosted in `directory` using the given `path`.
// This is essentially the same as `fuchsia_component::client::connect_to_named_protocol_at_dir_root`.
// We can't use the `fuchsia_component` library in ffx because it doesn't build on host.
fn connect_to_named_protocol_at_dir_root<P: fidl::endpoints::ProtocolMarker>(
    directory: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<P::Proxy> {
    let (proxy, server_end) = create_proxy::<P>().unwrap();
    directory
        .open(
            fio::OpenFlags::empty(),
            fio::ModeType::empty(),
            path,
            server_end.into_channel().into(),
        )
        .bug_context("Failed to call Directory.Open")?;
    Ok(proxy)
}

/// Connects to the `fuchsia.hardware.audio.Codec` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_codec(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::CodecProxy> {
    let connector_proxy =
        connect_to_named_protocol_at_dir_root::<fhaudio::CodecConnectorMarker>(dev_class, path)
            .bug_context("Failed to connect to CodecConnector")?;

    let (proxy, server_end) = create_proxy::<fhaudio::CodecMarker>().unwrap();
    connector_proxy.connect(server_end).bug_context("Failed to call Connect")?;

    Ok(proxy)
}

/// Connects to the `fuchsia.hardware.audio.Dai` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_dai(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::DaiProxy> {
    let connector_proxy =
        connect_to_named_protocol_at_dir_root::<fhaudio::DaiConnectorMarker>(dev_class, path)
            .bug_context("Failed to connect to DaiConnector")?;

    let (proxy, server_end) = create_proxy::<fhaudio::DaiMarker>().unwrap();
    connector_proxy.connect(server_end).bug_context("Failed to call Connect")?;

    Ok(proxy)
}

/// Connects to the `fuchsia.hardware.audio.Composite` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_composite(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::CompositeProxy> {
    // DFv2 Composite drivers do not use a connector/trampoline like Codec/Dai/StreamConfig.
    connect_to_named_protocol_at_dir_root::<fhaudio::CompositeMarker>(dev_class, path)
        .bug_context("Failed to connect to Composite")
}

/// Connects to the `fuchsia.hardware.audio.StreamConfig` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_streamconfig(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::StreamConfigProxy> {
    let connector_proxy = connect_to_named_protocol_at_dir_root::<
        fhaudio::StreamConfigConnectorMarker,
    >(dev_class, path)
    .bug_context("Failed to connect to StreamConfigConnector")?;

    let (proxy, server_end) = create_proxy::<fhaudio::StreamConfigMarker>().unwrap();
    connector_proxy.connect(server_end).bug_context("Failed to call Connect")?;

    Ok(proxy)
}

/// Connects to the `fuchsia.audio.device.Control` protocol for a device in the registry.
pub async fn connect_registry_control(
    control_creator: &fadevice::ControlCreatorProxy,
    token_id: fadevice::TokenId,
) -> fho::Result<fadevice::ControlProxy> {
    let (proxy, server_end) = create_proxy::<fadevice::ControlMarker>().unwrap();

    control_creator
        .create(fadevice::ControlCreatorCreateRequest {
            token_id: Some(token_id),
            control_server: Some(server_end),
            ..Default::default()
        })
        .await
        .bug_context("Failed to call ControlCreator.Create")?
        .map_err(|err| bug!("Failed to create Control: {:?}", err))?;

    Ok(proxy)
}

/// Connects to the control protocol of the device identified by `selector`.
pub async fn connect_device_control(
    dev_class: &fio::DirectoryProxy,
    control_creator: Option<&fadevice::ControlCreatorProxy>,
    selector: Selector,
) -> fho::Result<Box<dyn DeviceControl>> {
    let device_control: Box<dyn DeviceControl> = match selector {
        Selector::Devfs(devfs_selector) => {
            let protocol_path = devfs_selector.relative_path();

            match devfs_selector.0.device_type {
                fadevice::DeviceType::Codec => {
                    let codec = connect_hw_codec(dev_class, protocol_path.as_str())?;
                    Box::new(control::HardwareCodec(codec))
                }
                fadevice::DeviceType::Composite => {
                    let composite = connect_hw_composite(dev_class, protocol_path.as_str())?;
                    Box::new(control::HardwareComposite(composite))
                }
                fadevice::DeviceType::Dai => {
                    let dai = connect_hw_dai(dev_class, protocol_path.as_str())?;
                    Box::new(control::HardwareDai(dai))
                }
                fadevice::DeviceType::Input | fadevice::DeviceType::Output => {
                    let streamconfig = connect_hw_streamconfig(dev_class, protocol_path.as_str())?;
                    Box::new(control::HardwareStreamConfig(streamconfig))
                }
                _ => return_bug!("Unknown device type: {:?}", devfs_selector.0.device_type),
            }
        }
        Selector::Registry(registry_selector) => {
            let control_creator =
                control_creator.ok_or_else(|| bug!("ControlCreator is not available"))?;
            let control =
                connect_registry_control(&control_creator, registry_selector.token_id()).await?;
            Box::new(control::Registry(control))
        }
    };
    Ok(device_control)
}
