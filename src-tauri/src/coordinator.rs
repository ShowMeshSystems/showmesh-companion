use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::arming::NightSessionState;

const NIGHT_SESSION_PATH: &str = "/api/v1/night/session";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkState {
    NotConfigured,
    NeverConnected,
    Connected,
    Unreachable,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorStatus {
    pub state: LinkState,
    pub reason: String,
    pub holding: bool,
}

/// What the companion knows about one coordinator address. A failed poll keeps the
/// last observed night state, so losing the coordinator mid-night does not disarm.
#[derive(Debug, Default)]
pub struct CoordinatorLink {
    base_url: String,
    last_observed: Option<NightSessionState>,
    failing_since_ms: Option<u64>,
    last_error: Option<String>,
}

impl CoordinatorLink {
    /// Forgets everything observed when the address changes, because a held state
    /// from a different coordinator is not evidence about this one.
    pub fn retarget(&mut self, configured_url: &str) -> bool {
        let base_url = normalize_base_url(configured_url);
        if base_url == self.base_url {
            return false;
        }
        *self = Self {
            base_url,
            ..Self::default()
        };
        true
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn record_success(&mut self, state: NightSessionState) {
        self.last_observed = Some(state);
        self.failing_since_ms = None;
        self.last_error = None;
    }

    pub fn record_failure(&mut self, error: String, now_ms: u64) {
        self.failing_since_ms.get_or_insert(now_ms);
        self.last_error = Some(error);
    }

    #[cfg(test)]
    fn night_state(&self) -> NightSessionState {
        self.last_observed.unwrap_or(NightSessionState::Unknown)
    }

    pub fn status(&self, now_ms: u64) -> CoordinatorStatus {
        if self.base_url.is_empty() {
            return CoordinatorStatus {
                state: LinkState::NotConfigured,
                reason:
                    "No coordinator address is set. Enter one in settings to follow Show Night."
                        .into(),
                holding: false,
            };
        }
        let error = self.last_error.as_deref().unwrap_or("no answer yet");
        match (self.last_observed, self.failing_since_ms) {
            (None, None) => CoordinatorStatus {
                state: LinkState::NeverConnected,
                reason: format!("Connecting to the coordinator at {}.", self.base_url),
                holding: false,
            },
            (None, Some(_)) => CoordinatorStatus {
                state: LinkState::NeverConnected,
                reason: format!(
                    "Cannot reach the coordinator at {}: {error}. Check the address and that the coordinator is running.",
                    self.base_url
                ),
                holding: false,
            },
            (Some(_), None) => CoordinatorStatus {
                state: LinkState::Connected,
                reason: format!("Connected to the coordinator at {}.", self.base_url),
                holding: false,
            },
            (Some(held), Some(since_ms)) => CoordinatorStatus {
                state: LinkState::Unreachable,
                reason: format!(
                    "The coordinator has not answered for {}: {error}. Show Night is held at {} until it answers or you set an override.",
                    format_elapsed(now_ms.saturating_sub(since_ms)),
                    held.label()
                ),
                holding: true,
            },
        }
    }
}

pub fn normalize_base_url(configured_url: &str) -> String {
    let trimmed = configured_url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/api/v1")
        .unwrap_or(trimmed)
        .to_string()
}

#[derive(Deserialize)]
struct NightSessionResponse {
    session: NightSessionBody,
}

#[derive(Deserialize)]
struct NightSessionBody {
    state: String,
}

pub fn parse_night_session(body: &str) -> Result<NightSessionState, String> {
    let response: NightSessionResponse = serde_json::from_str(body)
        .map_err(|_| "the coordinator answered with something other than a Show Night state")?;
    let state = response.session.state;
    match serde_json::from_value::<NightSessionState>(serde_json::Value::String(state.clone())) {
        Ok(NightSessionState::Unknown) | Err(_) => Err(format!(
            "the coordinator reported a Show Night state this companion does not know, \"{state}\", so update the companion"
        )),
        Ok(known) => Ok(known),
    }
}

pub fn fetch_night_session(
    agent: &ureq::Agent,
    base_url: &str,
) -> Result<NightSessionState, String> {
    let url = format!("{base_url}{NIGHT_SESSION_PATH}");
    let response = agent.get(&url).call().map_err(describe_error)?;
    let body = response
        .into_string()
        .map_err(|error| format!("the answer could not be read: {error}"))?;
    parse_night_session(&body)
}

pub fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build()
}

fn describe_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, _) => format!("it answered HTTP {code}"),
        ureq::Error::Transport(transport) => match transport.message() {
            Some(message) => message.to_string(),
            None => transport.kind().to_string(),
        },
    }
}

fn format_elapsed(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 3600 {
        format!("{} min", seconds / 60)
    } else {
        format!("{} h {} min", seconds / 3600, (seconds % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected_link() -> CoordinatorLink {
        let mut link = CoordinatorLink::default();
        link.retarget("http://coordinator:8080");
        link
    }

    #[test]
    fn parses_every_state_the_coordinator_publishes() {
        for (wire, expected) in [
            ("inactive", NightSessionState::Inactive),
            ("preparing", NightSessionState::Preparing),
            ("preshow", NightSessionState::Preshow),
            ("transition-to-show", NightSessionState::TransitionToShow),
            ("live", NightSessionState::Live),
            (
                "transition-to-resting",
                NightSessionState::TransitionToResting,
            ),
            ("resting-intershow", NightSessionState::RestingIntershow),
            ("end-of-night-resting", NightSessionState::EndOfNightResting),
            ("fading-out", NightSessionState::FadingOut),
            ("stopped", NightSessionState::Stopped),
        ] {
            let body = format!(
                r#"{{"serverTime":"2026-10-14T18:00:00Z","session":{{"id":"","state":"{wire}","stateLabel":"x"}}}}"#
            );
            assert_eq!(parse_night_session(&body), Ok(expected), "{wire}");
        }
    }

    #[test]
    fn an_unrecognised_state_is_an_error_not_a_guess() {
        let error = parse_night_session(r#"{"session":{"state":"intermission"}}"#).unwrap_err();
        assert!(error.contains("\"intermission\""));
        assert!(parse_night_session(r#"{"session":{"state":"unknown"}}"#).is_err());
        assert!(parse_night_session("<html>proxy error</html>").is_err());
    }

    #[test]
    fn the_api_root_and_trailing_slashes_are_accepted_in_the_address() {
        assert_eq!(normalize_base_url(" http://h:8080/ "), "http://h:8080");
        assert_eq!(normalize_base_url("http://h:8080/api/v1/"), "http://h:8080");
        assert_eq!(normalize_base_url(""), "");
    }

    #[test]
    fn a_lost_coordinator_holds_the_last_observed_state() {
        let mut link = connected_link();
        link.record_success(NightSessionState::Live);

        link.record_failure("connection refused".into(), 1_000);
        link.record_failure("connection refused".into(), 200_000);

        assert_eq!(link.night_state(), NightSessionState::Live);
        let status = link.status(200_000);
        assert_eq!(status.state, LinkState::Unreachable);
        assert!(status.holding);
        assert!(status.reason.contains("3 min"), "{}", status.reason);
        assert!(status.reason.contains("held at live"), "{}", status.reason);
    }

    #[test]
    fn a_coordinator_never_reached_leaves_the_night_unknown() {
        let mut link = connected_link();
        link.record_failure("connection refused".into(), 1_000);

        assert_eq!(link.night_state(), NightSessionState::Unknown);
        let status = link.status(5_000);
        assert_eq!(status.state, LinkState::NeverConnected);
        assert!(!status.holding);
        assert!(status.reason.contains("connection refused"));
    }

    #[test]
    fn an_answer_after_an_outage_ends_the_hold() {
        let mut link = connected_link();
        link.record_success(NightSessionState::Live);
        link.record_failure("timed out".into(), 1_000);

        link.record_success(NightSessionState::FadingOut);

        assert_eq!(link.night_state(), NightSessionState::FadingOut);
        assert_eq!(link.status(9_000).state, LinkState::Connected);
    }

    #[test]
    fn changing_the_address_forgets_the_held_state() {
        let mut link = connected_link();
        link.record_success(NightSessionState::Live);

        assert!(!link.retarget("http://coordinator:8080/"));
        assert_eq!(link.night_state(), NightSessionState::Live);

        assert!(link.retarget("http://other:8080"));
        assert_eq!(link.night_state(), NightSessionState::Unknown);
    }

    /// Run against a live coordinator with
    /// `SHOWMESH_TEST_COORDINATOR_URL=http://host:port cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn reads_the_night_state_from_a_running_coordinator() {
        let url = std::env::var("SHOWMESH_TEST_COORDINATOR_URL").expect("set the coordinator URL");
        let state = fetch_night_session(&http_agent(), &normalize_base_url(&url)).unwrap();
        assert_ne!(state, NightSessionState::Unknown);
    }

    #[test]
    fn an_empty_address_is_not_configured() {
        let link = CoordinatorLink::default();
        assert_eq!(link.status(0).state, LinkState::NotConfigured);
        assert_eq!(link.night_state(), NightSessionState::Unknown);
    }
}
