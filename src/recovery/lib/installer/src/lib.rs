// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! # Installer library
//!
//! The installer library exposes functionality to install Fuchsia to a disk by copying images from
//! one block device to another. The primary use case is an installer USB, which contains the images
//! to write to persistent storage.

pub mod partition;

use {
    anyhow::{anyhow, Context, Error},
    fdio,
    fidl::endpoints::{ClientEnd, Proxy, ServerEnd},
    fidl_fuchsia_hardware_power_statecontrol::{AdminMarker, RebootReason},
    fidl_fuchsia_paver::{
        BootManagerMarker, Configuration, DynamicDataSinkProxy, PaverMarker, PaverProxy,
    },
    fidl_fuchsia_sysinfo::SysInfoMarker,
    fuchsia_component::client,
    fuchsia_zircon as zx,
    partition::Partition,
    recovery_util_block::BlockDevice,
    std::sync::Mutex,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BootloaderType {
    Efi,
    Coreboot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstallationPaths {
    pub install_source: Option<BlockDevice>,
    pub install_target: Option<BlockDevice>,
    pub bootloader_type: Option<BootloaderType>,
    pub install_destinations: Vec<BlockDevice>,
    pub available_disks: Vec<BlockDevice>,
}

impl InstallationPaths {
    pub fn new() -> InstallationPaths {
        InstallationPaths {
            install_source: None,
            install_target: None,
            bootloader_type: None,
            install_destinations: Vec::new(),
            available_disks: Vec::new(),
        }
    }
}

pub async fn find_install_source(
    block_devices: &Vec<BlockDevice>,
    bootloader: BootloaderType,
) -> Result<&BlockDevice, Error> {
    let mut candidate = Err(anyhow!("Could not find the installer disk. Is it plugged in?"));
    for device in block_devices.iter().filter(|d| d.is_disk()) {
        // get_partitions returns an empty vector if it doesn't find any partitions
        // with the workstation-installer GUID on the disk.
        let partitions = Partition::get_partitions(device, block_devices, bootloader).await?;
        if !partitions.is_empty() {
            if candidate.is_err() {
                candidate = Ok(device);
            } else {
                return Err(anyhow!(
                    "Please check you only have one installation disk plugged in!"
                ));
            }
        }
    }
    candidate
}

fn paver_connect(path: &str) -> Result<(PaverProxy, DynamicDataSinkProxy), Error> {
    let (block_device_chan, block_remote) = zx::Channel::create();
    fdio::service_connect(&path, block_remote)?;

    let (controller, controller_remote) = zx::Channel::create();
    fdio::service_connect(&format!("{path}/device_controller"), controller_remote)?;

    let (data_sink_chan, data_remote) = zx::Channel::create();

    let paver: PaverProxy =
        client::connect_to_protocol::<PaverMarker>().context("Could not connect to paver")?;
    paver.use_block_device(
        ClientEnd::from(block_device_chan),
        ClientEnd::from(controller),
        ServerEnd::from(data_remote),
    )?;

    let data_sink =
        DynamicDataSinkProxy::from_channel(fidl::AsyncChannel::from_channel(data_sink_chan)?);
    Ok((paver, data_sink))
}

pub async fn get_bootloader_type() -> Result<BootloaderType, Error> {
    let proxy = fuchsia_component::client::connect_to_protocol::<SysInfoMarker>()
        .context("Could not connect to 'fuchsia.sysinfo.SysInfo' service")?;
    let (status, bootloader) =
        proxy.get_bootloader_vendor().await.context("Getting bootloader vendor")?;
    if let Some(bootloader) = bootloader {
        tracing::info!("Bootloader vendor = {}", bootloader);
        if bootloader == "coreboot" {
            Ok(BootloaderType::Coreboot)
        } else {
            // The installer only supports coreboot and EFI,
            // and EFI BIOS vendor depends on the manufacturer,
            // so we assume that non-coreboot bootloader vendors
            // mean EFI.
            Ok(BootloaderType::Efi)
        }
    } else {
        Err(Error::new(zx::Status::from_raw(status)))
    }
}

/// Restart the machine.
pub async fn restart() {
    let proxy = fuchsia_component::client::connect_to_protocol::<AdminMarker>()
        .expect("Could not connect to 'fuchsia.hardware.power.statecontrol.Admin' service");

    proxy
        .reboot(RebootReason::UserRequest)
        .await
        .expect("Failed to reboot")
        .expect("Failed to reboot");
}

/// Set the active boot configuration for the newly-installed system. We always boot from the "A"
/// slot to start with.
async fn set_active_configuration(paver: &PaverProxy) -> Result<(), Error> {
    let (boot_manager, server) = fidl::endpoints::create_proxy::<BootManagerMarker>()
        .context("Creating boot manager endpoints")?;

    paver.find_boot_manager(server).context("Could not find boot manager")?;

    zx::Status::ok(
        boot_manager
            .set_configuration_active(Configuration::A)
            .await
            .context("Sending set configuration active")?,
    )
    .context("Setting active configuration")?;

    zx::Status::ok(boot_manager.flush().await.context("Sending boot manager flush")?)
        .context("Flushing active configuration")
}

pub async fn do_install<F>(
    installation_paths: InstallationPaths,
    progress_callback: &F,
) -> Result<(), Error>
where
    F: Send + Sync + Fn(String),
{
    let install_target =
        installation_paths.install_target.ok_or(anyhow!("No installation target?"))?;
    let install_source =
        installation_paths.install_source.ok_or(anyhow!("No installation source?"))?;
    let bootloader_type = installation_paths.bootloader_type.unwrap();

    let (paver, data_sink) =
        paver_connect(&install_target.class_path).context("Could not contact paver")?;

    tracing::info!("Wiping old partition tables...");
    progress_callback(String::from("Wiping old partition tables..."));
    data_sink.wipe_partition_tables().await?;

    tracing::info!("Initializing Fuchsia partition tables...");
    progress_callback(String::from("Initializing Fuchsia partition tables..."));
    data_sink.initialize_partition_tables().await?;

    tracing::info!("Getting source partitions");
    progress_callback(String::from("Getting source partitions"));
    let to_install = Partition::get_partitions(
        &install_source,
        &installation_paths.available_disks,
        bootloader_type,
    )
    .await
    .context("Getting source partitions")?;

    let num_partitions = to_install.len();
    let mut current_partition = 1;
    for part in to_install {
        progress_callback(String::from(format!(
            "paving partition {} of {}",
            current_partition, num_partitions
        )));

        let prev_percent = Mutex::new(0 as i64);

        let pave_progress_callback = |data_read, data_total| {
            if data_total == 0 {
                return;
            }
            let cur_percent: i64 =
                unsafe { (((data_read as f64) / (data_total as f64)) * 100.0).to_int_unchecked() };
            let mut prev = prev_percent.lock().unwrap();
            if cur_percent == *prev {
                return;
            }
            *prev = cur_percent;

            progress_callback(String::from(format!(
                "paving partition {} of {}: {}%",
                current_partition, num_partitions, cur_percent
            )));
        };

        tracing::info!("Paving partition: {:?}", part);
        part.pave(&data_sink, &pave_progress_callback).await?;
        if part.is_ab() {
            tracing::info!("Paving partition: {:?} [-B]", part);
            part.pave_b(&data_sink).await?
        }

        current_partition += 1;
    }

    progress_callback(String::from("Flushing Partitions"));
    zx::Status::ok(data_sink.flush().await.context("Sending flush")?)
        .context("Flushing partitions")?;

    progress_callback(String::from("Setting active configuration for the new system"));
    set_active_configuration(&paver)
        .await
        .context("Setting active configuration for the new system")?;

    Ok(())
}
