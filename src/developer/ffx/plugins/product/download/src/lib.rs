// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - acquire and display product bundle information (metadata)
//! - acquire related data files, such as disk partition images (data)

use ::gcs::client::{Client, ProgressResponse, ProgressState};
use anyhow::{anyhow, Context, Result};
use async_fs::rename;
use errors::ffx_bail;
use ffx_config::sdk::SdkVersion;
use ffx_core::ffx_plugin;
use ffx_product_download_args::DownloadCommand;
use ffx_product_list::pb_list_impl;
use pbms::{make_way_for_output, transfer_download, AuthFlowChoice};
use std::{
    io::{stderr, stdin, stdout},
    path::Path,
};
use structured_ui;

/// `ffx product download` sub-command.
#[ffx_plugin()]
pub async fn pb_download(cmd: DownloadCommand) -> Result<()> {
    let client = Client::initial()?;
    let mut input = stdin();
    let mut output = stdout();
    let mut err_out = stderr();
    let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);

    let cmd = preprocess_cmd(cmd, &ui).await?;

    pb_download_impl(&cmd.auth, cmd.force, &cmd.manifest_url, &cmd.product_dir, &client, &ui).await
}

pub async fn pb_download_impl<I: structured_ui::Interface + Sync>(
    auth: &AuthFlowChoice,
    force: bool,
    manifest_url: &str,
    product_dir: &Path,
    client: &Client,
    ui: &I,
) -> Result<()> {
    let start = std::time::Instant::now();
    tracing::info!("---------------------- Begin ----------------------------");
    tracing::debug!("transfer_manifest_url Url::parse");
    let transfer_manifest_url = match url::Url::parse(manifest_url) {
        Ok(p) => p,
        _ => ffx_bail!("The source location must be a URL, failed to parse {:?}", manifest_url),
    };
    tracing::debug!("make_way_for_output {:?}", product_dir);
    make_way_for_output(&product_dir, force).await?;

    let parent_dir = product_dir.parent().ok_or_else(|| anyhow!("local dir has no parent"))?;
    let temp_dir = tempfile::TempDir::new_in(&parent_dir)?;
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

pub async fn preprocess_cmd<I: structured_ui::Interface + Sync>(
    cmd: DownloadCommand,
    ui: &I,
) -> Result<DownloadCommand> {
    // If the manifest_url is a transfer url, we don't need to preprocess.
    if let Ok(_) = url::Url::parse(&cmd.manifest_url) {
        return Ok(cmd);
    };

    // If the manifest_url look like a product name, we try to convert it into a
    // transfer manifest url.
    let version = match cmd.version {
        Some(version) => version,
        None => {
            let sdk = ffx_config::global_env_context()
                .context("loading global environment context")?
                .get_sdk()
                .await
                .context("getting sdk env context")?;
            match sdk.get_version() {
                SdkVersion::Version(version) => version.to_string(),
                SdkVersion::InTree => {
                    ffx_bail!("Using in-tree sdk. Please specify the version through '--version'")
                }
                SdkVersion::Unknown => ffx_bail!("Unable to determine SDK version. Please specify the version through '--version'"),
            }
        }
    };
    let products = pb_list_impl(&cmd.auth, cmd.base_url.clone(), &version, ui)
        .await?
        .iter()
        .cloned()
        .filter(|x| x.name == cmd.manifest_url)
        .collect::<Vec<_>>();

    if products.len() != 1 {
        ffx_bail!(
            "Expected a single product entry while trying to download a product by name, found {}",
            products.len()
        );
    }

    let processed_cmd = DownloadCommand {
        manifest_url: products[0].transfer_manifest_url.clone(),
        version: Some(version),
        ..cmd
    };
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
    use tempfile;

    #[fuchsia_async::run_singlethreaded(test)]
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

    #[fuchsia_async::run_singlethreaded(test)]
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

    #[fuchsia_async::run_singlethreaded(test)]
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
        let cmd = DownloadCommand { force, auth, manifest_url, product_dir, base_url, version };

        let processed_cmd = preprocess_cmd(cmd.clone(), &ui).await.expect("testing preprocess cmd");

        assert_eq!(
            DownloadCommand { manifest_url: String::from("fake_url"), ..cmd },
            processed_cmd
        );
    }
}
