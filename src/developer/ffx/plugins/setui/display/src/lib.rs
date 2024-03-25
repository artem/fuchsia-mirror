// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_setui_display_args::{Display, SubCommandEnum};
use fho::{moniker, AvailabilityFlag, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_settings::DisplayProxy;

pub use utils;

mod get;
mod set;
mod watch;

#[derive(FfxTool)]
#[check(AvailabilityFlag("setui"))]
pub struct DisplayTool {
    #[command]
    cmd: Display,
    #[with(moniker("/core/setui_service"))]
    display_proxy: DisplayProxy,
}

fho::embedded_plugin!(DisplayTool);

#[async_trait(?Send)]
impl FfxMain for DisplayTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        run_command(self.display_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

async fn run_command<W: std::io::Write>(
    display_proxy: DisplayProxy,
    display: Display,
    w: &mut W,
) -> Result<()> {
    match display.subcommand {
        SubCommandEnum::Set(args) => set::set(display_proxy, args, w).await,
        SubCommandEnum::Get(args) => get::get(display_proxy, args, w).await,
        SubCommandEnum::Watch(_) => watch::watch(display_proxy, w).await,
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_setui_display_args::{Field, GetArgs, SetArgs};
    use fidl_fuchsia_settings::{DisplayRequest, DisplaySettings};

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_command() {
        let expected_display = SetArgs {
            brightness: None,
            auto_brightness_level: None,
            auto_brightness: Some(false),
            low_light_mode: None,
            theme: None,
            screen_enabled: None,
        };

        let proxy = fho::testing::fake_proxy(move |req| match req {
            DisplayRequest::Set { .. } => {
                panic!("Unexpected call to set");
            }
            DisplayRequest::Watch { responder } => {
                let _ = responder.send(&DisplaySettings::from(expected_display.clone()));
            }
            DisplayRequest::WatchLightSensor { .. } => {
                panic!("Unexpected call to watch light sensor");
            }
        });

        let display =
            Display { subcommand: SubCommandEnum::Get(GetArgs { field: Some(Field::Auto) }) };
        let response = run_command(proxy, display, &mut vec![]).await;
        assert!(response.is_ok());
    }
}
