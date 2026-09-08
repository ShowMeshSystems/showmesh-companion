use serde::Serialize;

use crate::audio_devices::{self, AudioInputDevice};
use crate::capture::CaptureObservation;
use crate::ltc;
use crate::settings::TimecodeSettings;

const NO_AUDIO_THRESHOLD_MS: u64 = 1_000;
const LOCK_THRESHOLD_MS: u64 = 250;
const NEAR_SILENCE_LEVEL: f32 = 0.02;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
pub enum TimecodeState {
    Disabled,
    NotConfigured,
    NotStarted,
    DeviceUnavailable,
    ChannelUnavailable,
    NoAudio,
    NoLock,
    Locked,
    DecodeFailed,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedFrame {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
}

impl From<ltc::Frame> for ObservedFrame {
    fn from(frame: ltc::Frame) -> Self {
        Self {
            hours: frame.hours,
            minutes: frame.minutes,
            seconds: frame.seconds,
            frames: frame.frames,
            drop_frame: frame.drop_frame,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimecodeStatus {
    pub state: TimecodeState,
    pub reason: String,
    pub operational: bool,
    pub level: Option<f32>,
    pub frame: Option<ObservedFrame>,
    pub frame_age_ms: Option<u64>,
    pub detected_frame_rate: Option<LtcFrameRate>,
    pub frame_rate_comparison: FrameRateComparison,
}

impl TimecodeStatus {
    fn base(state: TimecodeState, reason: String, operational: bool) -> Self {
        Self {
            state,
            reason,
            operational,
            level: None,
            frame: None,
            frame_age_ms: None,
            detected_frame_rate: None,
            frame_rate_comparison: FrameRateComparison::UnknownExpectedUnobserved,
        }
    }

    pub fn not_started() -> Self {
        Self::base(
            TimecodeState::NotStarted,
            "LTC capture is starting.".into(),
            false,
        )
    }

    pub fn disabled() -> Self {
        Self::base(
            TimecodeState::Disabled,
            "LTC observation is disabled in settings.".into(),
            false,
        )
    }

    pub fn not_configured() -> Self {
        Self::base(
            TimecodeState::NotConfigured,
            "Choose an audio input before LTC observation can start.".into(),
            false,
        )
    }

    pub fn device_unavailable() -> Self {
        Self::base(
            TimecodeState::DeviceUnavailable,
            "The configured audio input device was not found on this computer.".into(),
            false,
        )
    }

    pub fn channel_unavailable() -> Self {
        Self::base(
            TimecodeState::ChannelUnavailable,
            "The configured channel exceeds the channel count of the configured audio input device.".into(),
            false,
        )
    }

    pub fn not_enumerated() -> Self {
        Self::base(
            TimecodeState::NotStarted,
            "Audio inputs have not been enumerated yet, so the configured input has not been checked.".into(),
            false,
        )
    }

    pub fn from_settings(
        settings: &TimecodeSettings,
        devices: Option<&[AudioInputDevice]>,
    ) -> Self {
        if !settings.enabled {
            return Self::disabled();
        }
        let device_id = settings.device_id.trim();
        if device_id.is_empty() {
            return Self::not_configured();
        }
        let Some(devices) = devices else {
            return Self::not_enumerated();
        };
        match audio_devices::find(devices, device_id) {
            None => Self::device_unavailable(),
            Some(device) => match device.channels {
                Some(channels) if settings.channel > channels => Self::channel_unavailable(),
                _ => Self::not_started(),
            },
        }
    }

    pub fn from_capture(observation: &CaptureObservation, now_ms: u64) -> Self {
        let state = derive_capture_state(
            observation.started_at_ms,
            observation.last_sample_at_ms,
            observation.last_frame_at_ms,
            observation.stream_error.is_some(),
            now_ms,
        );
        let detected_frame_rate = map_frame_rate(observation.detected_rate);
        let frame_rate_comparison = match detected_frame_rate {
            Some(rate) => compare_frame_rate(rate, ExpectedFrameRate::Unobserved),
            None => FrameRateComparison::UnknownExpectedUnobserved,
        };
        let frame = observation.last_frame.map(ObservedFrame::from);
        let frame_age_ms = observation
            .last_frame_at_ms
            .map(|frame_at| now_ms.saturating_sub(frame_at));
        let level = Some(observation.level);
        let near_silence = observation.level < NEAR_SILENCE_LEVEL;

        let reason = match state {
            TimecodeState::NotStarted => "LTC capture is starting.".to_string(),
            TimecodeState::NoAudio => format!(
                "No audio has been received from the configured input in the last second. Observed level: {:.4}.",
                observation.level
            ),
            TimecodeState::NoLock => format!(
                "Audio is arriving from the configured input, but no LTC frame has decoded in the last 250 milliseconds. Observed level: {:.4}{}.",
                observation.level,
                if near_silence { " (near silence)" } else { "" }
            ),
            TimecodeState::Locked => {
                "An LTC frame decoded within the last 250 milliseconds.".to_string()
            }
            TimecodeState::DecodeFailed => observation
                .stream_error
                .clone()
                .unwrap_or_else(|| "The audio input stream failed.".to_string()),
            TimecodeState::Disabled
            | TimecodeState::NotConfigured
            | TimecodeState::DeviceUnavailable
            | TimecodeState::ChannelUnavailable => "LTC capture is starting.".to_string(),
        };
        let operational = matches!(state, TimecodeState::Locked);

        Self {
            state,
            reason,
            operational,
            level,
            frame,
            frame_age_ms,
            detected_frame_rate,
            frame_rate_comparison,
        }
    }

    pub fn combine(
        settings_status: TimecodeStatus,
        observation: Option<&CaptureObservation>,
        now_ms: u64,
    ) -> Self {
        if !matches!(settings_status.state, TimecodeState::NotStarted) {
            return settings_status;
        }
        match observation {
            Some(observation) => Self::from_capture(observation, now_ms),
            None => settings_status,
        }
    }
}

fn derive_capture_state(
    started_at_ms: Option<u64>,
    last_sample_at_ms: Option<u64>,
    last_frame_at_ms: Option<u64>,
    stream_error: bool,
    now_ms: u64,
) -> TimecodeState {
    if stream_error {
        return TimecodeState::DecodeFailed;
    }
    match last_sample_at_ms {
        Some(sample_at) if now_ms.saturating_sub(sample_at) >= NO_AUDIO_THRESHOLD_MS => {
            TimecodeState::NoAudio
        }
        Some(_) => match last_frame_at_ms {
            Some(frame_at) if now_ms.saturating_sub(frame_at) < LOCK_THRESHOLD_MS => {
                TimecodeState::Locked
            }
            _ => TimecodeState::NoLock,
        },
        None => match started_at_ms {
            Some(started_at) if now_ms.saturating_sub(started_at) >= NO_AUDIO_THRESHOLD_MS => {
                TimecodeState::NoAudio
            }
            _ => TimecodeState::NotStarted,
        },
    }
}

fn map_frame_rate(rate: ltc::FrameRate) -> Option<LtcFrameRate> {
    match rate {
        ltc::FrameRate::Fps24 => Some(LtcFrameRate::Fps24),
        ltc::FrameRate::Fps25 => Some(LtcFrameRate::Fps25),
        ltc::FrameRate::Fps2997Drop => Some(LtcFrameRate::Fps29_97Df),
        ltc::FrameRate::Fps2997NonDrop => Some(LtcFrameRate::Fps29_97Ndf),
        ltc::FrameRate::Fps30 => Some(LtcFrameRate::Fps30),
        ltc::FrameRate::Unknown => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[allow(dead_code)]
pub enum LtcFrameRate {
    #[serde(rename = "24")]
    Fps24,
    #[serde(rename = "25")]
    Fps25,
    #[serde(rename = "29.97-df")]
    Fps29_97Df,
    #[serde(rename = "29.97-ndf")]
    Fps29_97Ndf,
    #[serde(rename = "30")]
    Fps30,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
pub enum ExpectedFrameRate {
    Unobserved,
    Known(LtcFrameRate),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
pub enum FrameRateComparison {
    Match,
    Mismatch,
    UnknownExpectedUnobserved,
}

#[allow(dead_code)]
pub fn compare_frame_rate(
    observed: LtcFrameRate,
    expected: ExpectedFrameRate,
) -> FrameRateComparison {
    match expected {
        ExpectedFrameRate::Unobserved => FrameRateComparison::UnknownExpectedUnobserved,
        ExpectedFrameRate::Known(rate) => {
            if rate == observed {
                FrameRateComparison::Match
            } else {
                FrameRateComparison::Mismatch
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unenumerated_device_list_is_not_reported_as_unavailable() {
        let settings = TimecodeSettings {
            enabled: true,
            device_id: "USB Interface".into(),
            channel: 1,
        };
        let status = TimecodeStatus::from_settings(&settings, None);
        assert_eq!(status.state, TimecodeState::NotStarted);
        assert!(status.reason.contains("not been enumerated"));
    }

    #[test]
    fn matching_rates_report_match() {
        assert_eq!(
            compare_frame_rate(
                LtcFrameRate::Fps25,
                ExpectedFrameRate::Known(LtcFrameRate::Fps25)
            ),
            FrameRateComparison::Match
        );
    }

    #[test]
    fn differing_rates_report_mismatch() {
        assert_eq!(
            compare_frame_rate(
                LtcFrameRate::Fps25,
                ExpectedFrameRate::Known(LtcFrameRate::Fps30)
            ),
            FrameRateComparison::Mismatch
        );
    }

    #[test]
    fn unobserved_expected_rate_never_reports_a_mismatch() {
        for observed in [
            LtcFrameRate::Fps24,
            LtcFrameRate::Fps25,
            LtcFrameRate::Fps29_97Df,
            LtcFrameRate::Fps29_97Ndf,
            LtcFrameRate::Fps30,
        ] {
            assert_eq!(
                compare_frame_rate(observed, ExpectedFrameRate::Unobserved),
                FrameRateComparison::UnknownExpectedUnobserved
            );
        }
    }

    fn test_device(id: &str, channels: Option<u16>) -> AudioInputDevice {
        AudioInputDevice {
            id: id.into(),
            name: id.into(),
            channels,
            default_sample_rate: channels.map(|_| 48_000),
            config_known: channels.is_some(),
        }
    }

    #[test]
    fn disabled_settings_report_disabled_state() {
        let settings = TimecodeSettings {
            enabled: false,
            ..TimecodeSettings::default()
        };
        assert_eq!(
            TimecodeStatus::from_settings(&settings, Some(&[])).state,
            TimecodeState::Disabled
        );
    }

    #[test]
    fn enabled_without_device_reports_not_configured() {
        let settings = TimecodeSettings {
            enabled: true,
            ..TimecodeSettings::default()
        };
        assert_eq!(
            TimecodeStatus::from_settings(&settings, Some(&[])).state,
            TimecodeState::NotConfigured
        );
    }

    #[test]
    fn enabled_with_missing_device_reports_device_unavailable() {
        let settings = TimecodeSettings {
            enabled: true,
            device_id: "device-1".into(),
            ..TimecodeSettings::default()
        };
        assert_eq!(
            TimecodeStatus::from_settings(&settings, Some(&[])).state,
            TimecodeState::DeviceUnavailable
        );
    }

    #[test]
    fn enabled_with_matching_device_reports_not_started() {
        let settings = TimecodeSettings {
            enabled: true,
            device_id: "device-1".into(),
            channel: 1,
        };
        let devices = [test_device("device-1", Some(2))];
        assert_eq!(
            TimecodeStatus::from_settings(&settings, Some(&devices)).state,
            TimecodeState::NotStarted
        );
    }

    #[test]
    fn enabled_with_channel_beyond_device_count_reports_channel_unavailable() {
        let settings = TimecodeSettings {
            enabled: true,
            device_id: "device-1".into(),
            channel: 3,
        };
        let devices = [test_device("device-1", Some(2))];
        assert_eq!(
            TimecodeStatus::from_settings(&settings, Some(&devices)).state,
            TimecodeState::ChannelUnavailable
        );
    }

    #[test]
    fn enabled_with_device_of_unknown_channel_count_reports_not_started() {
        let settings = TimecodeSettings {
            enabled: true,
            device_id: "device-1".into(),
            channel: 5,
        };
        let devices = [test_device("device-1", None)];
        assert_eq!(
            TimecodeStatus::from_settings(&settings, Some(&devices)).state,
            TimecodeState::NotStarted
        );
    }

    #[test]
    fn never_falls_back_to_a_different_device_when_configured_one_is_absent() {
        let settings = TimecodeSettings {
            enabled: true,
            device_id: "missing-device".into(),
            ..TimecodeSettings::default()
        };
        let devices = [
            test_device("device-1", Some(2)),
            test_device("device-2", Some(4)),
        ];
        assert_eq!(
            TimecodeStatus::from_settings(&settings, Some(&devices)).state,
            TimecodeState::DeviceUnavailable
        );
    }

    #[test]
    fn no_sample_within_grace_window_reports_not_started() {
        assert_eq!(
            derive_capture_state(Some(1_000), None, None, false, 1_500),
            TimecodeState::NotStarted
        );
    }

    #[test]
    fn no_sample_past_the_grace_window_reports_no_audio() {
        assert_eq!(
            derive_capture_state(Some(1_000), None, None, false, 2_001),
            TimecodeState::NoAudio
        );
    }

    #[test]
    fn a_stale_sample_reports_no_audio_even_with_a_recent_frame() {
        assert_eq!(
            derive_capture_state(Some(0), Some(1_000), Some(1_100), false, 2_001),
            TimecodeState::NoAudio
        );
    }

    #[test]
    fn a_recent_sample_without_a_recent_frame_reports_no_lock() {
        assert_eq!(
            derive_capture_state(Some(0), Some(5_000), None, false, 5_100),
            TimecodeState::NoLock
        );
        assert_eq!(
            derive_capture_state(Some(0), Some(5_000), Some(4_000), false, 5_100),
            TimecodeState::NoLock
        );
    }

    #[test]
    fn a_recent_sample_with_a_recent_frame_reports_locked() {
        assert_eq!(
            derive_capture_state(Some(0), Some(5_000), Some(4_900), false, 5_100),
            TimecodeState::Locked
        );
    }

    #[test]
    fn a_stream_error_always_reports_decode_failed() {
        assert_eq!(
            derive_capture_state(Some(0), Some(5_000), Some(4_900), true, 5_100),
            TimecodeState::DecodeFailed
        );
    }

    #[test]
    fn from_capture_surfaces_the_observed_level_and_detected_rate() {
        let observation = CaptureObservation {
            started_at_ms: Some(0),
            last_sample_at_ms: Some(1_000),
            last_frame_at_ms: Some(950),
            last_frame: Some(ltc::Frame {
                hours: 1,
                minutes: 2,
                seconds: 3,
                frames: 4,
                drop_frame: false,
                sample_index: 0,
            }),
            level: 0.42,
            detected_rate: ltc::FrameRate::Fps25,
            stream_error: None,
        };
        let status = TimecodeStatus::from_capture(&observation, 1_100);
        assert_eq!(status.state, TimecodeState::Locked);
        assert!(status.operational);
        assert_eq!(status.level, Some(0.42));
        assert_eq!(status.detected_frame_rate, Some(LtcFrameRate::Fps25));
        assert_eq!(
            status.frame_rate_comparison,
            FrameRateComparison::UnknownExpectedUnobserved
        );
        assert_eq!(status.frame.map(|frame| frame.hours), Some(1));
    }

    #[test]
    fn combine_prefers_settings_derived_states_over_stale_capture_state() {
        let settings_status = TimecodeStatus::disabled();
        let observation = CaptureObservation {
            started_at_ms: Some(0),
            last_sample_at_ms: Some(1_000),
            last_frame_at_ms: Some(950),
            ..CaptureObservation::default()
        };
        let combined = TimecodeStatus::combine(settings_status, Some(&observation), 1_100);
        assert_eq!(combined.state, TimecodeState::Disabled);
    }

    #[test]
    fn combine_uses_capture_state_once_settings_allow_it_to_start() {
        let settings_status = TimecodeStatus::not_started();
        let observation = CaptureObservation {
            started_at_ms: Some(0),
            last_sample_at_ms: Some(1_000),
            last_frame_at_ms: None,
            ..CaptureObservation::default()
        };
        let combined = TimecodeStatus::combine(settings_status, Some(&observation), 1_100);
        assert_eq!(combined.state, TimecodeState::NoLock);
    }
}
