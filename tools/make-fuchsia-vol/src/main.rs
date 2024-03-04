// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{bail, ensure, Context, Error},
    argh::FromArgs,
    assembly_partitions_config::{Partition, PartitionsConfig, Slot},
    byteorder::{BigEndian, WriteBytesExt},
    camino::{Utf8Path, Utf8PathBuf},
    crc::crc32,
    fatfs::{FsOptions, NullTimeProvider, OemCpConverter, TimeProvider},
    gpt::{
        partition_types::{OperatingSystem, Type as PartType},
        GptDisk,
    },
    rand::{RngCore, SeedableRng},
    rand_xorshift::XorShiftRng,
    sdk_metadata::{LoadedProductBundle, ProductBundle},
    serde::{Deserialize, Serialize},
    std::{
        collections::{BTreeMap, HashMap},
        fs::{File, OpenOptions},
        io::{BufReader, Read, Seek, SeekFrom, Write},
        ops::Range,
        os::unix::fs::FileExt,
        process::Command,
        str::FromStr,
    },
    zerocopy::{AsBytes, NoCell},
};

const fn part_type(guid: &'static str) -> PartType {
    PartType { guid, os: OperatingSystem::None }
}

const ZIRCON_A_GUID: PartType = part_type("DE30CC86-1F4A-4A31-93C4-66F147D33E05");
const ZIRCON_B_GUID: PartType = part_type("23CC04DF-C278-4CE7-8471-897D1A4BCDF7");
const ZIRCON_R_GUID: PartType = part_type("A0E5CF57-2DEF-46BE-A80C-A2067C37CD49");
const VBMETA_A_GUID: PartType = part_type("A13B4D9A-EC5F-11E8-97D8-6C3BE52705BF");
const VBMETA_B_GUID: PartType = part_type("A288ABF2-EC5F-11E8-97D8-6C3BE52705BF");
const VBMETA_R_GUID: PartType = part_type("6A2460C3-CD11-4E8B-80A8-12CCE268ED0A");
const MISC_GUID: PartType = part_type("1D75395D-F2C6-476B-A8B7-45CC1C97B476");
const INSTALLER_GUID: PartType = part_type("4DCE98CE-E77E-45C1-A863-CAF92F1330C1");
const FVM_GUID: PartType = part_type("41D0E340-57E3-954E-8C1E-17ECAC44CFF5");

const ZBI_ZIRCON_A: &str = "zbi_zircon-a";
const ZBI_ZIRCON_R: &str = "zbi_zircon-r";
const BLK_STORAGE_SPARSE: &str = "blk_storage-sparse";
const FXFS_BLK_STORAGE_FULL: &str = "fxfs-blk_storage-full";
const BLK_BLOB: &str = "blk_blob";

// The relevant part of the product bundle metadata schema, as realized by
// entries in the product_bundles.json build API module.
#[derive(Deserialize, Serialize)]
struct ProductBundleMetadata {
    name: String,
    path: Utf8PathBuf,
}

/// make-fuchsia-vol
#[derive(FromArgs, Debug)]
struct TopLevel {
    /// disk-path
    #[argh(positional)]
    disk_path: Utf8PathBuf,

    /// enable verbose logging
    #[argh(switch)]
    verbose: bool,

    /// fuchsia build dir
    #[argh(option)]
    fuchsia_build_dir: Option<Utf8PathBuf>,

    /// use named product information from Product Bundle Metadata (PBM). If no
    /// product bundle is specified and there is an obvious choice, that will be
    /// used (e.g. if there is only one PBM available).
    #[argh(option)]
    product_bundle: Option<Utf8PathBuf>,

    /// the name of a particular product bundle to use, consulted if
    /// --product-bundle is unset.
    #[argh(option)]
    product_bundle_name: Option<String>,

    /// the architecture of the target CPU (x64|arm64)
    #[argh(option, default = "Arch::X64")]
    arch: Arch,

    /// the architecture of the host CPU (x64|arm64)
    #[argh(option, default = "Arch::X64")]
    host_arch: Arch,

    /// path to fuchsia-efi.efi
    #[argh(option)]
    bootloader: Option<Utf8PathBuf>,

    /// path to zbi (default: zircon-a from image manifests)
    #[argh(option)]
    zbi: Option<Utf8PathBuf>,

    /// path to command line file (if exists)
    #[argh(option)]
    cmdline: Option<Utf8PathBuf>,

    /// path to zedboot.zbi (default: zircon-r from image manifests)
    #[argh(option)]
    zedboot: Option<Utf8PathBuf>,

    /// ramdisk-only mode - only write an ESP partition
    #[argh(switch)]
    ramdisk_only: bool,

    /// path to blob partition image (not used with ramdisk)
    #[argh(option)]
    blob: Option<Utf8PathBuf>,

    /// if true, use sparse fvm instead of full fvm
    #[argh(switch)]
    use_sparse_fvm: bool,

    /// path to sparse FVM image (default: storage-sparse from image manifests)
    #[argh(option)]
    sparse_fvm: Option<Utf8PathBuf>,

    /// don't add Zircon-{{A,B,R}} partitions
    #[argh(switch)]
    no_abr: bool,

    /// path to partition image for Zircon-A (default: from --zbi)
    #[argh(option)]
    zircon_a: Option<Utf8PathBuf>,

    /// path to partition image for Vbmeta-A
    #[argh(option)]
    vbmeta_a: Option<Utf8PathBuf>,

    /// path to partition image for Zircon-B (default: from --zbi)
    #[argh(option)]
    zircon_b: Option<Utf8PathBuf>,

    /// path to partition image for Vbmeta-B
    #[argh(option)]
    vbmeta_b: Option<Utf8PathBuf>,

    /// path to partition image for Zircon-R (default: zircon-r from image manifests)
    #[argh(option)]
    zircon_r: Option<Utf8PathBuf>,

    /// path to partition image for Vbmeta-R
    #[argh(option)]
    vbmeta_r: Option<Utf8PathBuf>,

    /// kernel partition size for A/B/R
    #[argh(option, default = "256 * 1024 * 1024")]
    abr_size: u64,

    /// partition size for vbmeta A/B/R
    #[argh(option, default = "64 * 1024")]
    vbmeta_size: u64,

    /// A/B/R partition to boot by default
    #[argh(option, default = "BootPart::BootA")]
    abr_boot: BootPart,

    /// the block size of the target disk
    #[argh(option)]
    block_size: Option<u64>,

    /// efi partition size in bytes
    #[argh(option, default = "63 * 1024 * 1024")]
    efi_size: u64,

    /// system (i.e. FVM or Fxfs) disk partition size in bytes (unspecified means `fill`)
    #[argh(option)]
    system_disk_size: Option<u64>,

    /// create or resize the image to this size in bytes
    #[argh(option)]
    resize: Option<u64>,

    /// a seed from which the UUIDs are derived; this will also set any timestamps used to fixed
    /// values, so the resulting image should be reproducible (only suitable for TESTING).
    #[argh(option)]
    seed: Option<String>,

    /// if true, write a dependency file which will be the same as the output path but with a '.d'
    /// extension.
    #[argh(switch)]
    depfile: bool,

    /// whether to use Fxfs instead of FVM to store the base system
    #[argh(switch)]
    use_fxfs: bool,

    /// path to Fxfs partition image (if --use_fxfs is set)
    #[argh(option)]
    fxfs: Option<Utf8PathBuf>,
}

#[derive(Debug)]
enum Arch {
    X64,
    Arm64,
}

impl FromStr for Arch {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "x64" => Ok(Arch::X64),
            "arm64" => Ok(Arch::Arm64),
            _ => Err("Expected x64 or arm64".to_string()),
        }
    }
}

// Shift takes something that implements Read + Write + Seek and offsets it by `offset`.  This is
// used to allow fatfs to read the ESP partition.  There are no checks to ensure that fatfs doesn't
// escape the partition, but that shouldn't happen.
struct Shift<T> {
    inner: T,
    offset: u64,
}

impl<T: Seek> Shift<T> {
    fn new(mut inner: T, offset: u64) -> Self {
        assert_eq!(inner.seek(SeekFrom::Start(offset)).unwrap(), offset);
        Self { inner, offset }
    }
}

impl<T: Read> Read for Shift<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<T: Write> Write for Shift<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<T: Seek> Seek for Shift<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::Start(offset) => {
                Ok(self.inner.seek(SeekFrom::Start(offset + self.offset))? - self.offset)
            }
            SeekFrom::End(_) => panic!("Not supported"),
            SeekFrom::Current(offset) => {
                Ok(self.inner.seek(SeekFrom::Current(offset))? - self.offset)
            }
        }
    }
}

fn get_product_bundle_partitions(path: &Utf8Path) -> Result<PartitionsConfig, Error> {
    let product_bundle = LoadedProductBundle::try_load_from(path)?;

    match product_bundle.into() {
        ProductBundle::V2(pb) => Ok(pb.partitions),
    }
}

fn get_bootloader_partition_name(partitions: &Option<PartitionsConfig>) -> Result<&str, Error> {
    if let Some(partitions) = &partitions {
        match &partitions.bootloader_partitions[..] {
            [bootloader_partition] => {
                if let Some(name) = &bootloader_partition.name {
                    return Ok(name);
                } else {
                    bail!("product bundle bootloader partition doesn't have a name");
                }
            }
            [] => bail!("product bundle doesn't have any bootloader partitions"),
            _ => bail!("product bundles with multiple bootloader partitions is not supported"),
        }
    }

    Ok("fuchsia-esp")
}

fn get_zbi_partition_name(
    partitions: &Option<PartitionsConfig>,
    partition_slot: Slot,
) -> Result<&str, Error> {
    let Some(partitions) = partitions else {
        return Ok(match partition_slot {
            Slot::A => "zircon-a",
            Slot::B => "zircon-b",
            Slot::R => "zircon-r",
        });
    };

    for partition in &partitions.partitions {
        if let Partition::ZBI { name, slot } = &partition {
            if slot == &partition_slot {
                return Ok(name);
            }
        }
    }

    bail!("could not find ZBI partition name for slot {partition_slot:?}")
}

fn get_vbmeta_partition_name(
    partitions: &Option<PartitionsConfig>,
    partition_slot: Slot,
) -> Result<&str, Error> {
    let Some(partitions) = partitions else {
        return Ok(match partition_slot {
            Slot::A => "vbmeta_a",
            Slot::B => "vbmeta_b",
            Slot::R => "vbmeta_r",
        });
    };

    for partition in &partitions.partitions {
        if let Partition::VBMeta { name, slot } = &partition {
            if slot == &partition_slot {
                return Ok(name);
            }
        }
    }

    bail!("could not find ZBI partition name for slot {partition_slot:?}")
}

fn get_image(images: &HashMap<String, Utf8PathBuf>, name: &str) -> Result<Utf8PathBuf, Error> {
    if let Some(name) = images.get(name) {
        Ok(name.clone())
    } else {
        bail!("images missing entry for '{}'", name)
    }
}

fn main() -> Result<(), Error> {
    run(argh::from_env::<TopLevel>())
}

fn run(mut args: TopLevel) -> Result<(), Error> {
    check_args(&mut args)?;

    let product_bundle_partitions = if let Some(pbms_path) = &args.product_bundle {
        Some(get_product_bundle_partitions(pbms_path)?)
    } else {
        None
    };

    let mut disk = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(args.resize.is_some())
        .create(args.resize.is_some())
        .open(&args.disk_path)
        .context(format!("Failed to open output file {}", args.disk_path))?;

    let disk_size = if let Some(size) = args.resize {
        disk.set_len(size)?;
        size
    } else {
        disk.seek(SeekFrom::End(0))?
    };

    let mut config = gpt::GptConfig::new().writable(true).initialized(false);
    if let Some(block_size) = args.block_size {
        config = config.logical_block_size(block_size.try_into()?);
    }
    let mut gpt_disk = config.create_from_device(Box::new(&mut disk), None)?;
    gpt_disk.update_partitions(BTreeMap::new())?;

    let efi_partition_name = get_bootloader_partition_name(&product_bundle_partitions)?;
    let efi_part =
        add_partition(&mut gpt_disk, efi_partition_name, args.efi_size, gpt::partition_types::EFI)?;

    struct Partitions {
        zircon_a: Range<u64>,
        vbmeta_a: Range<u64>,
        zircon_b: Range<u64>,
        vbmeta_b: Range<u64>,
        zircon_r: Range<u64>,
        vbmeta_r: Range<u64>,
        misc: Range<u64>,
    }

    let abr_partitions = if !args.no_abr {
        Some(Partitions {
            zircon_a: add_partition(
                &mut gpt_disk,
                get_zbi_partition_name(&product_bundle_partitions, Slot::A)?,
                args.abr_size,
                ZIRCON_A_GUID,
            )?,
            vbmeta_a: add_partition(
                &mut gpt_disk,
                get_vbmeta_partition_name(&product_bundle_partitions, Slot::A)?,
                args.vbmeta_size,
                VBMETA_A_GUID,
            )?,
            zircon_b: add_partition(
                &mut gpt_disk,
                get_zbi_partition_name(&product_bundle_partitions, Slot::B)?,
                args.abr_size,
                ZIRCON_B_GUID,
            )?,
            vbmeta_b: add_partition(
                &mut gpt_disk,
                get_vbmeta_partition_name(&product_bundle_partitions, Slot::B)?,
                args.vbmeta_size,
                VBMETA_B_GUID,
            )?,
            zircon_r: add_partition(
                &mut gpt_disk,
                get_zbi_partition_name(&product_bundle_partitions, Slot::R)?,
                args.abr_size,
                ZIRCON_R_GUID,
            )?,
            vbmeta_r: add_partition(
                &mut gpt_disk,
                get_vbmeta_partition_name(&product_bundle_partitions, Slot::R)?,
                args.vbmeta_size,
                VBMETA_R_GUID,
            )?,
            misc: add_partition(&mut gpt_disk, "misc", args.vbmeta_size, MISC_GUID)?,
        })
    } else {
        None
    };

    let block_size: u64 = gpt_disk.logical_block_size().clone().into();

    let fvm_part = if !args.ramdisk_only && !args.use_fxfs {
        let size = args.system_disk_size.unwrap_or_else(|| {
            gpt_disk.find_free_sectors().iter().map(|(_offset, length)| length).max().unwrap()
                * block_size
        });
        if args.use_sparse_fvm {
            Some(add_partition(&mut gpt_disk, "storage-sparse", size, INSTALLER_GUID)?)
        } else {
            Some(add_partition(&mut gpt_disk, "fvm", size, FVM_GUID)?)
        }
    } else {
        None
    };
    let fxfs_part = if args.fxfs.is_some() {
        assert!(fvm_part.is_none(), "Can't have both FVM and Fxfs");
        let size = args.system_disk_size.unwrap_or_else(|| {
            gpt_disk.find_free_sectors().iter().map(|(_offset, length)| length).max().unwrap()
                * block_size
        });
        // For now, we use the same name and type as FVM because the paver looks for this.
        Some(add_partition(&mut gpt_disk, "fvm", size, FVM_GUID)?)
    } else {
        None
    };

    if let Some(seed) = &args.seed {
        let mut seed_bytes = [0; 16];
        for chunk in seed.as_bytes().chunks(16) {
            seed_bytes.iter_mut().zip(chunk.iter()).for_each(|(x, b)| *x ^= *b);
        }
        let mut rng = XorShiftRng::from_seed(seed_bytes);
        let mut bytes = [0; 16];

        rng.fill_bytes(&mut bytes);
        let uuid = uuid::Builder::from_bytes(bytes)
            .with_variant(uuid::Variant::RFC4122)
            .with_version(uuid::Version::Random)
            .into_uuid();

        // Unfortunately, the at time o writing, the GPT crate is using a different version of
        // the uuid crate than we have access to.
        gpt_disk.update_guid(Some(FromStr::from_str(&uuid.to_string()).unwrap()))?;

        let mut partitions = gpt_disk.partitions().clone();
        for (_id, partition) in &mut partitions {
            rng.fill_bytes(&mut bytes);
            let uuid = uuid::Builder::from_bytes(bytes)
                .with_variant(uuid::Variant::RFC4122)
                .with_version(uuid::Version::Random)
                .into_uuid();
            partition.part_guid = FromStr::from_str(&uuid.to_string()).unwrap();
        }
        gpt_disk.update_partitions(partitions)?;
    }

    gpt_disk.write()?;

    // Create a protective MBR
    // The size here should be the number of logical-blocks on the disk less one for the MBR itself.
    let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
        u32::try_from((disk_size - 1) / block_size).unwrap_or(0xffffffff),
    );
    mbr.overwrite_lba0(&mut disk)?;

    let search_path = if let Some(build_dir) = &args.fuchsia_build_dir {
        // Use tools from the build directory over $PATH.
        let host_path = match args.host_arch {
            Arch::X64 => "host_x64",
            Arch::Arm64 => "host_arm64",
        };
        format!("{}/{}:{}", build_dir, host_path, std::env::var("PATH").unwrap_or(String::new()))
    } else {
        String::new()
    };

    let mut command = Command::new("mkfs-msdosfs");

    // If a seed is provided, use a fixed timestamp.
    if args.seed.is_some() {
        command.arg("-T").arg("1661998826");
    }

    let output = command
        .arg("-@")
        .arg(format!("{}", efi_part.start))
        .arg("-S")
        .arg(format!("{}", args.efi_size + efi_part.start))
        .arg("-F")
        .arg("32")
        .arg("-L")
        .arg("ESP")
        .arg("-O")
        .arg("Fuchsia")
        .arg("-b")
        .arg(format!("{}", block_size))
        .arg(&args.disk_path)
        .env("PATH", &search_path)
        .output()
        .context("Failed to run mkfs-msdosfs")?;

    if !output.status.success() {
        bail!(
            "mkfs-msdosfs failed, stdout={}, stderr={}",
            std::str::from_utf8(&output.stdout).unwrap(),
            std::str::from_utf8(&output.stderr).unwrap()
        );
    }

    if args.seed.is_some() {
        write_esp_content(
            Shift::new(&mut disk, efi_part.start),
            &args,
            FsOptions::new().time_provider(NullTimeProvider::new()),
        )?;
    } else {
        write_esp_content(Shift::new(&mut disk, efi_part.start), &args, FsOptions::new())?;
    }

    if let Some(partitions) = abr_partitions {
        copy_partition(&mut disk, partitions.zircon_a, &args.zircon_a.unwrap())?;
        copy_partition(&mut disk, partitions.vbmeta_a, &args.vbmeta_a.unwrap())?;
        copy_partition(&mut disk, partitions.zircon_b, &args.zircon_b.unwrap())?;
        copy_partition(&mut disk, partitions.vbmeta_b, &args.vbmeta_b.unwrap())?;
        copy_partition(&mut disk, partitions.zircon_r, &args.zircon_r.unwrap())?;
        copy_partition(&mut disk, partitions.vbmeta_r, &args.vbmeta_r.unwrap())?;
        write_abr(&mut disk, partitions.misc.start, args.abr_boot)?;
    }

    if let Some(fvm_part) = fvm_part {
        if args.verbose {
            println!("Populating FVM in GPT image");
        }
        if args.use_sparse_fvm {
            copy_partition(&mut disk, fvm_part, &args.sparse_fvm.unwrap())?;
        } else {
            let status = Command::new("fvm")
                .arg(&args.disk_path)
                .arg("create")
                .arg("--offset")
                .arg(format!("{}", fvm_part.start))
                .arg("--length")
                .arg(format!("{}", fvm_part.end - fvm_part.start))
                .arg("--slice")
                .arg("8388608")
                .arg("--blob")
                .arg(args.blob.unwrap())
                .arg("--with-empty-minfs")
                .env("PATH", &search_path)
                .status()
                .context("Failed to run fvm tool")?;
            ensure!(status.success(), "fvm tool failed");
        }
    }

    if let Some(fxfs_part) = fxfs_part {
        copy_partition(&mut disk, fxfs_part, &args.fxfs.unwrap())?;
    }

    Ok(())
}

fn check_args(args: &mut TopLevel) -> Result<(), Error> {
    if args.ramdisk_only {
        if args.blob.is_some() || args.sparse_fvm.is_some() || args.use_sparse_fvm {
            bail!(
                "--ramdisk_only incompatible with --blob, --sparse-fvm and \
                   --use-sparse-fvm"
            );
        }
    } else if (args.blob.is_some()) && (args.sparse_fvm.is_some() || args.use_sparse_fvm) {
        bail!("--blob incompatbile with --use-sparse-fvm|--sparse-fvm");
    }

    if args.sparse_fvm.is_some() {
        args.use_sparse_fvm = true
    }

    if args.fuchsia_build_dir.is_none() {
        if let Ok(build_dir) = std::env::var("FUCHSIA_BUILD_DIR") {
            args.fuchsia_build_dir = Some(build_dir.into())
        }
    }

    let mut dependencies = vec![];

    if args.product_bundle.is_none() {
        args.product_bundle = get_build_product_bundle_path(args, &mut dependencies)?;
    };

    let product_bundle_path = if let Some(product_bundle_path) = &args.product_bundle {
        Some(product_bundle_path.join("product_bundle.json"))
    } else {
        None
    };

    if let Some(product_bundle_path) = &product_bundle_path {
        dependencies.push(product_bundle_path.as_path());
    }

    if args.bootloader.is_none() {
        if let Some(build_dir) = &args.fuchsia_build_dir {
            let bootloader_dir = match args.arch {
                Arch::X64 => build_dir.join("kernel.efi_x64"),
                Arch::Arm64 => build_dir.join("kernel.efi_arm64"),
            };
            args.bootloader = Some(bootloader_dir.join("fuchsia-efi.efi"));
        } else {
            bail!("Missing --bootloader");
        }
    }

    dependencies.push(args.bootloader.as_ref().unwrap());

    let images = if let Some(build_dir) = &args.fuchsia_build_dir {
        dependencies.push("images.json".into());

        #[derive(Deserialize)]
        struct Image {
            name: String,
            path: Utf8PathBuf,
            #[serde(rename(deserialize = "type"))]
            image_type: String,
        }
        let images: Vec<Image> =
            serde_json::from_slice(&read_file(build_dir.join("images.json"))?)?;
        // Maps "<image-type>_<image-name>" => "<image-path>"
        let images: HashMap<String, Utf8PathBuf> = images
            .into_iter()
            .map(|i| (i.image_type + "_" + &i.name, build_dir.join(i.path)))
            .collect();

        if args.zbi.is_none() {
            args.zbi = Some(get_image(&images, ZBI_ZIRCON_A)?);
        }
        if args.zedboot.is_none() {
            args.zedboot = Some(get_image(&images, ZBI_ZIRCON_R)?);
        }
        if !args.ramdisk_only {
            if args.use_sparse_fvm {
                if args.sparse_fvm.is_none() {
                    args.sparse_fvm = Some(get_image(&images, BLK_STORAGE_SPARSE)?);
                }
            } else if args.use_fxfs {
                if args.fxfs.is_none() {
                    args.fxfs = Some(get_image(&images, FXFS_BLK_STORAGE_FULL)?);
                }
            } else {
                if args.blob.is_none() {
                    args.blob = Some(get_image(&images, BLK_BLOB)?);
                }
            }
        }
        Some(images)
    } else {
        None
    };

    ensure!(args.zbi.is_some(), "Missing --zbi");
    dependencies.push(args.zbi.as_ref().unwrap());
    ensure!(args.zedboot.is_some(), "Missing --zedboot");
    dependencies.push(args.zedboot.as_ref().unwrap());

    if !args.no_abr {
        if args.zircon_a.is_none() {
            args.zircon_a = args.zbi.clone();
        }
        if args.zircon_b.is_none() {
            args.zircon_b = args.zbi.clone();
        }
        if let Some(images) = images {
            if args.vbmeta_a.is_none() {
                args.vbmeta_a = Some(images["vbmeta_zircon-a"].clone());
            }
            if args.vbmeta_b.is_none() {
                args.vbmeta_b = Some(images["vbmeta_zircon-a"].clone());
            }
            if args.zircon_r.is_none() {
                args.zircon_r = Some(images["zbi_zircon-r"].clone());
            }
            if args.vbmeta_r.is_none() {
                args.vbmeta_r = Some(images["vbmeta_zircon-r"].clone());
            }
        }

        ensure!(args.zircon_a.is_some(), "Missing --zircon_a");
        dependencies.push(args.zircon_a.as_ref().unwrap());
        ensure!(args.vbmeta_a.is_some(), "Missing --vbmeta_a");
        dependencies.push(args.vbmeta_a.as_ref().unwrap());
        ensure!(args.zircon_b.is_some(), "Missing --zircon_b");
        dependencies.push(args.zircon_b.as_ref().unwrap());
        ensure!(args.vbmeta_b.is_some(), "Missing --vbmeta_b");
        dependencies.push(args.vbmeta_b.as_ref().unwrap());
        ensure!(args.zircon_r.is_some(), "Missing --zircon_r");
        dependencies.push(args.zircon_r.as_ref().unwrap());
        ensure!(args.vbmeta_r.is_some(), "Missing --vbmeta_r");
        dependencies.push(args.vbmeta_r.as_ref().unwrap());
    }

    if !args.ramdisk_only {
        if args.use_sparse_fvm {
            ensure!(args.sparse_fvm.is_some(), "Missing --sparse-fvm");
            dependencies.push(args.sparse_fvm.as_ref().unwrap());
        } else if args.use_fxfs {
            ensure!(args.fxfs.is_some(), "Missing --fxfs");
            dependencies.push(args.fxfs.as_ref().unwrap());
        } else {
            ensure!(args.blob.is_some(), "Missing --blob");
            dependencies.push(args.blob.as_ref().unwrap());
        }
    }

    if args.depfile {
        // Write a dependency file
        // The output file and dependencies needs to be relative to the build dir.
        let mut disk_path = args.disk_path.as_path();
        if let Some(build_dir) = &args.fuchsia_build_dir {
            if let Ok(dir) = disk_path.strip_prefix(build_dir) {
                disk_path = dir;
            }

            for dep in dependencies.iter_mut() {
                if let Ok(d) = dep.strip_prefix(build_dir) {
                    *dep = d;
                }
            }
        }
        let depfile = format!("{}.d", args.disk_path);
        std::fs::write(
            &depfile,
            format!(
                "{}: {}",
                disk_path,
                dependencies.iter().map(|dep| dep.as_str()).collect::<Vec<_>>().join(" ")
            ),
        )
        .context(format!("Failed to write {}", &depfile))?;
    }

    Ok(())
}

fn get_build_product_bundle_path(
    args: &TopLevel,
    dependencies: &mut Vec<&Utf8Path>,
) -> Result<Option<Utf8PathBuf>, Error> {
    let Some(build_dir) = &args.fuchsia_build_dir else { return Ok(None) };
    let Some(name) = &args.product_bundle_name else { return Ok(None) };

    dependencies.push(Utf8Path::new("product_bundles.json"));
    let product_bundles_path = &build_dir.join(dependencies.last().unwrap());
    let f = match File::open(product_bundles_path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("{product_bundles_path} not found");
        }
        Err(err) => {
            return Err(err.into());
        }
    };

    let product_bundles: Vec<ProductBundleMetadata> = serde_json::from_reader(BufReader::new(f))?;
    match product_bundles.iter().find(|&pb| &pb.name == name) {
        None => Ok(None),
        Some(pb) => Ok(Some(build_dir.join(&pb.path))),
    }
}

fn read_file(path: impl AsRef<std::path::Path>) -> Result<Vec<u8>, Error> {
    std::fs::read(&path).context(format!("Failed to read {}", path.as_ref().display()))
}

// Adds a partition and returns the partition byte range.
fn add_partition(
    disk: &mut GptDisk<'_>,
    name: &str,
    size: u64,
    part_type: PartType,
) -> Result<Range<u64>, Error> {
    let part_id = disk
        .add_partition(name, size, part_type, 0, None)
        .context(format!("Failed to add {} partition (size={})", name, size))?;
    Ok(part_range(disk, part_id))
}

fn write_esp_content<TP: TimeProvider, OCC: OemCpConverter>(
    part: impl Read + Write + Seek,
    args: &TopLevel,
    options: FsOptions<TP, OCC>,
) -> Result<(), Error> {
    // If a seed is provided, all timestamps used will be the epoch.
    let fs = fatfs::FileSystem::new(part, options)?;

    {
        let root_dir = fs.root_dir();

        let bootloader_name = match args.arch {
            Arch::X64 => "bootx64.efi",
            Arch::Arm64 => "bootaa64.efi",
        };

        root_dir.create_dir("EFI")?;
        root_dir.create_dir("EFI/Google")?;
        root_dir.create_dir("EFI/Google/GSetup")?;
        root_dir
            .create_file("EFI/Google/GSetup/Boot")?
            .write_all(format!("efi\\boot\\{}", bootloader_name).as_bytes())?;

        root_dir.create_dir("EFI/BOOT")?;
        root_dir
            .create_file(&format!("EFI/BOOT/{}", bootloader_name))?
            .write_all(&read_file(args.bootloader.as_ref().unwrap())?)?;

        if args.no_abr {
            root_dir
                .create_file("zircon.bin")?
                .write_all(&read_file(args.zbi.as_ref().unwrap())?)?;
            root_dir
                .create_file("zedboot.bin")?
                .write_all(&read_file(args.zedboot.as_ref().unwrap())?)?;
        }

        if let Some(cmdline) = &args.cmdline {
            root_dir.create_file("cmdline")?.write_all(&read_file(cmdline)?)?;
        }
    }

    fs.unmount()?;

    Ok(())
}

// Copies the file at `source` path to the disk. `range` is the partition byte range.
fn copy_partition(disk: &mut File, range: Range<u64>, source: &Utf8Path) -> Result<(), Error> {
    let contents = read_file(source)?;
    let max_len = (range.end - range.start) as usize;
    let contents = if contents.len() > max_len {
        println!("{} too big", source);
        &contents[..max_len]
    } else {
        &contents
    };
    disk.write_all_at(contents, range.start)?;
    Ok(())
}

// Returns the partition range given a partition ID.
fn part_range(disk: &GptDisk<'_>, part_id: u32) -> Range<u64> {
    let lbs = u64::from(disk.logical_block_size().clone());
    let part = &disk.partitions()[&part_id];
    part.first_lba * lbs..(part.last_lba + 1) * lbs
}

// Routines for writing the ABR.
const MAX_TRIES: u8 = 7;
const MAX_PRIORITY: u8 = 15;

#[derive(AsBytes, NoCell)]
#[repr(C, packed)]
#[derive(Default)]
struct AbrData {
    magic: [u8; 4],
    version_major: u8,
    version_minor: u8,
    reserved1: u16,
    slot_data: [AbrSlotData; 2],
    one_shot_recovery_boot: u8,
    reserved2: [u8; 11],
    // A CRC32 comes next.
}

#[derive(AsBytes, NoCell)]
#[repr(C, packed)]
#[derive(Default)]
struct AbrSlotData {
    priority: u8,
    tries_remaining: u8,
    successful_boot: u8,
    reserved: u8,
}

impl AbrSlotData {
    fn with_priority(priority: u8) -> AbrSlotData {
        AbrSlotData { tries_remaining: MAX_TRIES, priority, ..Default::default() }
    }
}

#[derive(Debug)]
enum BootPart {
    BootA,
    BootB,
    BootR,
}

impl FromStr for BootPart {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "a" => Ok(BootPart::BootA),
            "b" => Ok(BootPart::BootB),
            "r" => Ok(BootPart::BootR),
            _ => Err("Expected a, b or r".to_string()),
        }
    }
}

// Writes the ABR to the disk at `offset`.
fn write_abr(disk: &mut File, offset: u64, boot_part: BootPart) -> Result<(), Error> {
    let data = AbrData {
        magic: b"\0AB0".clone(),
        version_major: 2,
        version_minor: 1,
        slot_data: match boot_part {
            BootPart::BootA => {
                [AbrSlotData::with_priority(MAX_PRIORITY), AbrSlotData::with_priority(1)]
            }
            BootPart::BootB => {
                [AbrSlotData::with_priority(1), AbrSlotData::with_priority(MAX_PRIORITY)]
            }
            BootPart::BootR => [AbrSlotData::with_priority(0), AbrSlotData::with_priority(0)],
        },
        ..Default::default()
    };

    disk.seek(SeekFrom::Start(offset))?;
    disk.write_all(data.as_bytes())?;
    disk.write_u32::<BigEndian>(crc32::checksum_ieee(data.as_bytes()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare_golden(test_data_dir: &Utf8Path, image_path: &Utf8Path) {
        let image = std::fs::read(&image_path).expect("Unable to read image");
        let file =
            std::fs::File::open(test_data_dir.join("golden")).expect("Unable to read golden");
        let golden = zstd::decode_all(file).expect("Unable to decompress");

        // If this fails, here are some tips for debugging:
        //
        // Create a loopback device:
        //
        //   sudo losetup -fP /tmp/image
        //
        // Inspect the partition map:
        //
        //   fdisk /tmp/image
        //
        // Verify the ESP partition:
        //
        //   sudo fsck sudo fsck /dev/loop0p1
        //
        // Mount the ESP partition:
        //
        //   mkdir /tmp/esp
        //   sudo mount -t vfat -o loop /dev/loop0p1 /tmp/esp
        //
        // To overwrite the golden image:
        //
        //   zstd /tmp/image -o golden
        //
        // View the contents of one of the partitions e.g. the misc partition:
        //
        //   sudo dd if=/dev/loop0p8 | hexdump -C
        assert!(image == golden);
    }

    #[test]
    fn test_with_product_bundle_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap();

        for i in 0..11 {
            std::fs::write(dir.join(format!("placeholder.{}", i)), vec![i; 8192]).unwrap();
        }

        let current_exe = std::env::current_exe().unwrap();

        let test_data_dir = Utf8PathBuf::from_path_buf(
            current_exe.parent().unwrap().join("make-fuchsia-vol_test_data"),
        )
        .unwrap();

        {
            let f = File::create(test_data_dir.join("product_bundles.json")).unwrap();
            let pbs: [ProductBundleMetadata; 0] = [];
            serde_json::to_writer(&f, &pbs).unwrap();
        }

        const IMAGE_SIZE: usize = 67108864;

        let image_path = dir.join("image");
        run(TopLevel::from_args(
            &["make-fuchsia-vol"],
            &[
                image_path.as_str(),
                "--abr-size",
                "8192",
                "--resize",
                &format!("{}", IMAGE_SIZE),
                "--seed",
                "test_compare_golden",
                "--efi-size",
                "40000000",
                "--bootloader",
                dir.join("placeholder.0").as_str(),
                "--zbi",
                dir.join("placeholder.1").as_str(),
                "--zedboot",
                dir.join("placeholder.2").as_str(),
                "--cmdline",
                dir.join("placeholder.3").as_str(),
                "--sparse-fvm",
                dir.join("placeholder.4").as_str(),
                "--zircon-a",
                dir.join("placeholder.5").as_str(),
                "--vbmeta-a",
                dir.join("placeholder.6").as_str(),
                "--zircon-b",
                dir.join("placeholder.7").as_str(),
                "--vbmeta-b",
                dir.join("placeholder.8").as_str(),
                "--zircon-r",
                dir.join("placeholder.9").as_str(),
                "--vbmeta-r",
                dir.join("placeholder.10").as_str(),
                "--fuchsia-build-dir",
                test_data_dir.as_str(),
                "--product-bundle-name",
                "my-product-bundle",
            ],
        )
        .unwrap())
        .expect("run failed");

        compare_golden(&test_data_dir, &image_path);
    }

    #[test]
    fn test_with_product_bundle_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap();

        for i in 0..11 {
            std::fs::write(dir.join(format!("placeholder.{}", i)), vec![i; 8192]).unwrap();
        }

        let current_exe = std::env::current_exe().unwrap();

        let test_data_dir = Utf8PathBuf::from_path_buf(
            current_exe.parent().unwrap().join("make-fuchsia-vol_test_data"),
        )
        .unwrap();

        const IMAGE_SIZE: usize = 67108864;

        let image_path = dir.join("image");
        run(TopLevel::from_args(
            &["make-fuchsia-vol"],
            &[
                image_path.as_str(),
                "--abr-size",
                "8192",
                "--resize",
                &format!("{}", IMAGE_SIZE),
                "--seed",
                "test_compare_golden",
                "--efi-size",
                "40000000",
                "--bootloader",
                dir.join("placeholder.0").as_str(),
                "--zbi",
                dir.join("placeholder.1").as_str(),
                "--zedboot",
                dir.join("placeholder.2").as_str(),
                "--cmdline",
                dir.join("placeholder.3").as_str(),
                "--sparse-fvm",
                dir.join("placeholder.4").as_str(),
                "--zircon-a",
                dir.join("placeholder.5").as_str(),
                "--vbmeta-a",
                dir.join("placeholder.6").as_str(),
                "--zircon-b",
                dir.join("placeholder.7").as_str(),
                "--vbmeta-b",
                dir.join("placeholder.8").as_str(),
                "--zircon-r",
                dir.join("placeholder.9").as_str(),
                "--vbmeta-r",
                dir.join("placeholder.10").as_str(),
                "--fuchsia-build-dir",
                test_data_dir.as_str(),
                "--product-bundle",
                test_data_dir.as_str(),
            ],
        )
        .unwrap())
        .expect("run failed");

        compare_golden(&test_data_dir, &image_path);
    }
}
