use serde::Serialize;

use crate::audio_devices::{self, AudioInputDevice};
use crate::settings::TimecodeSettings;

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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimecodeStatus {
    pub state: TimecodeState,
    pub reason: String,
    pub operational: bool,
}

impl TimecodeStatus {
    pub fn not_started() -> Self {
        Self {
            state: TimecodeState::NotStarted,
            reason:
                "The timecode module is configured but LTC capture has not been implemented yet."
                    .into(),
            operational: false,
        }
    }

    pub fn disabled() -> Self {
        Self {
            state: TimecodeState::Disabled,
            reason: "LTC observation is disabled in settings.".into(),
            operational: false,
        }
    }

    pub fn not_configured() -> Self {
        Self {
            state: TimecodeState::NotConfigured,
            reason: "Choose an audio input before LTC observation can start.".into(),
            operational: false,
        }
    }

    pub fn device_unavailable() -> Self {
        Self {
            state: TimecodeState::DeviceUnavailable,
            reason: "The configured audio input device was not found on this computer.".into(),
            operational: false,
        }
    }

    pub fn channel_unavailable() -> Self {
        Self {
            state: TimecodeState::ChannelUnavailable,
            reason: "The configured channel exceeds the channel count of the configured audio input device.".into(),
            operational: false,
        }
    }

    pub fn not_enumerated() -> Self {
        Self {
            state: TimecodeState::NotStarted,
            reason: "Audio inputs have not been enumerated yet, so the configured input has not been checked.".into(),
            operational: false,
        }
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
}
