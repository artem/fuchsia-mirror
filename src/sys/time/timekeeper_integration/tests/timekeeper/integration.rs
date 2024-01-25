// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use fidl::{endpoints, HandleBased};
use fidl_fuchsia_testing_harness as ftth;
use fidl_fuchsia_time as fft;
use fidl_test_time_realm as fttr;
use fuchsia_component::client;
use {
    fidl_fuchsia_metrics::MetricEvent,
    fidl_fuchsia_metrics_test::{LogMethod, MetricEventLoggerQuerierProxy},
    fidl_fuchsia_time_external::TimeSample,
    fuchsia_async as fasync,
    fuchsia_cobalt_builders::MetricEventExt,
    fuchsia_zircon::{self as zx},
    futures::{stream::StreamExt, Future},
    std::sync::Arc,
    test_util::{assert_geq, assert_leq, assert_lt},
    time_metrics_registry::{
        RealTimeClockEventsMigratedMetricDimensionEventType as RtcEventType,
        TimeMetricDimensionExperiment as Experiment, TimeMetricDimensionTrack as Track,
        TimekeeperLifecycleEventsMigratedMetricDimensionEventType as LifecycleEventType,
        TimekeeperTimeSourceEventsMigratedMetricDimensionEventType as TimeSourceEvent,
        TimekeeperTrackEventsMigratedMetricDimensionEventType as TrackEvent,
        REAL_TIME_CLOCK_EVENTS_MIGRATED_METRIC_ID, TIMEKEEPER_CLOCK_CORRECTION_MIGRATED_METRIC_ID,
        TIMEKEEPER_LIFECYCLE_EVENTS_MIGRATED_METRIC_ID,
        TIMEKEEPER_SQRT_COVARIANCE_MIGRATED_METRIC_ID,
        TIMEKEEPER_TIME_SOURCE_EVENTS_MIGRATED_METRIC_ID,
        TIMEKEEPER_TRACK_EVENTS_MIGRATED_METRIC_ID,
    },
    timekeeper_integration_lib::{
        create_cobalt_event_stream, new_nonshareable_clock, poll_until, poll_until_some_async,
        rtc_time_to_zx_time, RemotePushSourcePuppet, RemoteRtcUpdates, BACKSTOP_TIME,
        BEFORE_BACKSTOP_TIME, BETWEEN_SAMPLES, STD_DEV, VALID_RTC_TIME, VALID_TIME, VALID_TIME_2,
    },
};

/// Run a test against an instance of timekeeper. Timekeeper will maintain the provided clock.
/// If `initial_rtc_time` is provided, a fake RTC device that reports the time as
/// `initial_rtc_time` is injected into timekeeper's environment. The provided `test_fn` is
/// provided with handles to manipulate the time source and observe changes to the RTC and cobalt.
async fn timekeeper_test<F, Fut>(
    clock: zx::Clock,
    initial_rtc_time: Option<zx::Time>,
    test_fn: F,
) -> Result<()>
where
    F: FnOnce(
        zx::Clock,
        Arc<RemotePushSourcePuppet>,
        RemoteRtcUpdates,
        MetricEventLoggerQuerierProxy,
    ) -> Fut,
    Fut: Future<Output = ()>,
{
    let clock_copy = clock.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("duplicated");
    let result = async move {
        let test_realm_proxy = client::connect_to_protocol::<fttr::RealmFactoryMarker>()
            .with_context(|| {
                format!(
                    "while connecting to: {}",
                    <fttr::RealmFactoryMarker as fidl::endpoints::ProtocolMarker>::DEBUG_NAME
                )
            })?;

        // _realm_keepalive must live as long as you need your test realm to live.
        let (_realm_keepalive, server_end) =
            endpoints::create_endpoints::<ftth::RealmProxy_Marker>();
        let (push_source_puppet, _opts, cobalt_metric_client) = test_realm_proxy
            .create_realm(
                fttr::RealmOptions {
                    use_real_monotonic_clock: Some(true),
                    rtc: initial_rtc_time.map(|t| fttr::RtcOptions::InitialRtcTime(t.into_nanos())),
                    ..Default::default()
                },
                clock_copy,
                server_end,
            )
            .await
            .expect("FIDL protocol error")
            .expect("Error value returned from the call");
        let cobalt = cobalt_metric_client.into_proxy().expect("infallible");
        let rtc_updates = _opts
            .rtc_updates
            .expect("rtc updates should always be present in these tests")
            .into_proxy()
            .expect("infallible");
        let rtc = RemoteRtcUpdates::new(rtc_updates);
        let push_source_puppet = push_source_puppet.into_proxy().expect("infallible");
        let push_source_controller = RemotePushSourcePuppet::new(push_source_puppet);
        tracing::debug!("timekeeper_test: about to run test_fn");
        let result = test_fn(clock, push_source_controller, rtc, cobalt).await;
        tracing::debug!("timekeeper_test: done with run test_fn");
        Ok(result)
    };
    result.await
}

#[fuchsia::test]
async fn test_no_rtc_start_clock_from_time_source_alternate_signal() {
    let clock = new_nonshareable_clock();
    timekeeper_test(clock, None, |clock, push_source_controller, _, _| async move {
        let sample_monotonic = zx::Time::get_monotonic();
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(VALID_TIME.into_nanos()),
                monotonic: Some(sample_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;

        fasync::OnSignals::new(
            &clock,
            zx::Signals::from_bits(fft::SIGNAL_UTC_CLOCK_LOGGING_QUALITY).unwrap(),
        )
        .await
        .unwrap();
    })
    .await
    .unwrap();
}

#[fuchsia::test]
async fn test_no_rtc_start_clock_from_time_source() {
    let clock = new_nonshareable_clock();
    timekeeper_test(clock, None, |clock, push_source_controller, _, cobalt| async move {
        let before_update_ticks = clock.get_details().unwrap().last_value_update_ticks;

        let sample_monotonic = zx::Time::get_monotonic();
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(VALID_TIME.into_nanos()),
                monotonic: Some(sample_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;

        tracing::info!("[https://fxbug.dev/42080434]: before CLOCK_STARTED");
        fasync::OnSignals::new(&clock, zx::Signals::CLOCK_STARTED).await.unwrap();
        tracing::info!("[https://fxbug.dev/42080434]: before SIGNAL_UTC_CLOCK_SYNCHRONIZED");
        fasync::OnSignals::new(
            &clock,
            zx::Signals::from_bits(fft::SIGNAL_UTC_CLOCK_SYNCHRONIZED).unwrap(),
        )
        .await
        .unwrap();
        tracing::info!("[https://fxbug.dev/42080434]: after SIGNAL_UTC_CLOCK_SYNCHRONIZED");
        let after_update_ticks = clock.get_details().unwrap().last_value_update_ticks;
        assert!(after_update_ticks > before_update_ticks);

        // UTC time reported by the clock should be at least the time in the sample and no
        // more than the UTC time in the sample + time elapsed since the sample was created.
        let reported_utc = clock.read().unwrap();
        let monotonic_after_update = zx::Time::get_monotonic();
        assert_geq!(reported_utc, *VALID_TIME);
        assert_leq!(reported_utc, *VALID_TIME + (monotonic_after_update - sample_monotonic));

        let cobalt_event_stream =
            create_cobalt_event_stream(Arc::new(cobalt), LogMethod::LogMetricEvents);
        tracing::info!("[https://fxbug.dev/42080434]: before cobalt_event_stream.take");
        let actual = cobalt_event_stream.take(6).collect::<Vec<_>>().await;
        assert!(
            actual.iter().any(|elem| *elem
                == MetricEvent::builder(REAL_TIME_CLOCK_EVENTS_MIGRATED_METRIC_ID)
                    .with_event_codes(RtcEventType::NoDevices)
                    .as_occurrence(1)),
            "got: {:#?}",
            actual
        );
        assert!(
            actual.iter().any(|elem| *elem
                == MetricEvent::builder(TIMEKEEPER_LIFECYCLE_EVENTS_MIGRATED_METRIC_ID)
                    .with_event_codes(LifecycleEventType::InitializedBeforeUtcStart)
                    .as_occurrence(1)),
            "got: {:#?}",
            actual
        );
        assert!(
            actual.iter().any(|elem| *elem
                == MetricEvent::builder(TIMEKEEPER_SQRT_COVARIANCE_MIGRATED_METRIC_ID)
                    .with_event_codes((Track::Primary, Experiment::None))
                    .as_integer(STD_DEV.into_micros()),),
            "got: {:#?}",
            actual
        );
        assert!(
            actual.iter().any(|elem| *elem
                == MetricEvent::builder(TIMEKEEPER_TRACK_EVENTS_MIGRATED_METRIC_ID)
                    .with_event_codes((
                        TrackEvent::EstimatedOffsetUpdated,
                        Track::Primary,
                        Experiment::None,
                    ))
                    .as_occurrence(1),),
            "got: {:#?}",
            actual
        );
    })
    .await
    .unwrap();
}

#[fuchsia::test]
async fn test_invalid_rtc_start_clock_from_time_source() {
    let clock = new_nonshareable_clock();
    timekeeper_test(
        clock,
        Some(*BEFORE_BACKSTOP_TIME),
        |clock, push_source_controller, rtc_updates, cobalt| async move {
            let mut cobalt_event_stream =
                create_cobalt_event_stream(Arc::new(cobalt), LogMethod::LogMetricEvents);
            // Timekeeper should reject the RTC time.
            tracing::info!("[https://fxbug.dev/42080434]: before cobalt_event_stream.take");
            assert_eq!(
                cobalt_event_stream.by_ref().take(2).collect::<Vec<MetricEvent>>().await,
                vec![
                    MetricEvent::builder(TIMEKEEPER_LIFECYCLE_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(LifecycleEventType::InitializedBeforeUtcStart)
                        .as_occurrence(1),
                    MetricEvent::builder(REAL_TIME_CLOCK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(RtcEventType::ReadInvalidBeforeBackstop)
                        .as_occurrence(1)
                ]
            );

            let sample_monotonic = zx::Time::get_monotonic();
            tracing::info!(
                "[https://fxbug.dev/42080434]: before push_source_controller.set_sample"
            );
            push_source_controller
                .set_sample(TimeSample {
                    utc: Some(VALID_TIME.into_nanos()),
                    monotonic: Some(sample_monotonic.into_nanos()),
                    standard_deviation: Some(STD_DEV.into_nanos()),
                    ..Default::default()
                })
                .await;

            // Timekeeper should accept the time from the time source.
            tracing::info!("[https://fxbug.dev/42080434]: before CLOCK_STARTED");
            fasync::OnSignals::new(&clock, zx::Signals::CLOCK_STARTED).await.unwrap();
            fasync::OnSignals::new(
                &clock,
                zx::Signals::from_bits(fft::SIGNAL_UTC_CLOCK_SYNCHRONIZED).unwrap(),
            )
            .await
            .unwrap();
            // UTC time reported by the clock should be at least the time reported by the time
            // source, and no more than the UTC time reported by the time source + time elapsed
            // since the time was read.
            let reported_utc = clock.read().unwrap();
            let monotonic_after = zx::Time::get_monotonic();
            assert_geq!(reported_utc, *VALID_TIME);
            assert_leq!(reported_utc, *VALID_TIME + (monotonic_after - sample_monotonic));
            // RTC should also be set.
            let rtc_update = poll_until_some_async!(async { rtc_updates.to_vec().await.pop() });
            let monotonic_after_rtc_set = zx::Time::get_monotonic();
            let rtc_reported_utc = rtc_time_to_zx_time(rtc_update);
            assert_geq!(rtc_reported_utc, *VALID_TIME);
            assert_leq!(
                rtc_reported_utc,
                *VALID_TIME + (monotonic_after_rtc_set - sample_monotonic)
            );
            assert_eq!(
                cobalt_event_stream.take(4).collect::<Vec<_>>().await,
                vec![
                    MetricEvent::builder(TIMEKEEPER_TRACK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes((
                            TrackEvent::EstimatedOffsetUpdated,
                            Track::Primary,
                            Experiment::None
                        ))
                        .as_occurrence(1),
                    MetricEvent::builder(TIMEKEEPER_SQRT_COVARIANCE_MIGRATED_METRIC_ID)
                        .with_event_codes((Track::Primary, Experiment::None))
                        .as_integer(STD_DEV.into_micros()),
                    MetricEvent::builder(TIMEKEEPER_LIFECYCLE_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(LifecycleEventType::StartedUtcFromTimeSource)
                        .as_occurrence(1),
                    MetricEvent::builder(REAL_TIME_CLOCK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(RtcEventType::WriteSucceeded)
                        .as_occurrence(1)
                ]
            );
        },
    )
    .await
    .unwrap();
}

#[fuchsia::test]
async fn test_start_clock_from_rtc() {
    let clock = new_nonshareable_clock();
    let monotonic_before = zx::Time::get_monotonic();
    timekeeper_test(
        clock,
        Some(*VALID_RTC_TIME),
        |clock, push_source_controller, rtc_updates, cobalt| async move {
            let mut cobalt_event_stream =
                create_cobalt_event_stream(Arc::new(cobalt), LogMethod::LogMetricEvents);

            // Clock should start from the time read off the RTC.
            tracing::info!("[https://fxbug.dev/42080434]: before CLOCK_STARTED");
            fasync::OnSignals::new(&clock, zx::Signals::CLOCK_STARTED).await.unwrap();

            // UTC time reported by the clock should be at least the time reported by the RTC, and no
            // more than the UTC time reported by the RTC + time elapsed since Timekeeper was launched.
            let reported_utc = clock.read().unwrap();
            let monotonic_after = zx::Time::get_monotonic();
            assert_geq!(reported_utc, *VALID_RTC_TIME);
            assert_leq!(reported_utc, *VALID_RTC_TIME + (monotonic_after - monotonic_before));

            tracing::info!("[https://fxbug.dev/42080434]: before cobalt_event_stream.take");
            assert_eq!(
                cobalt_event_stream.by_ref().take(3).collect::<Vec<MetricEvent>>().await,
                vec![
                    MetricEvent::builder(TIMEKEEPER_LIFECYCLE_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(LifecycleEventType::InitializedBeforeUtcStart)
                        .as_occurrence(1),
                    MetricEvent::builder(REAL_TIME_CLOCK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(RtcEventType::ReadSucceeded)
                        .as_occurrence(1),
                    MetricEvent::builder(TIMEKEEPER_LIFECYCLE_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(LifecycleEventType::StartedUtcFromRtc)
                        .as_occurrence(1),
                ]
            );

            // Clock should be updated again when the push source reports another time.
            let clock_last_set_ticks = clock.get_details().unwrap().last_value_update_ticks;
            let sample_monotonic = zx::Time::get_monotonic();
            tracing::info!(
                "[https://fxbug.dev/42080434]: before push_source_controller.set_sample"
            );
            push_source_controller
                .set_sample(TimeSample {
                    utc: Some(VALID_TIME.into_nanos()),
                    monotonic: Some(sample_monotonic.into_nanos()),
                    standard_deviation: Some(STD_DEV.into_nanos()),
                    ..Default::default()
                })
                .await;
            tracing::info!(
                "[https://fxbug.dev/42080434]: after push_source_controller.set_sample stage 1"
            );
            poll_until!(|| {
                clock.get_details().unwrap().last_value_update_ticks != clock_last_set_ticks
            })
            .await;
            tracing::info!(
                "[https://fxbug.dev/42080434]: after push_source_controller.set_sample stage 2"
            );
            let clock_utc = clock.read().unwrap();
            let monotonic_after_read = zx::Time::get_monotonic();
            assert_geq!(clock_utc, *VALID_TIME);
            assert_leq!(clock_utc, *VALID_TIME + (monotonic_after_read - sample_monotonic));
            // RTC should be set too.
            let rtc_update = poll_until_some_async!(async { rtc_updates.to_vec().await.pop() });
            let monotonic_after_rtc_set = zx::Time::get_monotonic();
            let rtc_reported_utc = rtc_time_to_zx_time(rtc_update);
            assert_geq!(rtc_reported_utc, *VALID_TIME);
            assert_leq!(
                rtc_reported_utc,
                *VALID_TIME + (monotonic_after_rtc_set - sample_monotonic)
            );

            assert_eq!(
                cobalt_event_stream.by_ref().take(3).collect::<Vec<MetricEvent>>().await,
                vec![
                    MetricEvent::builder(TIMEKEEPER_TRACK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes((
                            TrackEvent::EstimatedOffsetUpdated,
                            Track::Primary,
                            Experiment::None
                        ))
                        .as_occurrence(1),
                    MetricEvent::builder(TIMEKEEPER_SQRT_COVARIANCE_MIGRATED_METRIC_ID)
                        .with_event_codes((Track::Primary, Experiment::None))
                        .as_integer(STD_DEV.into_micros()),
                    MetricEvent::builder(TIMEKEEPER_TRACK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes((
                            TrackEvent::CorrectionByStep,
                            Track::Primary,
                            Experiment::None
                        ))
                        .as_occurrence(1),
                ]
            );

            // A correction value always follows a CorrectionBy* event. Verify metric type but rely
            // on unit test to verify content since we can't predict exactly what time will be used.
            assert_eq!(
                cobalt_event_stream.by_ref().take(1).collect::<Vec<MetricEvent>>().await[0]
                    .metric_id,
                TIMEKEEPER_CLOCK_CORRECTION_MIGRATED_METRIC_ID
            );

            assert_eq!(
                cobalt_event_stream.by_ref().take(2).collect::<Vec<MetricEvent>>().await,
                vec![
                    MetricEvent::builder(TIMEKEEPER_TRACK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes((
                            TrackEvent::ClockUpdateTimeStep,
                            Track::Primary,
                            Experiment::None
                        ))
                        .as_occurrence(1),
                    MetricEvent::builder(REAL_TIME_CLOCK_EVENTS_MIGRATED_METRIC_ID)
                        .with_event_codes(RtcEventType::WriteSucceeded)
                        .as_occurrence(1),
                ]
            );
        },
    )
    .await
    .unwrap();
}

#[fuchsia::test]
async fn test_start_clock_from_rtc_alternate_signal() {
    let clock = new_nonshareable_clock();
    timekeeper_test(clock, Some(*VALID_RTC_TIME), |clock, _, _, _| async move {
        // Clock should start from the time read off the RTC.
        fasync::OnSignals::new(
            &clock,
            zx::Signals::from_bits(fft::SIGNAL_UTC_CLOCK_LOGGING_QUALITY).unwrap(),
        )
        .await
        .unwrap();
    })
    .await
    .unwrap();
}

#[fuchsia::test]
async fn test_reject_before_backstop() {
    let clock = new_nonshareable_clock();
    timekeeper_test(clock, None, |clock, push_source_controller, _, cobalt| async move {
        let cobalt_event_stream =
            create_cobalt_event_stream(Arc::new(cobalt), LogMethod::LogMetricEvents);

        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(BEFORE_BACKSTOP_TIME.into_nanos()),
                monotonic: Some(zx::Time::get_monotonic().into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;

        // Wait for the sample rejected event to be sent to Cobalt.
        tracing::info!("[https://fxbug.dev/42080434]: before cobalt_event_stream.take");
        cobalt_event_stream
            .take_while(|event| {
                let is_reject_sample_event = event.metric_id
                    == TIMEKEEPER_TIME_SOURCE_EVENTS_MIGRATED_METRIC_ID
                    && event
                        .event_codes
                        .contains(&(TimeSourceEvent::SampleRejectedBeforeBackstop as u32));
                futures::future::ready(is_reject_sample_event)
            })
            .collect::<Vec<_>>()
            .await;
        // Clock should not have been rewound to before backstop.
        assert_leq!(*BACKSTOP_TIME, clock.read().unwrap());
    })
    .await
    .unwrap();
}

#[fuchsia::test]
async fn test_slew_clock() {
    // Constants for controlling the duration of the slew we want to induce. These constants
    // are intended to tune the test to avoid flakes and do not necessarily need to match up with
    // those in timekeeper.
    const SLEW_DURATION: zx::Duration = zx::Duration::from_minutes(90);
    const NOMINAL_SLEW_PPM: i64 = 20;
    let error_for_slew = SLEW_DURATION * NOMINAL_SLEW_PPM / 1_000_000;

    let clock = new_nonshareable_clock();
    timekeeper_test(clock, None, |clock, push_source_controller, _, _| async move {
        // Let the first sample be slightly in the past so later samples are not in the future.
        let sample_1_monotonic = zx::Time::get_monotonic() - BETWEEN_SAMPLES;
        let sample_1_utc = *VALID_TIME;
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(sample_1_utc.into_nanos()),
                monotonic: Some(sample_1_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;

        // After the first sample, the clock is started, and running at the same rate as
        // the reference.
        tracing::info!("[https://fxbug.dev/42080434]: before CLOCK_STARTED");
        fasync::OnSignals::new(&clock, zx::Signals::CLOCK_STARTED).await.unwrap();
        tracing::info!("[https://fxbug.dev/42080434]: before SIGNAL_UTC_CLOCK_SYNCHRONIZED");
        fasync::OnSignals::new(
            &clock,
            zx::Signals::from_bits(fft::SIGNAL_UTC_CLOCK_SYNCHRONIZED).unwrap(),
        )
        .await
        .unwrap();
        let clock_rate = clock.get_details().unwrap().mono_to_synthetic.rate;
        assert_eq!(clock_rate.reference_ticks, clock_rate.synthetic_ticks);
        let last_generation_counter = clock.get_details().unwrap().generation_counter;

        // Push a second sample that indicates UTC running slightly behind monotonic.
        let sample_2_monotonic = sample_1_monotonic + BETWEEN_SAMPLES;
        let sample_2_utc = sample_1_utc + BETWEEN_SAMPLES - error_for_slew * 2;
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample 2");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(sample_2_utc.into_nanos()),
                monotonic: Some(sample_2_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;

        // After the second sample, the clock is running slightly slower than the reference.
        poll_until!(|| clock.get_details().unwrap().generation_counter != last_generation_counter)
            .await;
        let slew_rate = clock.get_details().unwrap().mono_to_synthetic.rate;
        assert_lt!(slew_rate.synthetic_ticks, slew_rate.reference_ticks);

        // TODO(https://fxbug.dev/42143927) - verify that the slew completes.
    })
    .await
    .unwrap();
}

#[fuchsia::test]
async fn test_step_clock() {
    const STEP_ERROR: zx::Duration = zx::Duration::from_hours(1);
    let clock = new_nonshareable_clock();
    timekeeper_test(clock, None, |clock, push_source_controller, _, _| async move {
        // Let the first sample be slightly in the past so later samples are not in the future.
        let monotonic_before = zx::Time::get_monotonic();
        let sample_1_monotonic = monotonic_before - BETWEEN_SAMPLES;
        let sample_1_utc = *VALID_TIME;
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(sample_1_utc.into_nanos()),
                monotonic: Some(sample_1_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;

        // Wait until the clock is running and synchronized before testing.
        tracing::info!("[https://fxbug.dev/42080434]: before CLOCK_STARTED");
        fasync::OnSignals::new(&clock, zx::Signals::CLOCK_STARTED).await.unwrap();
        tracing::info!("[https://fxbug.dev/42080434]: before SIGNAL_UTC_CLOCK_SYNCHRONIZED");
        fasync::OnSignals::new(
            &clock,
            zx::Signals::from_bits(fft::SIGNAL_UTC_CLOCK_SYNCHRONIZED).unwrap(),
        )
        .await
        .unwrap();
        let utc_now = clock.read().unwrap();
        let monotonic_after = zx::Time::get_monotonic();
        assert_geq!(utc_now, sample_1_utc + BETWEEN_SAMPLES);
        assert_leq!(utc_now, sample_1_utc + BETWEEN_SAMPLES + (monotonic_after - monotonic_before));

        let clock_last_set_ticks = clock.get_details().unwrap().last_value_update_ticks;

        let sample_2_monotonic = sample_1_monotonic + BETWEEN_SAMPLES;
        let sample_2_utc = sample_1_utc + BETWEEN_SAMPLES + STEP_ERROR;
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample 2");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(sample_2_utc.into_nanos()),
                monotonic: Some(sample_2_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;
        poll_until!(
                || clock.get_details().unwrap().last_value_update_ticks != clock_last_set_ticks
            )
            .await;
        let utc_now_2 = clock.read().unwrap();
        let monotonic_after_2 = zx::Time::get_monotonic();

        // After the second sample, the clock should have jumped to an offset approximately halfway
        // between the offsets defined in the two samples. 500 ms is added to the upper bound as
        // the estimate takes more of the second sample into account (as the oscillator drift is
        // added to the uncertainty of the first sample).
        let jump_utc = sample_2_utc - STEP_ERROR / 2;
        assert_geq!(utc_now_2, jump_utc);
        assert_leq!(
            utc_now_2,
            jump_utc + (monotonic_after_2 - monotonic_before) + zx::Duration::from_millis(500)
        );
    })
    .await
    .unwrap();
}

fn avg(time_1: zx::Time, time_2: zx::Time) -> zx::Time {
    let time_1 = time_1.into_nanos() as i128;
    let time_2 = time_2.into_nanos() as i128;
    let avg = (time_1 + time_2) / 2;
    zx::Time::from_nanos(avg as i64)
}

#[fuchsia::test]
async fn test_restart_crashed_time_source() {
    let clock = new_nonshareable_clock();
    timekeeper_test(clock, None, |clock, push_source_controller, _, _| async move {
        // Let the first sample be slightly in the past so later samples are not in the future.
        let monotonic_before = zx::Time::get_monotonic();
        let sample_1_monotonic = monotonic_before - BETWEEN_SAMPLES;
        let sample_1_utc = *VALID_TIME;
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(sample_1_utc.into_nanos()),
                monotonic: Some(sample_1_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;

        // After the first sample, the clock is started.
        tracing::info!("[https://fxbug.dev/42080434]: before CLOCK_STARTED");
        fasync::OnSignals::new(&clock, zx::Signals::CLOCK_STARTED).await.unwrap();
        let last_generation_counter = clock.get_details().unwrap().generation_counter;

        // After a time source crashes, timekeeper should restart it and accept samples from it.
        let _result = push_source_controller.simulate_crash();
        let sample_2_utc = *VALID_TIME_2;
        let sample_2_monotonic = sample_1_monotonic + BETWEEN_SAMPLES;
        tracing::info!("[https://fxbug.dev/42080434]: before push_source_controller.set_sample 2");
        push_source_controller
            .set_sample(TimeSample {
                utc: Some(sample_2_utc.into_nanos()),
                monotonic: Some(sample_2_monotonic.into_nanos()),
                standard_deviation: Some(STD_DEV.into_nanos()),
                ..Default::default()
            })
            .await;
        poll_until!(|| clock.get_details().unwrap().generation_counter != last_generation_counter)
            .await;
        // Time from clock should incorporate the second sample.
        let result_utc = clock.read().unwrap();
        let monotonic_after = zx::Time::get_monotonic();
        let minimum_expected = avg(sample_1_utc + BETWEEN_SAMPLES, sample_2_utc)
            + (monotonic_after - monotonic_before);
        assert_geq!(result_utc, minimum_expected);
    })
    .await
    .unwrap();
}
