// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - acquire and display product bundle information (metadata)
//! - acquire related data files, such as disk partition images (data)

use ::gcs::client::{Client, ProgressResponse, ProgressState};
use anyhow::{anyhow, Context};
use async_fs::rename;
use async_trait::async_trait;
use ffx_product::{CommandStatus, MachineOutput, MachineUi};
use ffx_product_download_args::DownloadCommand;
use ffx_product_list::pb_list_impl;
use fho::{bug, return_user_error, FfxMain, FfxTool, ToolIO, VerifiedMachineWriter};
use pbms::{make_way_for_output, transfer_download, AuthFlowChoice};
use std::{
    io::{stdin, stdout},
    path::Path,
};

#[derive(FfxTool)]
pub struct PbDownloadTool {
    #[command]
    cmd: DownloadCommand,
}

#[async_trait(?Send)]
impl FfxMain for PbDownloadTool {
    type Writer = VerifiedMachineWriter<MachineOutput<()>>;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let client = Client::initial()?;

        if writer.is_machine() {
            self.do_machine_main(&client, writer).await
        } else {
            self.do_text_main(&client, writer).await
        }
    }
}

impl PbDownloadTool {
    async fn do_machine_main(
        &self,
        client: &Client,
        writer: <PbDownloadTool as fho::FfxMain>::Writer,
    ) -> fho::Result<()> {
        let ui = MachineUi::new(writer);

        let cmd = match preprocess_cmd(&self.cmd, &ui).await {
            Ok(c) => c,
            Err(e) => {
                ui.machine(MachineOutput::CommandStatus(CommandStatus::UserError {
                    message: e.to_string(),
                }))?;
                return Err(e.into());
            }
        };

        match pb_download_impl(
            &cmd.auth,
            cmd.force,
            &cmd.manifest_url,
            &cmd.product_dir,
            &client,
            &ui,
        )
        .await
        {
            Ok(_) => {
                ui.machine(MachineOutput::CommandStatus(CommandStatus::Ok { message: None }))?;
                return Ok(());
            }
            Err(e) => {
                ui.machine(MachineOutput::CommandStatus(CommandStatus::UnexpectedError {
                    message: e.to_string(),
                }))?;
                return Err(e.into());
            }
        }
    }

    async fn do_text_main(
        &self,
        client: &Client,
        mut writer: <PbDownloadTool as fho::FfxMain>::Writer,
    ) -> fho::Result<()> {
        let mut output = stdout();
        let mut err_out = writer.stderr();
        let mut input = stdin();
        let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);

        let cmd = preprocess_cmd(&self.cmd, &ui).await?;

        pb_download_impl(&cmd.auth, cmd.force, &cmd.manifest_url, &cmd.product_dir, &client, &ui)
            .await
            .map_err(|e| e.into())
    }
}

fho::embedded_plugin!(PbDownloadTool);

pub async fn pb_download_impl<I: structured_ui::Interface>(
    auth: &AuthFlowChoice,
    force: bool,
    manifest_url: &str,
    product_dir: &Path,
    client: &Client,
    ui: &I,
) -> fho::Result<()> {
    let start = std::time::Instant::now();
    tracing::info!("---------------------- Begin ----------------------------");
    tracing::debug!("transfer_manifest_url Url::parse");
    let transfer_manifest_url = match url::Url::parse(manifest_url) {
        Ok(p) => p,
        _ => return_user_error!(
            "The source location must be a URL, failed to parse {:?}",
            manifest_url
        ),
    };
    tracing::debug!("make_way_for_output {:?}", product_dir);
    make_way_for_output(&product_dir, force).await?;

    let parent_dir = product_dir.parent().ok_or_else(|| anyhow!("local dir has no parent"))?;
    let temp_dir = tempfile::TempDir::new_in(&parent_dir).map_err(|e| bug!("{e}"))?;
    tracing::debug!("transfer_manifest, transfer_manifest_url {:?}", transfer_manifest_url);
    transfer_download(
        &transfer_manifest_url,
        &temp_dir.path(),
        &auth,
        &|layers| {
            let mut progress = structured_ui::Progress::builder();
            progress.title("Transfer download");
            progress.entry("Transfer manifest", /*at=*/ 1, /*of=*/ 2, "steps");
            for layer in layers {
                progress.entry(&layer.name, layer.at, layer.of, layer.units);
            }
            ui.present(&structured_ui::Presentation::Progress(progress))?;
            Ok(ProgressResponse::Continue)
        },
        ui,
        &client,
    )
    .await
    .context("downloading via transfer manifest")?;

    // Workaround for having the product bundle nested in a sub-dir.
    let extra_dir = temp_dir.path().join("product_bundle");
    let from_dir = if extra_dir.exists() { extra_dir } else { temp_dir.path().to_path_buf() };
    rename(&from_dir, &product_dir)
        .await
        .with_context(|| format!("moving dir {:?} to {:?}", from_dir, product_dir))?;

    let layers = vec![ProgressState { name: "complete", at: 2, of: 2, units: "steps" }];
    let mut progress = structured_ui::Progress::builder();
    progress.title("Transfer download");
    for layer in layers {
        progress.entry(&layer.name, layer.at, layer.of, layer.units);
    }
    ui.present(&structured_ui::Presentation::Progress(progress))?;

    tracing::debug!(
        "Total ffx product download runtime {} seconds.",
        start.elapsed().as_secs_f32()
    );
    tracing::debug!("End");
    Ok(())
}

pub async fn preprocess_cmd<I: structured_ui::Interface>(
    cmd: &DownloadCommand,
    ui: &I,
) -> fho::Result<DownloadCommand> {
    // If the manifest_url is a transfer url, we don't need to preprocess.
    if let Ok(_) = url::Url::parse(&cmd.manifest_url) {
        return Ok(cmd.clone());
    };

    // If the manifest_url look like a product name, we try to convert it into a
    // transfer manifest url.
    let products =
        pb_list_impl(&cmd.auth, cmd.base_url.clone(), cmd.version.clone(), cmd.branch.clone(), ui)
            .await?
            .iter()
            .cloned()
            .filter(|x| x.name == cmd.manifest_url)
            .collect::<Vec<_>>();

    if products.len() != 1 {
        return_user_error!(
            "Expected a single product entry while trying to download a product by name, found {}",
            products.len()
        );
    }

    let processed_cmd =
        DownloadCommand { manifest_url: products[0].transfer_manifest_url.clone(), ..cmd.clone() };
    Ok(processed_cmd)
}

#[cfg(test)]
mod test {
    use super::*;
    use fuchsia_hyper_test_support::{
        handler::{ForPath, StaticResponse},
        TestServer,
    };
    use std::io::Write;
    use temp_test_env::TempTestEnv;

    #[fuchsia::test]
    async fn test_gcs_pb_download_impl() {
        let test_dir = tempfile::TempDir::new().expect("temp dir");
        let server = TestServer::builder()
            .handler(ForPath::new(
                "/example/fake/transfer.json",
                StaticResponse::ok_body(
                    r#"
            {
                "version": "1",
                "entries": [{
                    "type": "files",
                    "local": "foo",
                    "remote": "data",
                    "entries": [{ "name": "payload.txt"}]
                }]
            }"#,
                ),
            ))
            .handler(ForPath::new("/api/b/example/o", StaticResponse::ok_body(r#"{}"#)))
            .start()
            .await;
        let auth = pbms::AuthFlowChoice::Default;
        let force = false;
        let manifest_url = "gs://example/fake/transfer.json".to_string();
        let product_dir = test_dir.path().join("download");
        let client = Client::initial_with_urls(
            &server.local_url_for_path("api"),
            &server.local_url_for_path("storage"),
        )
        .expect("creating client");
        let ui = structured_ui::MockUi::new();
        pb_download_impl(&auth, force, &manifest_url, &product_dir, &client, &ui)
            .await
            .expect("testing download");
    }

    #[fuchsia::test]
    async fn test_http_download() {
        let server = TestServer::builder()
            .handler(ForPath::new(
                "/example/fake/transfer.json",
                StaticResponse::ok_body(
                    r#"
            {
                "version": "1",
                "entries": [{
                    "type": "files",
                    "local": "foo",
                    "remote": "data",
                    "entries": [{ "name": "payload.txt"}]
                }]
            }"#,
                ),
            ))
            .handler(ForPath::new(
                "/data/payload.txt",
                StaticResponse::ok_body(r#"Some fake payload to download."#),
            ))
            .start()
            .await;
        let test_dir = tempfile::TempDir::new().expect("temp dir");
        let download_dir = test_dir.path().join("download");
        let auth = pbms::AuthFlowChoice::NoAuth;
        let force = false;
        let manifest_url = server.local_url_for_path("example/fake/transfer.json");
        let out_dir = test_dir.path().join("download");
        let client = Client::initial().expect("creating client");
        let ui = structured_ui::MockUi::new();
        pb_download_impl(&auth, force, &manifest_url, &out_dir, &client, &ui)
            .await
            .expect("testing download");
        assert!(download_dir.join("foo").join("payload.txt").exists());
    }

    #[fuchsia::test]
    async fn test_preprocess_cmd() {
        let test_dir = tempfile::TempDir::new().expect("temp dir");
        let test_env = TempTestEnv::new().expect("test_env");
        let mut f =
            std::fs::File::create(test_env.home.join("product_bundles.json")).expect("file create");
        f.write_all(
            r#"[{
            "name": "fake_name",
            "product_version": "fake_version",
            "transfer_manifest_url": "fake_url"
            }]"#
            .as_bytes(),
        )
        .expect("write_all");

        let ui = structured_ui::MockUi::new();
        let force = false;
        let manifest_url = String::from("fake_name");
        let auth = pbms::AuthFlowChoice::NoAuth;
        let product_dir = test_dir.path().join("download");
        let base_url = Some(format!("file:{}", test_env.home.display()));
        let version = Some(String::from("fake_version"));
        let cmd = DownloadCommand {
            force,
            auth,
            manifest_url,
            product_dir,
            base_url,
            version,
            branch: None,
        };

        let processed_cmd =
            preprocess_cmd(&cmd.clone(), &ui).await.expect("testing preprocess cmd");

        assert_eq!(
            DownloadCommand { manifest_url: String::from("fake_url"), ..cmd },
            processed_cmd
        );
    }
}
