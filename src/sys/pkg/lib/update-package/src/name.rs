// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Typesafe wrappers around verifying the name of the update package.

use {
    fidl_fuchsia_io as fio,
    fuchsia_pkg::{MetaPackage, MetaPackageError},
    thiserror::Error,
};

/// An error encountered while verifying the board.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum VerifyNameError {
    #[error("while opening meta/package")]
    OpenMetaPackage(#[source] fuchsia_fs::node::OpenError),

    #[error("while reading meta/package")]
    ReadMetaPackage(#[source] fuchsia_fs::file::ReadError),

    #[error("while reading meta/package")]
    ParseMetaPackage(#[source] MetaPackageError),

    #[error("expected package name 'update/0' found '{0:?}'")]
    Invalid(MetaPackage),
}

pub(crate) async fn verify(proxy: &fio::DirectoryProxy) -> Result<(), VerifyNameError> {
    let file =
        fuchsia_fs::directory::open_file(proxy, "meta/package", fio::OpenFlags::RIGHT_READABLE)
            .await
            .map_err(VerifyNameError::OpenMetaPackage)?;
    let contents = fuchsia_fs::file::read(&file).await.map_err(VerifyNameError::ReadMetaPackage)?;

    let expected = MetaPackage::from_name_and_variant_zero("update".parse().unwrap());

    let actual =
        MetaPackage::deserialize(&mut &contents[..]).map_err(VerifyNameError::ParseMetaPackage)?;

    if expected != actual {
        return Err(VerifyNameError::Invalid(actual));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::TestUpdatePackage,
        assert_matches::assert_matches,
        fuchsia_pkg::{PackageName, PackageVariant},
    };

    fn make_meta_package(name: &str) -> Vec<u8> {
        let meta_package = MetaPackage::from_name_and_variant_zero(name.parse().unwrap());
        let mut bytes = vec![];
        let () = meta_package.serialize(&mut bytes).unwrap();
        bytes
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn allows_expected_name_and_variant() {
        assert_matches!(
            TestUpdatePackage::new()
                .add_file("meta/package", make_meta_package("update"))
                .await
                .verify_name()
                .await,
            Ok(())
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn rejects_unexpected_name() {
        assert_matches!(
            TestUpdatePackage::new()
                .add_file("meta/package", make_meta_package("invalid"))
                .await
                .verify_name()
                .await,
            Err(VerifyNameError::Invalid(actual))
                if actual == MetaPackage::from_name_and_variant_zero("invalid".parse().unwrap())
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn rejects_unexpected_variant() {
        let name: PackageName = "invalid".parse().unwrap();
        let variant: PackageVariant = "42".parse().unwrap();
        assert_matches!(
            TestUpdatePackage::new()
                .add_file("meta/package", br#"{"name":"invalid","version":"42"}"#)
                .await
                .verify_name()
                .await,
            Err(VerifyNameError::Invalid(actual))
                if actual.name() == &name && actual.variant() == &variant
        );
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn rejects_invalid_meta_package() {
        assert_matches!(
            TestUpdatePackage::new().add_file("meta/package", "bad json").await.verify_name().await,
            Err(VerifyNameError::ParseMetaPackage(_))
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn rejects_missing_meta_package() {
        assert_matches!(
            TestUpdatePackage::new().verify_name().await,
            Err(VerifyNameError::OpenMetaPackage(_)) | Err(VerifyNameError::ReadMetaPackage(_))
        );
    }
}
