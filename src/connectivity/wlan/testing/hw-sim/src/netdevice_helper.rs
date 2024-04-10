// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::{create_proxy, Proxy},
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    futures::{FutureExt as _, StreamExt as _, TryStreamExt as _},
    std::pin::pin,
    wlan_common::{appendable::Appendable, big_endian::BigEndianU16, mac},
};

/// Returns a Netdevice client with the specified MAC address, or None if none is found.
pub async fn create_client(
    devfs: &fidl_fuchsia_io::DirectoryProxy,
    mac: fidl_fuchsia_net::MacAddress,
) -> Option<(netdevice_client::Client, netdevice_client::Port)> {
    let (directory, directory_server) =
        create_proxy::<fidl_fuchsia_io::DirectoryMarker>().expect("create proxy");

    fdio::service_connect_at(
        devfs.as_channel().as_ref(),
        "class/network",
        directory_server.into_channel(),
    )
    .expect("connect to /dev/class/network");

    let dirents = fuchsia_fs::directory::readdir(&directory).await.expect("readdir failed");
    let devices = dirents.into_iter().map(|file| {
        tracing::info!("Found file name {:?}", file.name);
        connect_to_named_protocol_at_dir_root::<
                fidl_fuchsia_hardware_network::DeviceInstanceMarker,
            >(&directory, &file.name)
            .expect("creating proxy")
    });

    let results = futures::stream::iter(devices).filter_map(|netdev_device| async move {
        let (device_proxy, device_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_hardware_network::DeviceMarker>()
                .expect("create proxy");
        netdev_device.get_device(device_server).expect("get device");
        let client = netdevice_client::Client::new(device_proxy);

        let port_id = match client
            .device_port_event_stream()
            .expect("failed to get port event stream")
            .try_next()
            .await
            .expect("error observing ports")
            .expect("port stream ended unexpectedly")
        {
            fidl_fuchsia_hardware_network::DevicePortEvent::Existing(port_id) => port_id,
            e @ fidl_fuchsia_hardware_network::DevicePortEvent::Removed(_)
            | e @ fidl_fuchsia_hardware_network::DevicePortEvent::Idle(_)
            | e @ fidl_fuchsia_hardware_network::DevicePortEvent::Added(_) => {
                unreachable!("unexpected event: {:?}", e);
            }
        };
        let netdev_port = port_id.try_into().expect("bad port id");
        let port_proxy = client.connect_port(netdev_port).expect("failed to connect port");
        let (mac_addressing, mac_addressing_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_hardware_network::MacAddressingMarker>()
                .expect("failed to create proxy");
        port_proxy.get_mac(mac_addressing_server).expect("failed to get mac addressing");
        let addr = mac_addressing.get_unicast_address().await.expect("failed to get address");

        (addr.octets == mac.octets).then(move || (client, netdev_port))
    });
    let mut results = pin!(results);
    results.next().await
}

/// Returns a Session and a Task. The Task is not used by the client, but must remain in scope
/// to avoid being destructed, as it drives the Session.
pub async fn start_session(
    client: netdevice_client::Client,
    port: netdevice_client::Port,
) -> (netdevice_client::Session, fuchsia_async::Task<()>) {
    let info = client.device_info().await.expect("get device info");
    let (session, task) = client
        .primary_session(
            "wlan-test",
            info.base_info.max_buffer_length.expect("buffer length not set in DeviceInfo").get()
                as usize,
        )
        .await
        .expect("open primary session");
    let task_handle = fuchsia_async::Task::spawn(task.map(|r| r.expect("session task failed")));
    session
        .attach(port, &[fidl_fuchsia_hardware_network::FrameType::Ethernet])
        .await
        .expect("attach port");

    let (watcher_proxy, watcher_server) = fidl::endpoints::create_proxy().unwrap();
    client
        .connect_port(port)
        .expect("Failed to connect to port")
        // Using `MAX_STATUS_BUFFER` is overly conservative, but, since this is a test, there is no
        // real downside.
        .get_status_watcher(watcher_server, fidl_fuchsia_hardware_network::MAX_STATUS_BUFFER)
        .unwrap();

    // Wait for the port to come online. Otherwise, tests may attempt to use the data path before
    // the network device is ready to handle frames.
    loop {
        if let Some(flags) = watcher_proxy.watch_status().await.unwrap().flags {
            if flags == fidl_fuchsia_hardware_network::StatusFlags::ONLINE {
                tracing::info!("Network device port online!");
                break;
            }
            tracing::info!("Waiting for network device port to come online...");
        }
    }

    (session, task_handle)
}

pub async fn send(session: &netdevice_client::Session, port: &netdevice_client::Port, data: &[u8]) {
    let mut buffer = session.alloc_tx_buffer(data.len()).await.expect("allocate tx buffer");
    buffer.set_frame_type(fidl_fuchsia_hardware_network::FrameType::Ethernet);
    buffer.set_port(*port);
    buffer.write_at(0, &data).expect("write message");
    session.send(buffer).expect("failed to send data");
}

pub async fn recv(session: &netdevice_client::Session) -> Vec<u8> {
    let recv_result = session.recv().await.expect("recv failed");
    let mut buffer = vec![0; recv_result.len()];
    recv_result.read_at(0, &mut buffer).expect("read from buffer");
    buffer
}

pub fn write_fake_frame<B: Appendable>(
    da: ieee80211::MacAddr,
    sa: ieee80211::MacAddr,
    payload: &[u8],
    buf: &mut B,
) {
    buf.append_value(&mac::EthernetIIHdr {
        da,
        sa,
        ether_type: BigEndianU16::from_native(mac::ETHER_TYPE_IPV4),
    })
    .expect("error creating fake ethernet header");
    buf.append_bytes(payload).expect("buffer too small for ethernet payload");
}
