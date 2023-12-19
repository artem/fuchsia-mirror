// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use {
    anyhow::{format_err, Result},
    args::DumpCommand,
    fidl_fuchsia_driver_development as fdd,
    fuchsia_driver_dev::{self, DFv1Device, DFv2Node, Device},
    std::collections::{BTreeMap, VecDeque},
    std::io::Write,
};

const INDENT_SIZE: usize = 2;

trait NodeInfoPrinter {
    fn print(&self, writer: &mut dyn Write, indent_level: usize) -> Result<()>;

    fn print_graph_node(&self, writer: &mut dyn Write) -> Result<()>;
    fn print_graph_edge(&self, writer: &mut dyn Write, child: &fdd::NodeInfo) -> Result<()>;
}

impl NodeInfoPrinter for DFv1Device {
    fn print(&self, writer: &mut dyn Write, indent_level: usize) -> Result<()> {
        let bound_driver_libname = self.get_v1_info()?.bound_driver_libname.clone();
        writeln!(
            writer,
            "{:indent$}[{}] pid={} {}",
            "",
            self.extract_name()?,
            self.0.driver_host_koid.as_ref().ok_or(format_err!("Missing driver host KOID"))?,
            bound_driver_libname.ok_or(format_err!("Missing driver libname"))?,
            indent = indent_level * INDENT_SIZE,
        )?;
        Ok(())
    }

    fn print_graph_node(&self, writer: &mut dyn Write) -> Result<()> {
        writeln!(
            writer,
            "     \"{}\" [label=\"{}\"]",
            self.0.id.as_ref().ok_or(format_err!("Device missing id"))?,
            self.extract_name()?,
        )?;
        Ok(())
    }

    fn print_graph_edge(&self, writer: &mut dyn Write, child: &fdd::NodeInfo) -> Result<()> {
        writeln!(
            writer,
            "     \"{}\" -> \"{}\"",
            self.0.id.as_ref().ok_or(format_err!("Device missing id"))?,
            child.id.as_ref().ok_or(format_err!("Child device missing id"))?,
        )?;
        Ok(())
    }
}

impl NodeInfoPrinter for DFv2Node {
    fn print(&self, writer: &mut dyn Write, indent_level: usize) -> Result<()> {
        let koid_str = match &self.0.driver_host_koid {
            Some(koid) => format!("{}", koid),
            None => format!("None"),
        };

        writeln!(
            writer,
            "{:indent$}[{}] pid={} {}",
            "",
            self.extract_name()?,
            koid_str,
            self.0.bound_driver_url.as_deref().unwrap_or(""),
            indent = indent_level * INDENT_SIZE,
        )?;
        Ok(())
    }

    fn print_graph_node(&self, writer: &mut dyn Write) -> Result<()> {
        writeln!(
            writer,
            "     \"{}\" [label=\"{}\"]",
            self.0.id.as_ref().ok_or(format_err!("Node missing id"))?,
            self.extract_name()?,
        )?;
        Ok(())
    }

    fn print_graph_edge(&self, writer: &mut dyn Write, child: &fdd::NodeInfo) -> Result<()> {
        writeln!(
            writer,
            "     \"{}\" -> \"{}\"",
            self.0.id.as_ref().ok_or(format_err!("Node missing id"))?,
            child.id.as_ref().ok_or(format_err!("Child node missing id"))?
        )?;
        Ok(())
    }
}

impl NodeInfoPrinter for Device {
    fn print(&self, writer: &mut dyn Write, indent_level: usize) -> Result<()> {
        match self {
            Device::V1(device) => device.print(writer, indent_level),
            Device::V2(device) => device.print(writer, indent_level),
        }
    }

    fn print_graph_node(&self, writer: &mut dyn Write) -> Result<()> {
        match self {
            Device::V1(device) => device.print_graph_node(writer),
            Device::V2(node) => node.print_graph_node(writer),
        }
    }

    fn print_graph_edge(&self, writer: &mut dyn Write, child: &fdd::NodeInfo) -> Result<()> {
        match self {
            Device::V1(device) => device.print_graph_edge(writer, child),
            Device::V2(node) => node.print_graph_edge(writer, child),
        }
    }
}

fn print_tree(
    writer: &mut dyn Write,
    root: &Device,
    device_map: &BTreeMap<u64, &Device>,
) -> Result<()> {
    let mut stack = VecDeque::new();
    stack.push_front((root, 0));
    while let Some((device, indent_level)) = stack.pop_front() {
        device.print(writer, indent_level)?;
        if let Some(child_ids) = &device.get_device_info().child_ids {
            for id in child_ids.iter().rev() {
                if let Some(child) = device_map.get(id) {
                    stack.push_front((child, indent_level + 1));
                }
            }
        }
    }
    Ok(())
}

pub async fn dump(
    cmd: DumpCommand,
    writer: &mut dyn Write,
    driver_development_proxy: fdd::DriverDevelopmentProxy,
) -> Result<()> {
    let devices: Vec<Device> = fuchsia_driver_dev::get_device_info(
        &driver_development_proxy,
        &[],
        /* exact_match= */ false,
    )
    .await?
    .into_iter()
    .map(|device| device.into())
    .collect();

    let device_map = devices
        .iter()
        .map(|device| {
            let device_info = device.get_device_info();
            if let Some(id) = device_info.id {
                Ok((id, device))
            } else {
                Err(format_err!("Missing device id"))
            }
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    if cmd.graph {
        let digraph_prefix = r#"digraph {
     forcelabels = true; splines="ortho"; ranksep = 1.2; nodesep = 0.5;
     node [ shape = "box" color = " #2a5b4f" penwidth = 2.25 fontname = "prompt medium" fontsize = 10 margin = 0.22 ];
     edge [ color = " #37474f" penwidth = 1 style = dashed fontname = "roboto mono" fontsize = 10 ];"#;
        writeln!(writer, "{}", digraph_prefix)?;
        for device in devices.iter() {
            device.print_graph_node(writer)?;
        }

        for device in devices.iter() {
            if let Some(child_ids) = &device.get_device_info().child_ids {
                for id in child_ids.iter().rev() {
                    let child = &device_map[&id];
                    device.print_graph_edge(writer, child.get_device_info())?;
                }
            }
        }

        writeln!(writer, "}}")?;
    } else {
        let roots = devices.iter().filter(|device| {
            if let Some(node_filter) = &cmd.device {
                let name = device.extract_name().unwrap_or("");
                name == node_filter
            } else {
                let device_info = device.get_device_info();
                if let Some(parent_ids) = device_info.parent_ids.as_ref() {
                    for parent_id in parent_ids.iter() {
                        if device_map.contains_key(parent_id) {
                            return false;
                        }
                    }
                    true
                } else {
                    true
                }
            }
        });

        for root in roots {
            print_tree(writer, root, &device_map)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        anyhow::Context,
        argh::FromArgs,
        fidl::endpoints::ServerEnd,
        fuchsia_async as fasync,
        futures::{
            future::{Future, FutureExt},
            stream::StreamExt,
        },
    };

    async fn test_dump<F, Fut>(cmd: DumpCommand, on_driver_development_request: F) -> Result<String>
    where
        F: Fn(fdd::DriverDevelopmentRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + Sync,
    {
        let (driver_development_proxy, mut driver_development_requests) =
            fidl::endpoints::create_proxy_and_stream::<fdd::DriverDevelopmentMarker>()
                .context("Failed to create FIDL proxy")?;

        // Run the command and mock driver development server.
        let mut writer = Vec::new();
        let request_handler_task = fasync::Task::spawn(async move {
            while let Some(res) = driver_development_requests.next().await {
                let request = res.context("Failed to get next request")?;
                on_driver_development_request(request).await.context("Failed to handle request")?;
            }
            anyhow::bail!("Driver development request stream unexpectedly closed");
        });
        futures::select! {
            res = request_handler_task.fuse() => {
                res?;
                anyhow::bail!("Request handler task unexpectedly finished");
            }
            res = dump(cmd, &mut writer, driver_development_proxy).fuse() => res.context("Dump command failed")?,
        }

        String::from_utf8(writer).context("Failed to convert dump output to a string")
    }

    async fn run_device_info_iterator_server(
        mut device_infos: Vec<fdd::NodeInfo>,
        iterator: ServerEnd<fdd::NodeInfoIteratorMarker>,
    ) -> Result<()> {
        let mut iterator =
            iterator.into_stream().context("Failed to convert iterator into a stream")?;
        while let Some(res) = iterator.next().await {
            let request = res.context("Failed to get request")?;
            match request {
                fdd::NodeInfoIteratorRequest::GetNext { responder } => {
                    responder
                        .send(&device_infos)
                        .context("Failed to send device infos to responder")?;
                    device_infos.clear();
                }
            }
        }
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_simple() {
        let cmd = DumpCommand::from_args(&["dump"], &[]).unwrap();

        let output = test_dump(cmd, |request: fdd::DriverDevelopmentRequest| async move {
            match request {
                fdd::DriverDevelopmentRequest::GetNodeInfo {
                    node_filter: _,
                    iterator,
                    control_handle: _,
                    exact_match: _,
                } => {
                    let parent_id = 0;
                    let child_id = 1;
                    run_device_info_iterator_server(
                        vec![
                            fdd::NodeInfo {
                                id: Some(parent_id),
                                parent_ids: Some(Vec::new()),
                                child_ids: Some(vec![child_id]),
                                driver_host_koid: Some(0),
                                bound_driver_url: Some(String::from(
                                    "fuchsia-pkg://fuchsia.com/foo-package#meta/foo.cm",
                                )),
                                versioned_info: Some(fdd::VersionedNodeInfo::V1(
                                    fdd::V1DeviceInfo {
                                        topological_path: Some(String::from(
                                            "/dev/sys/platform/foo",
                                        )),
                                        bound_driver_libname: Some(String::from("foo.so")),
                                        ..Default::default()
                                    },
                                )),
                                ..Default::default()
                            },
                            fdd::NodeInfo {
                                id: Some(child_id),
                                parent_ids: Some(vec![parent_id]),
                                child_ids: Some(Vec::new()),
                                driver_host_koid: Some(0),
                                bound_driver_url: Some(String::from(
                                    "fuchsia-pkg://fuchsia.com/bar-package#meta/bar.cm",
                                )),
                                versioned_info: Some(fdd::VersionedNodeInfo::V1(
                                    fdd::V1DeviceInfo {
                                        topological_path: Some(String::from(
                                            "/dev/sys/platform/foo/bar",
                                        )),
                                        bound_driver_libname: Some(String::from("bar.so")),
                                        ..Default::default()
                                    },
                                )),
                                ..Default::default()
                            },
                        ],
                        iterator,
                    )
                    .await
                    .context("Failed to run device info iterator server")?;
                }
                _ => {}
            }
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(output, "[foo] pid=0 foo.so\n  [bar] pid=0 bar.so\n");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_duplicates_are_filtered() {
        let cmd = DumpCommand::from_args(&["dump"], &[]).unwrap();

        let output = test_dump(cmd, |request: fdd::DriverDevelopmentRequest| async move {
            match request {
                fdd::DriverDevelopmentRequest::GetNodeInfo {
                    node_filter: _,
                    iterator,
                    control_handle: _,
                    exact_match: _,
                } => {
                    run_device_info_iterator_server(make_test_devices(), iterator)
                        .await
                        .context("Failed to run device info iterator server")?;
                }
                _ => {}
            }
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            r#"[platform] pid=0 root.so
  [parent] pid=0 parent.so
    [child] pid=0 child.so
[parent] pid=0 parent.so
  [child] pid=0 child.so
"#
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_with_node_filter() {
        let cmd = DumpCommand::from_args(&["dump"], &["parent"]).unwrap();

        let output = test_dump(cmd, |request: fdd::DriverDevelopmentRequest| async move {
            match request {
                fdd::DriverDevelopmentRequest::GetNodeInfo {
                    node_filter: _,
                    iterator,
                    control_handle: _,
                    exact_match: _,
                } => {
                    run_device_info_iterator_server(make_test_devices(), iterator)
                        .await
                        .context("Failed to run device info iterator server")?;
                }
                _ => {}
            }
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            r#"[parent] pid=0 parent.so
  [child] pid=0 child.so
"#
        );
    }

    fn make_test_devices() -> Vec<fdd::NodeInfo> {
        let null_id = 0;
        let root_id = 1;
        let composite_parent_id = 2;
        let composite_child_id = 3;
        vec![
            // Root device
            fdd::NodeInfo {
                id: Some(root_id),
                parent_ids: Some(vec![null_id]),
                child_ids: Some(vec![composite_parent_id]),
                driver_host_koid: Some(0),
                bound_driver_url: Some(String::from(
                    "fuchsia-pkg://fuchsia.com/root-package#meta/root.cm",
                )),
                versioned_info: Some(fdd::VersionedNodeInfo::V1(fdd::V1DeviceInfo {
                    topological_path: Some(String::from("/dev/sys/platform")),
                    bound_driver_libname: Some(String::from("root.so")),
                    ..Default::default()
                })),
                ..Default::default()
            },
            // Composite parent
            fdd::NodeInfo {
                id: Some(composite_parent_id),
                parent_ids: None,
                child_ids: Some(vec![composite_child_id]),
                driver_host_koid: Some(0),
                bound_driver_url: Some(String::from(
                    "fuchsia-pkg://fuchsia.com/parent-package#meta/parent.cm",
                )),
                versioned_info: Some(fdd::VersionedNodeInfo::V1(fdd::V1DeviceInfo {
                    topological_path: Some(String::from("/dev/parent")),
                    bound_driver_libname: Some(String::from("parent.so")),
                    ..Default::default()
                })),
                ..Default::default()
            },
            // Composite child
            fdd::NodeInfo {
                id: Some(composite_child_id),
                parent_ids: Some(vec![composite_parent_id]),
                child_ids: Some(Vec::new()),
                driver_host_koid: Some(0),
                bound_driver_url: Some(String::from(
                    "fuchsia-pkg://fuchsia.com/child-package#meta/child.cm",
                )),
                versioned_info: Some(fdd::VersionedNodeInfo::V1(fdd::V1DeviceInfo {
                    topological_path: Some(String::from("/dev/parent/child")),
                    bound_driver_libname: Some(String::from("child.so")),
                    ..Default::default()
                })),
                ..Default::default()
            },
        ]
    }
}
