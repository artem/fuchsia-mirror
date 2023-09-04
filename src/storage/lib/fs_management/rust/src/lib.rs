// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Library for filesystem management in rust.
//!
//! This library is analogous to the fs-management library in zircon. It provides support for
//! formatting, mounting, unmounting, and fsck-ing. It is implemented in a similar way to the C++
//! version.  For components v2, add `/svc/fuchsia.process.Launcher` to `use` and add the
//! binaries as dependencies to your component.

mod error;
pub mod filesystem;
pub mod format;
pub mod partition;

use {
    fidl_fuchsia_fs_startup::{
        CompressionAlgorithm, EvictionPolicyOverride, FormatOptions, StartOptions,
    },
    fuchsia_zircon as zx,
    std::sync::Arc,
};

// Re-export errors as public.
pub use error::{CommandError, KillError, LaunchProcessError, QueryError, ShutdownError};

pub const BLOBFS_TYPE_GUID: [u8; 16] = [
    0x0e, 0x38, 0x67, 0x29, 0x4c, 0x13, 0xbb, 0x4c, 0xb6, 0xda, 0x17, 0xe7, 0xce, 0x1c, 0xa4, 0x5d,
];
pub const DATA_TYPE_GUID: [u8; 16] = [
    0x0c, 0x5f, 0x18, 0x08, 0x2d, 0x89, 0x8a, 0x42, 0xa7, 0x89, 0xdb, 0xee, 0xc8, 0xf5, 0x5e, 0x6a,
];
pub const FVM_TYPE_GUID: [u8; 16] = [
    0xb8, 0x7c, 0xfd, 0x49, 0x15, 0xdf, 0x73, 0x4e, 0xb9, 0xd9, 0x99, 0x20, 0x70, 0x12, 0x7f, 0x0f,
];

pub const FVM_TYPE_GUID_STR: &str = "49fd7cb8-df15-4e73-b9d9-992070127f0f";

pub const FS_COLLECTION_NAME: &'static str = "fs-collection";

#[derive(Clone)]
pub enum ComponentType {
    /// Launch the filesystem as a static child, using the configured name in the options as the
    /// child name. If the child doesn't exist, this will fail.
    StaticChild,

    /// Launch the filesystem as a dynamic child, in the configured collection. By default, the
    /// collection is "fs-collection".
    DynamicChild { collection_name: String },
}

impl Default for ComponentType {
    fn default() -> Self {
        ComponentType::DynamicChild { collection_name: "fs-collection".to_string() }
    }
}

pub struct Options<'a> {
    /// For static children, the name specifies the name of the child.  For dynamic children, the
    /// component URL is "#meta/{component-name}.cm".  The library will attempt to connect to a
    /// static child first, and if that fails, it will launch the filesystem within a collection.
    component_name: &'a str,

    /// It should be possible to reuse components after serving them, but it's not universally
    /// supported.
    reuse_component_after_serving: bool,

    /// Format options as defined by the startup protocol
    format_options: FormatOptions,

    /// Start options as defined by the startup protocol
    start_options: StartOptions,

    /// Whether to launch this filesystem as a dynamic or static child.
    component_type: ComponentType,
}

/// Describes the configuration for a particular filesystem.
pub trait FSConfig: Send + Sync + 'static {
    /// Returns the options specifying how to run this filesystem.
    fn options(&self) -> Options<'_>;

    /// Returns a handle for the crypt service (if any).
    fn crypt_client(&self) -> Option<zx::Channel> {
        // By default, filesystems don't need a crypt service.
        None
    }

    /// Whether the filesystem supports multiple volumes.
    fn is_multi_volume(&self) -> bool {
        false
    }

    fn disk_format(&self) -> format::DiskFormat {
        format::DiskFormat::Unknown
    }
}

///
/// FILESYSTEMS
///

/// Layout of blobs in blobfs
#[derive(Clone)]
pub enum BlobLayout {
    /// Merkle tree is stored in a separate block. This is deprecated and used only on Astro
    /// devices (it takes more space).
    DeprecatedPadded,

    /// Merkle tree is appended to the last block of data
    Compact,
}

/// Compression used for blobs in blobfs
#[derive(Clone)]
pub enum BlobCompression {
    ZSTDChunked,
    Uncompressed,
}

/// Eviction policy used for blobs in blobfs
#[derive(Clone)]
pub enum BlobEvictionPolicy {
    NeverEvict,
    EvictImmediately,
}

/// Blobfs Filesystem Configuration
/// If fields are None or false, they will not be set in arguments.
#[derive(Clone, Default)]
pub struct Blobfs {
    // Format options
    pub verbose: bool,
    pub deprecated_padded_blobfs_format: bool,
    pub num_inodes: u64,
    // Start Options
    pub readonly: bool,
    pub write_compression_algorithm: Option<BlobCompression>,
    pub write_compression_level: Option<i32>,
    pub cache_eviction_policy_override: Option<BlobEvictionPolicy>,
    pub component_type: ComponentType,
    pub allow_delivery_blobs: bool,
}

impl Blobfs {
    /// Manages a block device using the default configuration.
    pub fn new(block_device: fidl_fuchsia_device::ControllerProxy) -> filesystem::Filesystem {
        filesystem::Filesystem::new(block_device, Self::default())
    }

    /// Launch blobfs, with the default configuration, as a dynamic child in the fs-collection.
    pub fn dynamic_child() -> Self {
        Self {
            component_type: ComponentType::DynamicChild {
                collection_name: FS_COLLECTION_NAME.to_string(),
            },
            ..Default::default()
        }
    }
}

impl FSConfig for Blobfs {
    fn options(&self) -> Options<'_> {
        Options {
            component_name: "blobfs",
            reuse_component_after_serving: false,
            format_options: FormatOptions {
                verbose: Some(self.verbose),
                deprecated_padded_blobfs_format: Some(self.deprecated_padded_blobfs_format),
                num_inodes: if self.num_inodes > 0 { Some(self.num_inodes) } else { None },
                ..Default::default()
            },
            start_options: {
                let mut start_options = StartOptions {
                    read_only: self.readonly,
                    verbose: self.verbose,
                    fsck_after_every_transaction: false,
                    write_compression_level: self.write_compression_level.unwrap_or(-1),
                    write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
                    cache_eviction_policy_override: EvictionPolicyOverride::None,
                    allow_delivery_blobs: self.allow_delivery_blobs,
                };
                if let Some(compression) = &self.write_compression_algorithm {
                    start_options.write_compression_algorithm = match compression {
                        BlobCompression::ZSTDChunked => CompressionAlgorithm::ZstdChunked,
                        BlobCompression::Uncompressed => CompressionAlgorithm::Uncompressed,
                    }
                }
                if let Some(eviction) = &self.cache_eviction_policy_override {
                    start_options.cache_eviction_policy_override = match eviction {
                        BlobEvictionPolicy::NeverEvict => EvictionPolicyOverride::NeverEvict,
                        BlobEvictionPolicy::EvictImmediately => {
                            EvictionPolicyOverride::EvictImmediately
                        }
                    }
                }
                start_options
            },
            component_type: self.component_type.clone(),
        }
    }

    fn disk_format(&self) -> format::DiskFormat {
        format::DiskFormat::Blobfs
    }
}

/// Minfs Filesystem Configuration
/// If fields are None or false, they will not be set in arguments.
#[derive(Clone, Default)]
pub struct Minfs {
    // TODO(xbhatnag): Add support for fvm_data_slices
    // Format options
    pub verbose: bool,
    pub fvm_data_slices: u32,
    // Start Options
    pub readonly: bool,
    pub fsck_after_every_transaction: bool,
    pub component_type: ComponentType,
}

impl Minfs {
    /// Manages a block device using the default configuration.
    pub fn new(block_device: fidl_fuchsia_device::ControllerProxy) -> filesystem::Filesystem {
        filesystem::Filesystem::new(block_device, Self::default())
    }

    /// Launch minfs, with the default configuration, as a dynamic child in the fs-collection.
    pub fn dynamic_child() -> Self {
        Self {
            component_type: ComponentType::DynamicChild {
                collection_name: FS_COLLECTION_NAME.to_string(),
            },
            ..Default::default()
        }
    }
}

impl FSConfig for Minfs {
    fn options(&self) -> Options<'_> {
        Options {
            component_name: "minfs",
            reuse_component_after_serving: false,
            format_options: FormatOptions {
                verbose: Some(self.verbose),
                fvm_data_slices: Some(self.fvm_data_slices),
                ..Default::default()
            },
            start_options: StartOptions {
                read_only: self.readonly,
                verbose: self.verbose,
                fsck_after_every_transaction: self.fsck_after_every_transaction,
                write_compression_level: -1,
                write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
                cache_eviction_policy_override: EvictionPolicyOverride::None,
                allow_delivery_blobs: false,
            },
            component_type: self.component_type.clone(),
        }
    }

    fn disk_format(&self) -> format::DiskFormat {
        format::DiskFormat::Minfs
    }
}

pub type CryptClientFn = Arc<dyn Fn() -> zx::Channel + Send + Sync>;

/// Fxfs Filesystem Configuration
#[derive(Clone)]
pub struct Fxfs {
    // This is only used by fsck.
    pub crypt_client_fn: Option<CryptClientFn>,
    // Start Options
    pub readonly: bool,
    pub fsck_after_every_transaction: bool,
    pub component_type: ComponentType,
    pub allow_delivery_blobs: bool,
}

impl Default for Fxfs {
    fn default() -> Self {
        Self {
            crypt_client_fn: None,
            readonly: false,
            fsck_after_every_transaction: false,
            component_type: Default::default(),
            allow_delivery_blobs: true,
        }
    }
}

impl Fxfs {
    pub fn with_crypt_client(crypt_client_fn: CryptClientFn) -> Self {
        Fxfs { crypt_client_fn: Some(crypt_client_fn), ..Default::default() }
    }

    /// Manages a block device using the default configuration.
    pub fn new(block_device: fidl_fuchsia_device::ControllerProxy) -> filesystem::Filesystem {
        filesystem::Filesystem::new(block_device, Self::default())
    }

    /// Launch Fxfs, with the default configuration, as a dynamic child in the fs-collection.
    pub fn dynamic_child() -> Self {
        Self {
            component_type: ComponentType::DynamicChild {
                collection_name: FS_COLLECTION_NAME.to_string(),
            },
            ..Default::default()
        }
    }
}

impl FSConfig for Fxfs {
    fn options(&self) -> Options<'_> {
        Options {
            component_name: "fxfs",
            reuse_component_after_serving: true,
            format_options: FormatOptions { verbose: Some(false), ..Default::default() },
            start_options: StartOptions {
                read_only: self.readonly,
                verbose: false,
                fsck_after_every_transaction: self.fsck_after_every_transaction,
                write_compression_level: -1,
                write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
                cache_eviction_policy_override: EvictionPolicyOverride::None,
                allow_delivery_blobs: self.allow_delivery_blobs,
            },
            component_type: self.component_type.clone(),
        }
    }

    fn crypt_client(&self) -> Option<zx::Channel> {
        self.crypt_client_fn.as_ref().map(|f| f())
    }

    fn is_multi_volume(&self) -> bool {
        true
    }

    fn disk_format(&self) -> format::DiskFormat {
        format::DiskFormat::Fxfs
    }
}

/// F2fs Filesystem Configuration
/// If fields are None or false, they will not be set in arguments.
#[derive(Clone, Default)]
pub struct F2fs {
    pub component_type: ComponentType,
}

impl F2fs {
    /// Manages a block device using the default configuration.
    pub fn new(block_device: fidl_fuchsia_device::ControllerProxy) -> filesystem::Filesystem {
        filesystem::Filesystem::new(block_device, Self::default())
    }

    /// Launch f2fs, with the default configuration, as a dynamic child in the fs-collection.
    pub fn dynamic_child() -> Self {
        Self {
            component_type: ComponentType::DynamicChild {
                collection_name: FS_COLLECTION_NAME.to_string(),
            },
            ..Default::default()
        }
    }
}

impl FSConfig for F2fs {
    fn options(&self) -> Options<'_> {
        Options {
            component_name: "f2fs",
            reuse_component_after_serving: false,
            format_options: FormatOptions::default(),
            start_options: StartOptions {
                read_only: false,
                verbose: false,
                fsck_after_every_transaction: false,
                write_compression_level: -1,
                write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
                cache_eviction_policy_override: EvictionPolicyOverride::None,
                allow_delivery_blobs: false,
            },
            component_type: self.component_type.clone(),
        }
    }
    fn is_multi_volume(&self) -> bool {
        false
    }

    fn disk_format(&self) -> format::DiskFormat {
        format::DiskFormat::F2fs
    }
}
