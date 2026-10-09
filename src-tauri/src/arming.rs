use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArmingPolicy {
    #[default]
    FollowShowNight,
    AlwaysArmed,
    AlwaysDisarmed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArmingOverride {
    #[default]
    None,
    ArmUntilNightEnds,
    DisarmUntilNextNight,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NightSessionState {
    #[default]
    Unknown,
    Inactive,
    Preparing,
    Preshow,
    TransitionToShow,
    Live,
    TransitionToResting,
    RestingIntershow,
    EndOfNightResting,
    FadingOut,
    Stopped,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ArmingDecision {
    pub armed: bool,
    pub reason: String,
}

pub fn resolve(
    policy: ArmingPolicy,
    night: NightSessionState,
    override_value: ArmingOverride,
) -> ArmingDecision {
    match override_value {
        ArmingOverride::ArmUntilNightEnds => {
            return decision(true, "Manual override: armed.");
        }
        ArmingOverride::DisarmUntilNextNight => {
            return decision(false, "Manual override: disarmed.");
        }
        ArmingOverride::None => {}
    }

    match policy {
        ArmingPolicy::AlwaysArmed => decision(true, "Settings policy is Always armed."),
        ArmingPolicy::AlwaysDisarmed => decision(false, "Settings policy is Always disarmed."),
        ArmingPolicy::FollowShowNight => match night {
            NightSessionState::Preparing
            | NightSessionState::Preshow
            | NightSessionState::TransitionToShow
            | NightSessionState::Live
            | NightSessionState::TransitionToResting
            | NightSessionState::RestingIntershow
            | NightSessionState::EndOfNightResting
            | NightSessionState::FadingOut => {
                decision(true, &format!("Following Show Night: {}.", night.label()))
            }
            NightSessionState::Inactive | NightSessionState::Stopped => {
                decision(false, &format!("Following Show Night: {}.", night.label()))
            }
            NightSessionState::Unknown => decision(
                false,
                "Show Night state has not been observed. The companion will not guess.",
            ),
        },
    }
}

pub fn expire_override(
    previous: NightSessionState,
    next: NightSessionState,
    value: ArmingOverride,
) -> ArmingOverride {
    match value {
        ArmingOverride::ArmUntilNightEnds
            if matches!(
                next,
                NightSessionState::Inactive | NightSessionState::Stopped
            ) =>
        {
            ArmingOverride::None
        }
        ArmingOverride::DisarmUntilNextNight
            if matches!(next, NightSessionState::Preparing)
                && !matches!(previous, NightSessionState::Preparing) =>
        {
            ArmingOverride::None
        }
        _ => value,
    }
}

fn decision(armed: bool, reason: &str) -> ArmingDecision {
    ArmingDecision {
        armed,
        reason: reason.to_owned(),
    }
}

impl NightSessionState {
    fn label(self) -> &'static str {
        match self {
            Self::Unknown => "state unknown",
            Self::Inactive => "inactive",
            Self::Preparing => "preparing",
            Self::Preshow => "preshow",
            Self::TransitionToShow => "transitioning to show",
            Self::Live => "live",
            Self::TransitionToResting => "transitioning to resting",
            Self::RestingIntershow => "resting between presentations",
            Self::EndOfNightResting => "end-of-night resting",
            Self::FadingOut => "fading out",
            Self::Stopped => "stopped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_show_night_arms_every_active_phase() {
        for state in [
            NightSessionState::Preparing,
            NightSessionState::Preshow,
            NightSessionState::TransitionToShow,
            NightSessionState::Live,
            NightSessionState::TransitionToResting,
            NightSessionState::RestingIntershow,
            NightSessionState::EndOfNightResting,
            NightSessionState::FadingOut,
        ] {
            assert!(
                resolve(ArmingPolicy::FollowShowNight, state, ArmingOverride::None).armed,
                "{state:?}"
            );
        }
    }

    #[test]
    fn unknown_never_arms_by_guess() {
        let result = resolve(
            ArmingPolicy::FollowShowNight,
            NightSessionState::Unknown,
            ArmingOverride::None,
        );
        assert!(!result.armed);
        assert!(result.reason.contains("not been observed"));
    }

    #[test]
    fn explicit_override_wins_over_persistent_policy() {
        assert!(
            !resolve(
                ArmingPolicy::AlwaysArmed,
                NightSessionState::Live,
                ArmingOverride::DisarmUntilNextNight
            )
            .armed
        );
        assert!(
            resolve(
                ArmingPolicy::AlwaysDisarmed,
                NightSessionState::Inactive,
                ArmingOverride::ArmUntilNightEnds
            )
            .armed
        );
    }

    #[test]
    fn temporary_overrides_expire_at_night_boundaries() {
        assert_eq!(
            expire_override(
                NightSessionState::FadingOut,
                NightSessionState::Stopped,
                ArmingOverride::ArmUntilNightEnds
            ),
            ArmingOverride::None
        );
        assert_eq!(
            expire_override(
                NightSessionState::Inactive,
                NightSessionState::Preparing,
                ArmingOverride::DisarmUntilNextNight
            ),
            ArmingOverride::None
        );
    }
}
