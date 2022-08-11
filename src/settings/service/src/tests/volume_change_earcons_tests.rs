// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::audio::default_audio_info;
use crate::audio::types::{AudioSettingSource, AudioStream, AudioStreamType};
use crate::ingress::fidl::Interface;
use crate::input::common::MediaButtonsEventBuilder;
use crate::message::base::MessengerType;
use crate::service;
use crate::storage::testing::InMemoryStorageFactory;
use crate::tests::fakes::audio_core_service;
use crate::tests::fakes::input_device_registry_service::InputDeviceRegistryService;
use crate::tests::fakes::service_registry::ServiceRegistry;
use crate::tests::fakes::sound_player_service::{SoundEventReceiver, SoundPlayerService};
use crate::tests::scaffold;
use crate::AgentType;
use crate::EnvironmentBuilder;
use fidl_fuchsia_media::AudioRenderUsage;
use fidl_fuchsia_settings::{
    AudioMarker, AudioProxy, AudioSettings, AudioStreamSettingSource, AudioStreamSettings, Volume,
};
use fuchsia_component::server::NestedEnvironment;
use futures::future::BoxFuture;
use futures::lock::Mutex;
use futures::StreamExt;
use settings_storage::device_storage::DeviceStorage;
use std::sync::Arc;

const ENV_NAME: &str = "volume_change_earcons_test_environment";
const INITIAL_VOLUME_LEVEL: f32 = 0.5;
const MAX_VOLUME_LEVEL: f32 = 1.0;
const CHANGED_VOLUME_UNMUTED: bool = false;

const MAX_VOLUME_EARCON_ID: u32 = 0;

const INITIAL_MEDIA_STREAM_SETTINGS: AudioStreamSettings = AudioStreamSettings {
    stream: Some(fidl_fuchsia_media::AudioRenderUsage::Media),
    source: Some(fidl_fuchsia_settings::AudioStreamSettingSource::User),
    user_volume: Some(Volume {
        level: Some(INITIAL_VOLUME_LEVEL),
        muted: Some(false),
        ..Volume::EMPTY
    }),
    ..AudioStreamSettings::EMPTY
};

const CHANGED_MEDIA_STREAM_SETTINGS_MAX: AudioStreamSettings = AudioStreamSettings {
    stream: Some(fidl_fuchsia_media::AudioRenderUsage::Media),
    source: Some(AudioStreamSettingSource::User),
    user_volume: Some(Volume {
        level: Some(MAX_VOLUME_LEVEL),
        muted: Some(CHANGED_VOLUME_UNMUTED),
        ..Volume::EMPTY
    }),
    ..AudioStreamSettings::EMPTY
};

impl From<AudioStreamSettings> for AudioStream {
    fn from(stream: AudioStreamSettings) -> Self {
        AudioStream {
            stream_type: AudioStreamType::from(stream.stream.unwrap()),
            source: AudioSettingSource::from(stream.source.unwrap()),
            user_volume_level: stream.user_volume.as_ref().unwrap().level.unwrap(),
            user_volume_muted: stream.user_volume.as_ref().unwrap().muted.unwrap(),
        }
    }
}

// Used to store fake services for mocking dependencies and checking input/outputs.
// To add a new fake to these tests, add here, in create_services, and then use
// in your test.
struct FakeServices {
    input_device_registry: Arc<Mutex<InputDeviceRegistryService>>,
    sound_player: Arc<Mutex<SoundPlayerService>>,
}

async fn create_environment(
    service_registry: Arc<Mutex<ServiceRegistry>>,
    overridden_initial_streams: Vec<AudioStreamSettings>,
) -> (NestedEnvironment, Arc<DeviceStorage>, service::message::Receptor) {
    let mut initial_audio_info = default_audio_info();

    for stream in overridden_initial_streams {
        initial_audio_info.replace_stream(AudioStream::from(stream));
    }

    let (event_tx, mut event_rx) =
        futures::channel::mpsc::unbounded::<service::message::Delegate>();
    let storage_factory = Arc::new(InMemoryStorageFactory::with_initial_data(&initial_audio_info));

    // Upon instantiation, the subscriber will capture the event message
    // factory.
    let create_subscriber =
        Arc::new(move |delegate: service::message::Delegate| -> BoxFuture<'static, ()> {
            let event_tx = event_tx.clone();
            Box::pin(async move {
                event_tx.unbounded_send(delegate).unwrap();
            })
        });

    let env = EnvironmentBuilder::new(Arc::clone(&storage_factory))
        .service(ServiceRegistry::serve(service_registry))
        .event_subscribers(&[scaffold::event::subscriber::Blueprint::create(create_subscriber)])
        .fidl_interfaces(&[Interface::Audio])
        .agents(&[
            AgentType::Restore.into(),
            AgentType::Earcons.into(),
            AgentType::MediaButtons.into(),
        ])
        .spawn_and_get_nested_environment(ENV_NAME)
        .await
        .unwrap();

    let delegate = event_rx.next().await.expect("should return a factory");

    let (_, receptor) = delegate
        .create(MessengerType::Unbound)
        .await
        .expect("Should be able to retrieve messenger for publisher");

    let store = storage_factory.get_device_storage().await;
    (env, store, receptor)
}

// Returns a registry and audio related services it is populated with.
async fn create_services() -> (Arc<Mutex<ServiceRegistry>>, FakeServices) {
    let service_registry = ServiceRegistry::create();

    // Used for some functions inside the audio_controller and other dependencies.
    let audio_core_service_handle = audio_core_service::Builder::new().build();
    service_registry.lock().await.register_service(audio_core_service_handle.clone());

    let input_device_registry_service_handle =
        Arc::new(Mutex::new(InputDeviceRegistryService::new()));
    service_registry.lock().await.register_service(input_device_registry_service_handle.clone());

    let sound_player_service_handle = Arc::new(Mutex::new(SoundPlayerService::new()));
    service_registry.lock().await.register_service(sound_player_service_handle.clone());

    (
        service_registry,
        FakeServices {
            input_device_registry: input_device_registry_service_handle,
            sound_player: sound_player_service_handle,
        },
    )
}

/// Helper function to set volume streams on the proxy.
async fn set_volume(proxy: &AudioProxy, streams: Vec<AudioStreamSettings>) {
    let mut audio_settings = AudioSettings::EMPTY;
    audio_settings.streams = Some(streams);
    proxy.set(audio_settings).await.expect("set completed").expect("set successful");
}

async fn verify_earcon(receiver: &mut SoundEventReceiver, id: u32, usage: AudioRenderUsage) {
    assert_eq!(receiver.next().await.unwrap(), (id, usage));
}

// Test to ensure that when the volume is increased while already at max volume, the earcon for
// max volume plays.
#[fuchsia_async::run_until_stalled(test)]
async fn test_max_volume_sound_on_press() {
    let (service_registry, fake_services) = create_services().await;
    let (env, ..) = create_environment(service_registry, vec![INITIAL_MEDIA_STREAM_SETTINGS]).await;
    let mut sound_event_receiver =
        fake_services.sound_player.lock().await.create_sound_played_listener().await;

    let audio_proxy = env.connect_to_protocol::<AudioMarker>().unwrap();

    // Set volume to max.
    set_volume(&audio_proxy, vec![CHANGED_MEDIA_STREAM_SETTINGS_MAX]).await;
    verify_earcon(&mut sound_event_receiver, MAX_VOLUME_EARCON_ID, AudioRenderUsage::Background)
        .await;

    // Try to increase volume. Only serves to set the "last volume button press" event
    // to 1 (volume up).
    let buttons_event = MediaButtonsEventBuilder::new().set_volume(1).build();

    fake_services
        .input_device_registry
        .lock()
        .await
        .send_media_button_event(buttons_event.clone())
        .await;

    // Sets volume max again.
    set_volume(&audio_proxy, vec![CHANGED_MEDIA_STREAM_SETTINGS_MAX]).await;
    verify_earcon(&mut sound_event_receiver, MAX_VOLUME_EARCON_ID, AudioRenderUsage::Background)
        .await;

    // Set volume to max again, to simulate holding button.
    set_volume(&audio_proxy, vec![CHANGED_MEDIA_STREAM_SETTINGS_MAX]).await;
    verify_earcon(&mut sound_event_receiver, MAX_VOLUME_EARCON_ID, AudioRenderUsage::Background)
        .await;

    // Check that the sound played the correct number of times.
    assert_eq!(fake_services.sound_player.lock().await.get_play_count(0).await, Some(3));
}
