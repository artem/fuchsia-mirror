// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        cli::format::format_action_error, lifecycle::stop_instance,
        query::get_cml_moniker_from_query,
    },
    anyhow::Result,
    fidl_fuchsia_sys2 as fsys,
};

pub async fn stop_cmd<W: std::io::Write>(
    query: String,
    lifecycle_controller: fsys::LifecycleControllerProxy,
    realm_query: fsys::RealmQueryProxy,
    mut writer: W,
) -> Result<()> {
    let moniker = get_cml_moniker_from_query(&query, &realm_query).await?;

    writeln!(writer, "Moniker: {}", moniker)?;
    writeln!(writer, "Stopping component instance...")?;

    stop_instance(&lifecycle_controller, &moniker)
        .await
        .map_err(|e| format_action_error(&moniker, e))?;

    writeln!(writer, "Stopped component instance!")?;
    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*, crate::test_utils::serve_realm_query_instances,
        fidl::endpoints::create_proxy_and_stream, futures::TryStreamExt, moniker::Moniker,
    };

    fn setup_fake_lifecycle_controller(
        expected_moniker: &'static str,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::StopInstance { moniker, responder, .. } => {
                    assert_eq!(Moniker::parse_str(expected_moniker), Moniker::parse_str(&moniker));
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_success() -> Result<()> {
        let mut output = Vec::new();
        let lifecycle_controller = setup_fake_lifecycle_controller("/core/ffx-laboratory:test");
        let realm_query = serve_realm_query_instances(vec![fsys::Instance {
            moniker: Some("/core/ffx-laboratory:test".to_string()),
            url: Some("fuchsia-pkg://fuchsia.com/test#meta/test.cml".to_string()),
            instance_id: None,
            resolved_info: None,
            ..Default::default()
        }]);
        let response = stop_cmd(
            "/core/ffx-laboratory:test".to_string(),
            lifecycle_controller,
            realm_query,
            &mut output,
        )
        .await;
        response.unwrap();
        Ok(())
    }
}
