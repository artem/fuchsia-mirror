// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::base::SettingInfo;
use crate::do_not_disturb::types::DoNotDisturbInfo;
use crate::handler::base::Request;
use crate::handler::setting_handler::persist::{controller as data_controller, ClientProxy};
use crate::handler::setting_handler::{
    controller, ControllerError, IntoHandlerResult, SettingHandlerResult,
};
use async_trait::async_trait;
use settings_storage::device_storage::{DeviceStorage, DeviceStorageCompatible};
use settings_storage::storage_factory::{NoneT, StorageAccess};

impl DeviceStorageCompatible for DoNotDisturbInfo {
    type Loader = NoneT;
    const KEY: &'static str = "do_not_disturb_info";
}

impl Default for DoNotDisturbInfo {
    fn default() -> Self {
        DoNotDisturbInfo::new(false, false)
    }
}

impl From<DoNotDisturbInfo> for SettingInfo {
    fn from(info: DoNotDisturbInfo) -> SettingInfo {
        SettingInfo::DoNotDisturb(info)
    }
}

pub struct DoNotDisturbController {
    client: ClientProxy,
}

impl StorageAccess for DoNotDisturbController {
    type Storage = DeviceStorage;
    type Data = DoNotDisturbInfo;
    const STORAGE_KEY: &'static str = DoNotDisturbInfo::KEY;
}

#[async_trait]
impl data_controller::Create for DoNotDisturbController {
    async fn create(client: ClientProxy) -> Result<Self, ControllerError> {
        Ok(DoNotDisturbController { client })
    }
}

#[async_trait]
impl controller::Handle for DoNotDisturbController {
    async fn handle(&self, request: Request) -> Option<SettingHandlerResult> {
        match request {
            Request::SetDnD(dnd_info) => {
                let id = fuchsia_trace::Id::new();
                let mut stored_value = self.client.read_setting::<DoNotDisturbInfo>(id).await;
                if dnd_info.user_dnd.is_some() {
                    stored_value.user_dnd = dnd_info.user_dnd;
                }
                if dnd_info.night_mode_dnd.is_some() {
                    stored_value.night_mode_dnd = dnd_info.night_mode_dnd;
                }
                Some(self.client.write_setting(stored_value.into(), id).await.into_handler_result())
            }
            Request::Get => Some(
                self.client
                    .read_setting_info::<DoNotDisturbInfo>(fuchsia_trace::Id::new())
                    .await
                    .into_handler_result(),
            ),
            _ => None,
        }
    }
}
