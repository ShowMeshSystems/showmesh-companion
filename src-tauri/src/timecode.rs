use serde::Serialize;

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
    pub fn foundation() -> Self {
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

    pub fn from_settings(settings: &TimecodeSettings) -> Self {
        if !settings.enabled {
            Self::disabled()
        } else if settings.device_id.trim().is_empty() {
            Self::not_configured()
        } else {
            Self::foundation()
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

    #[test]
    fn disabled_settings_report_disabled_state() {
        let settings = TimecodeSettings {
            enabled: false,
            ..TimecodeSettings::default()
        };
        assert_eq!(
            TimecodeStatus::from_settings(&settings).state,
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
            TimecodeStatus::from_settings(&settings).state,
            TimecodeState::NotConfigured
        );
    }

    #[test]
    fn enabled_with_device_reports_not_started() {
        let settings = TimecodeSettings {
            enabled: true,
            device_id: "device-1".into(),
            ..TimecodeSettings::default()
        };
        assert_eq!(
            TimecodeStatus::from_settings(&settings).state,
            TimecodeState::NotStarted
        );
    }
}
