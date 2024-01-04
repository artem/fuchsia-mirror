// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, bail, Context, Result},
    async_trait::async_trait,
    chrono::{Datelike, Local, Timelike},
    ffx_target_screenshot_args::{Format, ScreenshotCommand},
    ffx_writer::{MachineWriter, ToolIO},
    fho::{moniker, FfxContext, FfxMain, FfxTool},
    fidl_fuchsia_io as fio,
    fidl_fuchsia_math::SizeU,
    fidl_fuchsia_ui_composition::{ScreenshotFormat, ScreenshotProxy, ScreenshotTakeFileRequest},
    futures::stream::{FuturesOrdered, StreamExt},
    png::HasParameters,
    serde::{Deserialize, Serialize},
    std::convert::{TryFrom, TryInto},
    std::fmt::{Display, Formatter, Result as FmtResult},
    std::fs,
    std::io::BufWriter,
    std::io::Write,
    std::path::{Path, PathBuf},
};

// Reads all of the contents of the given file from the current seek
// offset to end of file, returning the content. It errors if the seek pointer
// starts at an offset that results in reading less number of bytes read
// from the initial seek offset by the first request made by this function to EOF.
pub async fn read_data(file: &fio::FileProxy) -> Result<Vec<u8>> {
    // Number of concurrent read operations to maintain (aim for a 128kb
    // in-flight buffer, divided by the fuchsia.io chunk size). On a short range
    // network, 64kb should be more than sufficient, but on an LFN such as a
    // work-from-home scenario, having some more space further optimizes
    // performance.
    const CONCURRENCY: u64 = 131072 / fio::MAX_BUF;

    let mut out = Vec::new();

    let (status, attrs) =
        file.get_attr().await.user_message("Failed to get attributes of file (fidl failure)")?;

    if status != 0 {
        bail!("Error: Failed to get attributes, status: {}", status);
    }

    let mut queue = FuturesOrdered::new();

    for _ in 0..CONCURRENCY {
        queue.push(file.read(fio::MAX_BUF));
    }

    loop {
        let mut bytes: Vec<u8> = queue
            .next()
            .await
            .context("read stream closed prematurely")??
            .map_err(|status: i32| fho::Error::from(anyhow!("read error: status={status}")))?;

        if bytes.is_empty() {
            break;
        }
        out.append(&mut bytes);

        while queue.len() < CONCURRENCY.try_into().unwrap() {
            queue.push(file.read(fio::MAX_BUF));
        }
    }

    if out.len() != usize::try_from(attrs.content_size).bug_context("failed to convert to usize")? {
        return Err(anyhow!(
            "Error: Expected {} bytes, but instead read {} bytes",
            attrs.content_size,
            out.len()
        )
        .into());
    }

    Ok(out)
}

#[derive(FfxTool)]
pub struct ScreenshotTool {
    #[command]
    cmd: ScreenshotCommand,
    #[with(moniker("/core/ui"))]
    screenshot_proxy: ScreenshotProxy,
}

fho::embedded_plugin!(ScreenshotTool);

#[async_trait(?Send)]
impl FfxMain for ScreenshotTool {
    type Writer = MachineWriter<ScreenshotOutput>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        screenshot_impl(self.screenshot_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

async fn screenshot_impl<W: ToolIO<OutputItem = ScreenshotOutput>>(
    screenshot_proxy: ScreenshotProxy,
    cmd: ScreenshotCommand,
    writer: &mut W,
) -> Result<()> {
    let mut screenshot_file_path = match cmd.output_directory {
        Some(file_dir) => {
            let dir = Path::new(&file_dir);
            if !dir.is_dir() {
                bail!("ERROR: Path provided is not a directory");
            }
            dir.to_path_buf().join("screenshot")
        }
        None => {
            let dir = default_output_dir();
            fs::create_dir_all(&dir)?;
            dir.to_path_buf().join("screenshot")
        }
    };

    // TODO(https://fxbug.dev/108647): Use rgba format when available.
    // TODO(https://fxbug.dev/103742): Use png format when available.
    let screenshot_response = screenshot_proxy
        .take_file(ScreenshotTakeFileRequest {
            format: Some(ScreenshotFormat::BgraRaw),
            ..Default::default()
        })
        .await
        .map_err(|e| {
            fho::Error::from(anyhow!("Error: Could not get the screenshot from the target: {e:?}"))
        })?;

    let img_size = screenshot_response.size.expect("no data size returned from screenshot");
    let client_end = screenshot_response.file.expect("no file returned from screenshot");

    let file_proxy = client_end.into_proxy().expect("could not create file proxy");

    let mut img_data = read_data(&file_proxy).await?;
    // VMO in |file_proxy| may be padded for alignment.
    img_data.resize((img_size.width * img_size.height * 4).try_into().unwrap(), 0);

    match cmd.format {
        Format::PNG => {
            save_as_png(&mut screenshot_file_path, &mut img_data, img_size);
        }
        Format::BGRA => {
            save_as_bgra(&mut screenshot_file_path, &mut img_data);
        }
        Format::RGBA => {
            save_as_rgba(&mut screenshot_file_path, &mut img_data);
        }
    };

    writer.item(&ScreenshotOutput { output_file: screenshot_file_path })?;

    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ScreenshotOutput {
    output_file: PathBuf,
}

impl Display for ScreenshotOutput {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "Exported {}", self.output_file.display())
    }
}

fn default_output_dir() -> PathBuf {
    let now = Local::now();

    Path::new("/tmp").join("screenshot").join(format!(
        "{}{:02}{:02}_{:02}{:02}{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    ))
}

fn create_file(screenshot_file_path: &mut PathBuf) -> fs::File {
    fs::File::create(screenshot_file_path.clone())
        .unwrap_or_else(|_| panic!("cannot create file {}", screenshot_file_path.to_string_lossy()))
}

fn save_as_bgra(screenshot_file_path: &mut PathBuf, img_data: &mut Vec<u8>) {
    screenshot_file_path.set_extension("bgra");
    let mut screenshot_file = create_file(screenshot_file_path);

    screenshot_file.write_all(&img_data[..]).expect("failed to write bgra image data.");
}

// TODO(https://fxbug.dev/108647): Use rgba format when available.
fn save_as_rgba(screenshot_file_path: &mut PathBuf, img_data: &mut Vec<u8>) {
    bgra_to_rgba(img_data);

    screenshot_file_path.set_extension("rgba");
    let mut screenshot_file = create_file(screenshot_file_path);
    screenshot_file.write_all(&img_data[..]).expect("failed to write rgba image data.");
}

// TODO(https://fxbug.dev/103742): Use png format when available.
fn save_as_png(screenshot_file_path: &mut PathBuf, img_data: &mut Vec<u8>, img_size: SizeU) {
    bgra_to_rgba(img_data);

    screenshot_file_path.set_extension("png");
    let screenshot_file = create_file(screenshot_file_path);

    let ref mut w = BufWriter::new(screenshot_file);
    let mut encoder = png::Encoder::new(w, img_size.width, img_size.height);

    encoder.set(png::BitDepth::Eight);
    encoder.set(png::ColorType::RGBA);
    let mut png_writer = encoder.write_header().unwrap();
    png_writer.write_image_data(&img_data).expect("failed to write image data as PNG");
}

/// Performs inplace BGRA -> RGBA.
fn bgra_to_rgba(img_data: &mut Vec<u8>) {
    let bytes_per_pixel = 4;
    let mut blue_pos = 0;
    let mut red_pos = 2;
    let img_data_size = img_data.len();

    while blue_pos < img_data_size && red_pos < img_data_size {
        let blue = img_data[blue_pos];
        img_data[blue_pos] = img_data[red_pos];
        img_data[red_pos] = blue;
        blue_pos = blue_pos + bytes_per_pixel;
        red_pos = red_pos + bytes_per_pixel;
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use {
        super::*,
        ffx_writer::{Format as WriterFormat, MachineWriter, TestBuffers},
        fidl::endpoints::ServerEnd,
        fidl_fuchsia_ui_composition::{ScreenshotRequest, ScreenshotTakeFileResponse},
        futures::TryStreamExt,
        std::os::unix::ffi::OsStrExt,
        tempfile::tempdir,
    };

    fn serve_fake_file(server: ServerEnd<fio::FileMarker>) {
        fuchsia_async::Task::local(async move {
            let data: [u8; 16] = [1, 2, 3, 4, 1, 2, 3, 4, 4, 3, 2, 1, 4, 3, 2, 1];
            let mut stream =
                server.into_stream().expect("converting fake file server proxy to stream");

            let mut cc: u32 = 0;
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fio::FileRequest::Read { count: _, responder } => {
                        cc = cc + 1;
                        if cc == 1 {
                            responder.send(Ok(&data)).expect("writing file test response");
                        } else {
                            responder.send(Ok(&[])).expect("writing file test response");
                        }
                    }
                    fio::FileRequest::GetAttr { responder } => {
                        let attrs = fio::NodeAttributes {
                            mode: 0,
                            id: 0,
                            content_size: data.len() as u64,
                            storage_size: data.len() as u64,
                            link_count: 1,
                            creation_time: 0,
                            modification_time: 0,
                        };
                        responder.send(0, &attrs).expect("sending attributes");
                    }
                    e => panic!("not supported {:?}", e),
                }
            }
        })
        .detach();
    }

    fn setup_fake_screenshot_server() -> ScreenshotProxy {
        fho::testing::fake_proxy(move |req| match req {
            ScreenshotRequest::TakeFile { payload: _, responder } => {
                let mut screenshot = ScreenshotTakeFileResponse::default();

                let (file_client_end, file_server_end) =
                    fidl::endpoints::create_endpoints::<fio::FileMarker>();

                let _ = screenshot.file.insert(file_client_end);
                let _ = screenshot.size.insert(SizeU { width: 2, height: 2 });
                serve_fake_file(file_server_end);
                responder.send(screenshot).unwrap();
            }

            _ => assert!(false),
        })
    }

    async fn run_screenshot_test(cmd: ScreenshotCommand) -> ScreenshotOutput {
        let screenshot_proxy = setup_fake_screenshot_server();

        let test_buffers = TestBuffers::default();
        let mut writer = MachineWriter::new_test(Some(WriterFormat::Json), &test_buffers);
        let result = screenshot_impl(screenshot_proxy, cmd, &mut writer).await;
        assert!(result.is_ok());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stderr, "", "no warnings or errors should be reported");
        serde_json::from_str(&stdout).unwrap()
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_bgra() -> Result<()> {
        let ScreenshotOutput { output_file } =
            run_screenshot_test(ScreenshotCommand { output_directory: None, format: Format::BGRA })
                .await;
        assert_eq!(
            output_file.file_name().unwrap().as_bytes(),
            "screenshot.bgra".as_bytes(),
            "{output_file:?} must have filename==screenshot.bgra",
        );
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_output_dir_bgra() -> Result<()> {
        // Create a test directory in TempFile::tempdir.
        let output_dir = PathBuf::from(tempdir().unwrap().path()).join("screenshot_test");
        fs::create_dir_all(&output_dir)?;
        let ScreenshotOutput { output_file } = run_screenshot_test(ScreenshotCommand {
            output_directory: Some(output_dir.to_string_lossy().to_string()),
            format: Format::BGRA,
        })
        .await;
        assert_eq!(output_file, output_dir.join("screenshot.bgra"));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_png() -> Result<()> {
        let ScreenshotOutput { output_file } =
            run_screenshot_test(ScreenshotCommand { output_directory: None, format: Format::PNG })
                .await;
        assert_eq!(
            output_file.file_name().unwrap().as_bytes(),
            "screenshot.png".as_bytes(),
            "{output_file:?} must have filename==screenshot.png",
        );
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_output_dir_png() -> Result<()> {
        // Create a test directory in TempFile::tempdir.
        let output_dir = PathBuf::from(tempdir().unwrap().path()).join("screenshot_test");
        fs::create_dir_all(&output_dir)?;
        let ScreenshotOutput { output_file } = run_screenshot_test(ScreenshotCommand {
            output_directory: Some(output_dir.to_string_lossy().to_string()),
            format: Format::PNG,
        })
        .await;
        assert_eq!(output_file, output_dir.join("screenshot.png"));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_rgba() -> Result<()> {
        let ScreenshotOutput { output_file } =
            run_screenshot_test(ScreenshotCommand { output_directory: None, format: Format::RGBA })
                .await;
        assert_eq!(
            output_file.file_name().unwrap().as_bytes(),
            "screenshot.rgba".as_bytes(),
            "{output_file:?} must have filename==screenshot.rgba",
        );
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_output_dir_rgba() -> Result<()> {
        // Create a test directory in TempFile::tempdir.
        let output_dir = PathBuf::from(tempdir().unwrap().path()).join("screenshot_test");
        fs::create_dir_all(&output_dir)?;
        let ScreenshotOutput { output_file } = run_screenshot_test(ScreenshotCommand {
            output_directory: Some(output_dir.to_string_lossy().to_string()),
            format: Format::RGBA,
        })
        .await;
        assert_eq!(output_file, output_dir.join("screenshot.rgba"));
        Ok(())
    }
}
