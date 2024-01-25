// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Private functionality for pbms lib.

use crate::{gcs::fetch_from_gcs, AuthFlowChoice};
use ::gcs::client::{
    Client, DirectoryProgress, FileProgress, ProgressResponse, ProgressResult, Throttle,
};
use anyhow::{bail, Context, Result};
use async_fs::File;
use futures::{AsyncWriteExt as _, TryStreamExt as _};
use hyper::{header::CONTENT_LENGTH, StatusCode};
use std::path::{Path, PathBuf};
use structured_ui;

pub(crate) const CONFIG_STORAGE_PATH: &str = "pbms.storage.path";
pub(crate) const GS_SCHEME: &str = "gs";

/// Retrieve the path portion of a "file:/" url. Non-file-paths return None.
///
/// If the url has no scheme, the whole string is returned.
/// E.g.
/// - "/foo/bar" -> Some("/foo/bar")
/// - "file://foo/bar" -> Some("/foo/bar")
/// - "http://foo/bar" -> None
pub(crate) fn path_from_file_url(product_url: &url::Url) -> Option<PathBuf> {
    if product_url.scheme() == "file" {
        product_url.to_file_path().ok()
    } else {
        None
    }
}

/// Helper function for determining local path.
///
/// if `dir` return a directory path, else may return a glob (file) path.
pub(crate) async fn local_path_helper(
    product_url: &url::Url,
    add_dir: &str,
    dir: bool,
    sdk_root: &Path,
) -> Result<PathBuf> {
    assert!(!product_url.fragment().is_none());
    if let Some(path) = &path_from_file_url(product_url) {
        if dir {
            // TODO(https://fxbug.dev/42180298): Unify the file layout between local and remote
            // product bundles to avoid this hack.
            if path.starts_with(sdk_root) {
                Ok(sdk_root.to_path_buf())
            } else {
                Ok(path.parent().expect("parent of file path").to_path_buf())
            }
        } else {
            Ok(path.to_path_buf())
        }
    } else {
        let url = url_sans_fragment(&product_url)?;
        Ok(get_product_dir(&url).await?.join(add_dir))
    }
}

/// Retrieve the storage directory path from the config.
pub async fn get_storage_dir() -> Result<PathBuf> {
    let storage_path: PathBuf =
        ffx_config::get(CONFIG_STORAGE_PATH).await.context("getting CONFIG_STORAGE_PATH")?;
    Ok(storage_path)
}

/// Retrieve the product directory path from the config.
///
/// This is the storage path plus a hash of the `product_url` provided.
pub async fn get_product_dir(product_url: &url::Url) -> Result<PathBuf> {
    Ok(get_storage_dir().await?.join(pb_dir_name(product_url)))
}

/// Separate the URL on the last "#" character.
///
/// If no "#" is found, use the whole input as the url.
///
/// "file://foo#bar" -> "file://foo"
/// "file://foo" -> "file://foo"
pub(crate) fn url_sans_fragment(product_url: &url::Url) -> Result<url::Url> {
    let mut product_url = product_url.to_owned();
    product_url.set_fragment(None);
    Ok(product_url)
}

/// Generate a (likely) unique name for the URL.
///
/// URLs don't always make good file paths.
pub(crate) fn pb_dir_name(gcs_url: &url::Url) -> String {
    let mut gcs_url = gcs_url.to_owned();
    gcs_url.set_fragment(None);

    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };
    let mut s = DefaultHasher::new();
    gcs_url.as_str().hash(&mut s);
    let out = s.finish();
    tracing::debug!("pb_dir_name {:?}, hash {:?}", gcs_url, out);
    format!("{}", out)
}

/// Download data from any of the supported schemes listed in RFC-100, Product
/// Bundle, "bundle_uri".
///
/// Currently: "pattern": "^(?:http|https|gs|file):\/\/"
pub(crate) async fn fetch_from_url<F, I>(
    product_url: &url::Url,
    local_dir: PathBuf,
    auth_flow: &AuthFlowChoice,
    progress: &F,
    ui: &I,
    client: &Client,
) -> Result<()>
where
    F: Fn(DirectoryProgress<'_>, FileProgress<'_>) -> ProgressResult,
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("fetch_from_url {:?}", product_url);
    if product_url.scheme() == GS_SCHEME {
        fetch_from_gcs(product_url.as_str(), &local_dir, auth_flow, progress, ui, client)
            .await
            .context("Downloading from GCS.")?;
    } else if product_url.scheme() == "http" || product_url.scheme() == "https" {
        fetch_from_web(product_url, &local_dir, progress, ui)
            .await
            .context("fetching from http(s)")?;
    } else if let Some(_) = &path_from_file_url(product_url) {
        // Since the file is already local, no fetch is necessary.
        tracing::debug!("Found local file path {:?}", product_url);
    } else {
        bail!("Unexpected URI scheme in ({:?})", product_url);
    }
    Ok(())
}

async fn fetch_from_web<F, I>(
    product_uri: &url::Url,
    local_dir: &Path,
    progress: &F,
    _ui: &I,
) -> Result<()>
where
    F: Fn(DirectoryProgress<'_>, FileProgress<'_>) -> ProgressResult,
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("fetch_from_web");
    let name = if let Some((_, name)) = product_uri.path().rsplit_once('/') {
        name
    } else {
        unimplemented!()
    };

    if name.is_empty() {
        unimplemented!("downloading a directory from a web server is not implemented");
    }

    let res = fuchsia_hyper::new_client()
        .get(hyper::Uri::from_maybe_shared(product_uri.to_string())?)
        .await
        .with_context(|| format!("Requesting {}", product_uri))?;

    match res.status() {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => {
            bail!("{} not found", product_uri);
        }
        status => {
            bail!("Unexpected HTTP status downloading {}: {}", product_uri, status);
        }
    }

    let mut at: u64 = 0;
    let length = if res.headers().contains_key(CONTENT_LENGTH) {
        res.headers()
            .get(CONTENT_LENGTH)
            .context("getting content length")?
            .to_str()?
            .parse::<u64>()
            .context("parsing content length")?
    } else {
        0
    };

    std::fs::create_dir_all(local_dir)
        .with_context(|| format!("Creating {}", local_dir.display()))?;

    let path = local_dir.join(name);
    let mut file =
        File::create(&path).await.with_context(|| format!("Creating {}", path.display()))?;

    let mut stream = res.into_body();

    let mut of = length;
    // Throttle the progress UI updates to avoid burning CPU on changes
    // the user will have trouble seeing anyway. Without throttling,
    // around 20% of the execution time can be spent updating the
    // progress UI. The throttle makes the overhead negligible.
    let mut throttle = Throttle::from_duration(std::time::Duration::from_millis(500));
    let url = product_uri.to_string();
    while let Some(chunk) =
        stream.try_next().await.with_context(|| format!("Downloading {}", product_uri))?
    {
        file.write_all(&chunk).await.with_context(|| format!("Writing {}", path.display()))?;
        at += chunk.len() as u64;
        if at > of {
            of = at;
        }
        if throttle.is_ready() {
            match progress(
                DirectoryProgress { name: &url, at: 0, of: 1, units: "files" },
                FileProgress { name: &url, at, of, units: "bytes" },
            )
            .context("rendering progress")?
            {
                ProgressResponse::Cancel => break,
                _ => (),
            }
        }
    }

    file.close().await.with_context(|| format!("Closing {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_path_from_file_url() {
        let input = url::Url::parse("fake://foo#bar").expect("url");
        let output = path_from_file_url(&input);
        assert!(output.is_none());

        let input = url::Url::parse("file:///../../foo#bar").expect("url");
        let output = path_from_file_url(&input);
        assert_eq!(output, Some(Path::new("/foo").to_path_buf()));

        let input = url::Url::parse("file://foo#bar").expect("url");
        let output = path_from_file_url(&input);
        assert!(output.is_none());

        let input = url::Url::parse("file:///foo#bar").expect("url");
        let output = path_from_file_url(&input);
        assert_eq!(output, Some(Path::new("/foo").to_path_buf()));

        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let base_url = url::Url::from_directory_path(temp_dir.path().join("a/b/c/d")).expect("url");
        let input =
            url::Url::options().base_url(Some(&base_url)).parse("../../foo#bar").expect("url");
        let output = path_from_file_url(&input);
        assert_eq!(output, Some(temp_dir.path().join("a/b/foo").to_path_buf()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_url_sans_fragment() {
        let input = url::Url::parse("fake://foo#bar").expect("url");
        let output = url_sans_fragment(&input).expect("sans fragment");
        assert_eq!(output, url::Url::parse("fake://foo").expect("check url"));

        let input = url::Url::parse("fake://foo").expect("url");
        let output = url_sans_fragment(&input).expect("sans fragment");
        assert_eq!(output, url::Url::parse("fake://foo").expect("check url"));
    }

    // Disabling this test until a test config can be modified without altering
    // the local user's config.
    #[ignore]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_local_path_helper() {
        let sdk_prefix = PathBuf::from("/"); // this is only used for file paths
        let url = url::Url::parse("fake://foo#bar").expect("url");
        let path =
            local_path_helper(&url, "foo", /*dir=*/ true, &sdk_prefix).await.expect("dir helper");
        assert!(path.to_string_lossy().ends_with("ffx/pbms/951333825719265977/foo"));

        // Note that the hash will be the same even though the fragment is
        // different.
        let url = url::Url::parse("fake://foo#blah").expect("url");
        let path =
            local_path_helper(&url, "foo", /*dir=*/ true, &sdk_prefix).await.expect("dir helper");
        assert!(path.to_string_lossy().ends_with("ffx/pbms/951333825719265977/foo"));

        let url = url::Url::parse("gs://foo/blah/*.json#bar").expect("url");
        let path =
            local_path_helper(&url, "foo", /*dir=*/ true, &sdk_prefix).await.expect("dir helper");
        assert!(path.to_string_lossy().ends_with("ffx/pbms/16042545670964745983/foo"));

        let url = url::Url::parse("file:///foo/blah/*.json#bar").expect("url");
        let path =
            local_path_helper(&url, "foo", /*dir=*/ true, &sdk_prefix).await.expect("dir helper");
        assert_eq!(path.to_string_lossy(), "/foo/blah");

        let url = url::Url::parse("file:///foo/blah/*.json#bar").expect("url");
        let path =
            local_path_helper(&url, "foo", /*dir=*/ false, &sdk_prefix).await.expect("dir helper");
        assert_eq!(path.to_string_lossy(), "/foo/blah/*.json");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    #[should_panic(expected = "Unexpected URI scheme")]
    async fn test_fetch_from_url() {
        let url = url::Url::parse("fake://foo").expect("url");
        let ui = structured_ui::MockUi::new();
        let client = Client::initial().expect("creating client");
        fetch_from_url(
            &url,
            Path::new("unused").to_path_buf(),
            &AuthFlowChoice::Default,
            &|_d, _f| Ok(ProgressResponse::Continue),
            &ui,
            &client,
        )
        .await
        .expect("bad fetch");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_pb_dir_name() {
        let url = url::Url::parse("fake://foo").expect("url");
        let hash = pb_dir_name(&url);
        assert!(url.as_str() != hash);
        assert!(!hash.contains("/"));
        assert!(!hash.contains(" "));
    }
}
