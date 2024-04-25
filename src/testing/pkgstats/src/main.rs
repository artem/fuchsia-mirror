// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env::current_exe,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use anyhow::{bail, Context, Result};
use argh::FromArgs;
use fidl_fuchsia_component_decl as fdecl;
use fuchsia_archive::Reader as FARReader;
use fuchsia_pkg::PackageManifest;
use handlebars::{Handlebars, Helper, HelperResult, Output, RenderContext, RenderError};
use lazy_static::lazy_static;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(FromArgs)]
/// collect and generate stats on Fuchsia packages
struct Args {
    #[argh(subcommand)]
    cmd: CommandArgs,
}

#[derive(FromArgs)]
#[argh(subcommand)]
enum CommandArgs {
    Process(ProcessArgs),
    Html(HtmlArgs),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "process")]
/// process an out directory into a JSON representation
struct ProcessArgs {
    /// the path under which to look for packages
    #[argh(positional)]
    path: PathBuf,

    /// the path to save the output json file
    #[argh(option)]
    out: Option<PathBuf>,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "html")]
/// generate an HTML report from output data
struct HtmlArgs {
    /// input file generated using "process" command
    #[argh(option)]
    input: PathBuf,

    /// output directory for HTML, must not exist
    #[argh(option)]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    match args.cmd {
        CommandArgs::Process(args) => do_process_command(args),
        CommandArgs::Html(args) => do_html_command(args),
    }
}

fn do_html_command(args: HtmlArgs) -> Result<()> {
    if !args.input.is_file() {
        bail!("{:?} is not a file", args.input);
    } else if args.output.exists() {
        bail!("{:?} must not exist, it will be created by this tool", args.output);
    }

    let start = Instant::now();

    let input: OutputSummary = serde_json::from_reader(&mut File::open(&args.input)?)?;

    std::fs::create_dir_all(&args.output)?;

    let mut hb = Handlebars::new();
    hb.set_strict_mode(true);

    hb.register_template_string("base", include_str!("../templates/base_template.html.hbs"))?;
    hb.register_template_string("index", include_str!("../templates/index.html.hbs"))?;
    hb.register_template_string("package", include_str!("../templates/package.html.hbs"))?;
    hb.register_template_string("content", include_str!("../templates/content.html.hbs"))?;

    hb.register_helper("package_link", Box::new(package_link_helper));
    hb.register_helper("content_link", Box::new(content_link_helper));

    render_page(
        &hb,
        BaseTemplateArgs {
            page_title: "Home",
            css_path: "style.css",
            root_link: "",
            body_content: &render_index_contents(&hb, &input)?,
        },
        args.output.join("index.html"),
    )?;

    *RENDER_PATH.lock().unwrap() = "../".to_string();
    std::fs::create_dir_all(args.output.join("packages"))?;
    for item in input.packages.iter() {
        let body_content = hb.render("package", &item).context("rendering package")?;

        render_page(
            &hb,
            BaseTemplateArgs {
                page_title: &format!("Package: {}", item.0),
                css_path: "../style.css",
                root_link: "../",
                body_content: &body_content,
            },
            args.output
                .join("packages")
                .join(format!("{}.html", simplify_name_for_linking(item.0))),
        )?;
    }
    std::fs::create_dir_all(args.output.join("contents"))?;
    for item in input.contents.iter() {
        let mut file_names = match item.1 {
            FileInfo::Elf(elf) => elf
                .source_file_references
                .iter()
                .map(|idx| input.file_names[idx].as_str())
                .collect::<Vec<&str>>(),
            _ => vec![],
        };
        file_names.sort();

        let with_files = (item.0, item.1, file_names);
        let body_content = hb.render("content", &with_files).context("rendering content")?;
        render_page(
            &hb,
            BaseTemplateArgs {
                page_title: &format!("File content: {}", item.0),
                css_path: "../style.css",
                root_link: "../",
                body_content: &body_content,
            },
            args.output
                .join("contents")
                .join(format!("{}.html", simplify_name_for_linking(item.0))),
        )?;
    }
    *RENDER_PATH.lock().unwrap() = "".to_string();

    File::create(args.output.join("style.css"))
        .context("open style.css")?
        .write_all(include_bytes!("../templates/style.css"))
        .context("write style.css")?;

    println!("Created site at {:?} in {:?}", args.output, Instant::now() - start);

    Ok(())
}

#[derive(Serialize)]
struct BaseTemplateArgs<'a> {
    page_title: &'a str,
    css_path: &'a str,
    root_link: &'a str,
    body_content: &'a str,
}

fn render_page(
    hb: &Handlebars<'_>,
    args: BaseTemplateArgs<'_>,
    out_path: impl AsRef<Path> + std::fmt::Debug,
) -> Result<()> {
    println!("..rendering {:?}", out_path);
    hb.render_to_write("base", &args, File::create(out_path)?)?;

    Ok(())
}

fn render_index_contents(hb: &Handlebars<'_>, output: &OutputSummary) -> Result<String> {
    Ok(hb.render("index", output)?)
}

lazy_static! {
    static ref RENDER_PATH: Mutex<String> = Mutex::new("".to_string());
}

fn simplify_name_for_linking(name: &str) -> String {
    let mut ret = String::with_capacity(name.len());

    let replace_chars = ".-/\\:";

    let mut replaced = false;
    for ch in name.chars() {
        if replace_chars.contains(ch) {
            if replaced {
                continue;
            }
            ret.push('_');
            replaced = true;
        } else {
            replaced = false;
            ret.push(ch);
        }
    }

    ret
}

fn package_link_helper(
    h: &Helper<'_, '_>,
    _: &Handlebars<'_>,
    _: &handlebars::Context,
    _: &mut RenderContext<'_, '_>,
    out: &mut dyn Output,
) -> HelperResult {
    let input_name = if let Some(name) = h.param(0) {
        name.value().as_str().ok_or(RenderError::new("Value is not a non-empty string"))?
    } else {
        return Err(RenderError::new("Helper requires one param"));
    };

    out.write(&format!(
        "{}packages/{}.html",
        *RENDER_PATH.lock().unwrap(),
        simplify_name_for_linking(input_name)
    ))?;
    Ok(())
}

fn content_link_helper(
    h: &Helper<'_, '_>,
    _: &Handlebars<'_>,
    _: &handlebars::Context,
    _: &mut RenderContext<'_, '_>,
    out: &mut dyn Output,
) -> HelperResult {
    let input_name = if let Some(name) = h.param(0) {
        name.value().as_str().ok_or(RenderError::new("Value is not a non-empty string"))?
    } else {
        return Err(RenderError::new("Helper requires one param"));
    };

    out.write(&format!(
        "{}contents/{}.html",
        *RENDER_PATH.lock().unwrap(),
        simplify_name_for_linking(input_name)
    ))?;
    Ok(())
}

#[derive(Deserialize)]
struct DebugDumpOutput {
    status: String,
    error: String,
    files: Vec<String>,
}

fn do_process_command(args: ProcessArgs) -> Result<()> {
    let path = match args.path {
        ref p if p.is_dir() => p,
        ref p if p.exists() => {
            bail!("'{}' is not a directory", p.to_string_lossy())
        }
        ref p => {
            bail!("Directory '{}' does not exist", p.to_string_lossy())
        }
    };

    let mut manifests = vec![];

    let mut dirs = vec![path.read_dir()?];
    let mut dir_count = 0;
    let mut file_count = 0;
    let start = Instant::now();
    while let Some(dir) = dirs.pop() {
        dir_count += 1;
        for val in dir {
            let entry = val?;
            if &entry.file_name() == "package_manifest.json" {
                file_count += 1;
                manifests.push(entry.path());
            } else if entry.file_type()?.is_dir() {
                dirs.push(entry.path().read_dir()?);
            } else {
                file_count += 1;
            }
        }
    }
    let duration = Instant::now() - start;

    println!(
        "Found {} manifests out of {} files in {} dirs in {:?}",
        manifests.len(),
        file_count,
        dir_count,
        duration
    );

    let get_content_file_path = |relative_path: &str, source_file_path: &Path| {
        let filepath = path.join(relative_path);
        if filepath.exists() {
            return Ok(filepath);
        }

        if source_file_path.is_file() {
            match source_file_path.parent() {
                Some(v) => Ok(v.join(relative_path)),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find the file",
                )),
            }
        } else {
            Ok(source_file_path.join(relative_path))
        }
    };

    let get_content_file = |relative_path: &str, source_file_path: &Path| {
        File::open(get_content_file_path(relative_path, source_file_path)?)
    };

    let manifest_count = AtomicUsize::new(0);
    let manifest_errors = AtomicUsize::new(0);
    let manifest_file_errors = AtomicUsize::new(0);
    let names = Mutex::new(HashMap::new());
    let content_hash_to_path = Mutex::new(HashMap::new());
    let start = Instant::now();
    manifests.par_iter().for_each(|manifest_path| {
        manifest_count.fetch_add(1, Ordering::Relaxed);
        macro_rules! do_on_error {
            ($val:expr, $step:expr, $error_counter:expr, $do:expr) => {
                match $val {
                    Ok(v) => v,
                    Err(e) => {
                        $error_counter.fetch_add(1, Ordering::Relaxed);
                        eprintln!(
                            "[{}] Failed {}: {:?}",
                            manifest_path.to_string_lossy(),
                            $step,
                            e
                        );
                        $do;
                    }
                }
            };
            ($val:expr, $step:expr, $context:expr, $error_counter:expr, $do:expr) => {
                match $val {
                    Ok(v) => v,
                    Err(e) => {
                        $error_counter.fetch_add(1, Ordering::Relaxed);
                        eprintln!(
                            "[{}] Failed {} for {}: {:?}",
                            manifest_path.to_string_lossy(),
                            $step,
                            $context,
                            e
                        );
                        $do;
                    }
                }
            };
        }

        let manifest = do_on_error!(
            PackageManifest::try_load_from(
                manifest_path.to_string_lossy().to_owned().to_string().as_str()
            ),
            "loading manifest",
            manifest_errors,
            return
        );

        let url =
            match do_on_error!(manifest.package_url(), "formatting URL", manifest_errors, return) {
                Some(url) => url,
                None => {
                    // Package does not have a URL, skip.
                    manifest_errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };

        let mut contents = PackageContents::default();

        for blob in manifest.blobs() {
            if blob.path == "meta/" {
                // Handle meta
                let meta_file = do_on_error!(
                    get_content_file(&blob.source_path, manifest_path),
                    "opening file",
                    blob.path,
                    manifest_file_errors,
                    continue
                );
                let mut reader = do_on_error!(
                    FARReader::new(meta_file),
                    "opening as FAR file",
                    blob.path,
                    manifest_file_errors,
                    continue
                );

                let mut manifest_paths = vec![];
                for entry in reader.list() {
                    let path = String::from_utf8_lossy(entry.path());
                    if path.ends_with(".cm") {
                        manifest_paths.push(entry.path().to_owned());
                    }
                }

                for manifest_path in manifest_paths {
                    let data = do_on_error!(
                        reader.read_file(&manifest_path),
                        "reading component manifest",
                        String::from_utf8_lossy(&manifest_path),
                        manifest_file_errors,
                        break
                    );
                    let manifest: fdecl::Component = do_on_error!(
                        fidl::unpersist(&data),
                        "parsing component manifest",
                        String::from_utf8_lossy(&manifest_path),
                        manifest_file_errors,
                        break
                    );

                    let mut component = ComponentContents::default();
                    for cap in manifest.uses.into_iter().flatten() {
                        match cap {
                            fdecl::Use::Protocol(p) => {
                                let (name, from) = match (p.source_name, p.source) {
                                    (Some(s), Some(r)) => (s, r),
                                    _ => break,
                                };
                                match from {
                                    fdecl::Ref::Parent(_) => {
                                        component
                                            .used_from_parent
                                            .insert(Capability::Protocol(name));
                                    }
                                    fdecl::Ref::Child(c) => {
                                        component
                                            .used_from_child
                                            .insert((Capability::Protocol(name), c.name));
                                    }
                                    _ => break,
                                }
                            }
                            _ => {
                                // Skip all else for now
                            }
                        }
                    }
                    for cap in manifest.exposes.into_iter().flatten() {
                        match cap {
                            fdecl::Expose::Protocol(p) => {
                                let (name, from) = match (p.source_name, p.source) {
                                    (Some(s), Some(r)) => (s, r),
                                    _ => break,
                                };
                                match from {
                                    fdecl::Ref::Self_(_) => {
                                        component
                                            .exposed_from_self
                                            .insert(Capability::Protocol(name));
                                    }
                                    fdecl::Ref::Child(c) => {
                                        component
                                            .exposed_from_child
                                            .insert((Capability::Protocol(name), c.name));
                                    }
                                    _ => break,
                                }
                            }
                            _ => {
                                // Skip all else for now
                            }
                        }
                    }

                    let path = String::from_utf8_lossy(&manifest_path);
                    let last_segment = path.rfind("/");
                    let name = match last_segment {
                        Some(i) => &path[i + 1..],
                        None => &path,
                    };
                    contents.components.insert(name.to_string(), component);
                }
            } else if blob.path.starts_with("blobs/") {
                // This is a referenced blob. Put them in the
                // separate blobs section so a file entry will reference
                // the canonical source file rather than the copied blob.

                contents.blobs.push(blob.merkle.to_string());
            } else {
                let source_path = get_content_file_path(&blob.source_path, manifest_path)
                    .map(|v| v.to_string_lossy().to_string())
                    .unwrap_or_default();
                content_hash_to_path.lock().unwrap().insert(blob.merkle.to_string(), source_path);
                contents.files.push(PackageFile {
                    name: blob.path.to_string(),
                    hash: blob.merkle.to_string(),
                });
            }
        }
        names.lock().unwrap().insert(url.to_string(), contents);
    });
    let file_infos = Mutex::new(HashMap::new());
    let elf_count = AtomicUsize::new(0);
    let other_count = AtomicUsize::new(0);
    let interner = InternEnumerator::new();

    let debugdump_path = current_exe().expect("get current path").with_file_name("debugdump");
    if !debugdump_path.exists() {
        panic!("Expected to find debugdump binary adjacent to pkgstats here: {:?}", debugdump_path);
    }

    content_hash_to_path.lock().unwrap().par_iter().for_each(|(hash, path)| {
        if path.is_empty() {
            return;
        }
        let path = PathBuf::from(path);
        let alt_path = path
            .parent()
            .map(|v| v.join("exe.unstripped").join(path.file_name().unwrap_or_default()));
        let path = if let Some(alt_path) = alt_path {
            if alt_path.is_file() {
                alt_path
            } else {
                path
            }
        } else {
            path
        };

        let f = File::open(&path);
        if f.is_err() {
            eprintln!("Failed to open {}, skipping: {:?}", path.to_string_lossy(), f.unwrap_err());
            return;
        }
        let mut f = f.unwrap();
        let mut header_buf = [0u8; 4];
        // Check if this looks like an ELF file, starting with 0x7F 'E' 'L' 'F'
        if f.read_exact(&mut header_buf).is_ok() && header_buf == [0x7fu8, 0x45u8, 0x4cu8, 0x46u8] {
            // process
            elf_count.fetch_add(1, Ordering::Relaxed);

            let mut elf_contents = ElfContents::new(path.to_string_lossy().to_string());
            let proc = std::process::Command::new(&debugdump_path)
                .arg(path.as_os_str())
                .output()
                .expect("running debugdump");

            let output = serde_json::from_slice::<DebugDumpOutput>(&proc.stdout);
            let files = match output {
                Ok(output) => {
                    if output.status != "OK".to_string() {
                        eprintln!("Debug info error: {}", output.error);
                        vec![]
                    } else {
                        output.files
                    }
                }
                Err(e) => {
                    eprintln!("Error parsing debugdump output: {:?}", e);
                    vec![]
                }
            };
            for line in files.iter() {
                elf_contents.source_file_references.insert(interner.intern(line));
            }
            file_infos.lock().unwrap().insert(hash.clone(), FileInfo::Elf(elf_contents));
        } else {
            file_infos.lock().unwrap().insert(
                hash.clone(),
                FileInfo::Other(OtherContents { source_path: path.to_string_lossy().to_string() }),
            );
            other_count.fetch_add(1, Ordering::Relaxed);
        }
    });

    let duration = Instant::now() - start;

    println!(
        "Loaded in {:?}. {} manifests, {} valid, {} manifest errors, {} file errors. {} ELF / {} Other files found. Contents processed: {}",
        duration,
        manifest_count.load(Ordering::Relaxed),
        names.lock().unwrap().len(),
        manifest_errors.load(Ordering::Relaxed),
        manifest_file_errors.load(Ordering::Relaxed),
        elf_count.load(Ordering::Relaxed),
        other_count.load(Ordering::Relaxed),
        content_hash_to_path.lock().unwrap().len(),
    );

    let start = Instant::now();

    let packages = names.lock().unwrap().drain().collect::<BTreeMap<_, _>>();
    let contents = file_infos.lock().unwrap().drain().collect::<BTreeMap<_, _>>();
    let file_names = interner
        .intern_set
        .lock()
        .unwrap()
        .drain()
        .map(|(k, v)| (v, k))
        .collect::<BTreeMap<_, _>>();

    let output = OutputSummary { packages, contents, file_names };

    if let Some(out) = &args.out {
        let mut file = std::fs::File::create(out)?;
        serde_json::to_writer(&mut file, &output)?;
        let dur = Instant::now() - start;
        println!("Output JSON in {:?}", dur);
    }

    Ok(())
}

#[derive(Serialize, Deserialize)]
struct OutputSummary {
    packages: BTreeMap<String, PackageContents>,
    contents: BTreeMap<String, FileInfo>,
    file_names: BTreeMap<u32, String>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct PackageContents {
    // The named files included in this package.
    files: Vec<PackageFile>,
    // The named components included in this package.
    components: HashMap<String, ComponentContents>,
    // The blobs referenced by this package as "blobs/*" files.
    blobs: Vec<String>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct PackageFile {
    name: String,
    hash: String,
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct ComponentContents {
    used_from_parent: HashSet<Capability>,
    used_from_child: HashSet<(Capability, String)>,
    exposed_from_self: HashSet<Capability>,
    exposed_from_child: HashSet<(Capability, String)>,
}

#[derive(PartialEq, Hash, Eq, Debug, Serialize, Deserialize)]
enum Capability {
    #[serde(rename = "protocol")]
    Protocol(String),
    #[serde(rename = "directory")]
    Directory(String),
}

#[derive(Serialize, Deserialize)]
enum FileInfo {
    #[serde(rename = "elf")]
    Elf(ElfContents),
    #[serde(rename = "other")]
    Other(OtherContents),
}

#[derive(Serialize, Deserialize)]
struct ElfContents {
    source_path: String,
    source_file_references: BTreeSet<u32>,
}

impl ElfContents {
    pub fn new(source_path: String) -> Self {
        Self { source_path, source_file_references: BTreeSet::new() }
    }
}

#[derive(Serialize, Deserialize)]
struct OtherContents {
    source_path: String,
}

#[derive(Clone)]
struct InternEnumerator {
    intern_set: Arc<Mutex<HashMap<String, u32>>>,
    next_id: Arc<AtomicU32>,
}

impl InternEnumerator {
    pub fn new() -> Self {
        Self {
            intern_set: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU32::new(0)),
        }
    }
    pub fn intern(&self, value: &str) -> u32 {
        let mut set = self.intern_set.lock().unwrap();
        if let Some(val) = set.get(value) {
            *val
        } else {
            let next = self.next_id.fetch_add(1, Ordering::Relaxed);
            set.insert(value.to_string(), next);
            next
        }
    }
}
