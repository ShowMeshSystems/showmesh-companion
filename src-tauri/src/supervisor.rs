use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::settings::ResolumeSettings;

const GRACE_WINDOW: Duration = Duration::from_secs(20);
const STABILITY_WINDOW: Duration = Duration::from_secs(60);
const MIN_SPAWN_INTERVAL: Duration = Duration::from_secs(10);
const INITIAL_BACKOFF_SECS: u64 = 2;
const MAX_BACKOFF_SECS: u64 = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupervisorState {
    Disabled,
    NotConfigured,
    PathMissing,
    Present,
    AbsentDisarmed,
    AbsentArmed,
    Launching,
    LaunchFailed,
    Backoff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessMatch {
    NotMatched,
    MatchedByName,
    MatchedByPath,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorStatus {
    pub state: SupervisorState,
    pub reason: String,
    pub operational: bool,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub matched_by: ProcessMatch,
}

impl SupervisorStatus {
    pub fn disabled() -> Self {
        Self {
            state: SupervisorState::Disabled,
            reason: "Resolume supervision is disabled in settings.".into(),
            operational: false,
            consecutive_failures: 0,
            last_error: None,
            matched_by: ProcessMatch::NotMatched,
        }
    }

    pub fn not_configured() -> Self {
        Self {
            state: SupervisorState::NotConfigured,
            reason: "Choose the Resolume Arena application before supervision can start.".into(),
            operational: false,
            consecutive_failures: 0,
            last_error: None,
            matched_by: ProcessMatch::NotMatched,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Phase {
    Idle,
    Launching { since: Instant },
    Running { since: Instant },
    Backoff { until: Instant },
}

#[derive(Clone, Debug)]
pub struct SupervisionState {
    consecutive_failures: u32,
    last_error: Option<String>,
    last_spawn_at: Option<Instant>,
    phase: Phase,
}

impl Default for SupervisionState {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            last_error: None,
            last_spawn_at: None,
            phase: Phase::Idle,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupervisionInput {
    pub enabled: bool,
    pub path_configured: bool,
    pub path_exists: bool,
    pub armed: bool,
    pub matched: ProcessMatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisionAction {
    DoNothing,
    Spawn,
    AwaitGraceWindow,
    AwaitBackoff,
    AwaitSpawnInterval,
}

fn next_backoff(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(10);
    let secs = INITIAL_BACKOFF_SECS.saturating_mul(1u64 << exponent);
    Duration::from_secs(secs.min(MAX_BACKOFF_SECS))
}

fn status_present(matched: ProcessMatch, state: &SupervisionState) -> SupervisorStatus {
    let (reason, operational) = match matched {
        ProcessMatch::MatchedByPath => (
            "A process running at the configured application path was observed.".to_string(),
            true,
        ),
        ProcessMatch::MatchedByName => (
            "No process matched the configured application path, but a process with the same name was observed. No launch will be attempted while that process exists.".to_string(),
            false,
        ),
        ProcessMatch::NotMatched => unreachable!("status_present requires a match"),
    };
    SupervisorStatus {
        state: SupervisorState::Present,
        reason,
        operational,
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        matched_by: matched,
    }
}

fn status_awaiting_spawn_interval(state: &SupervisionState) -> SupervisorStatus {
    SupervisorStatus {
        state: SupervisorState::Backoff,
        reason: "A launch was attempted very recently. Waiting before attempting another."
            .to_string(),
        operational: false,
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        matched_by: ProcessMatch::NotMatched,
    }
}

fn status_absent_disarmed(state: &SupervisionState) -> SupervisorStatus {
    SupervisorStatus {
        state: SupervisorState::AbsentDisarmed,
        reason: "No matching process was found. Supervision is disarmed, so no launch will be attempted.".into(),
        operational: false,
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        matched_by: ProcessMatch::NotMatched,
    }
}

fn status_absent_armed(state: &SupervisionState) -> SupervisorStatus {
    SupervisorStatus {
        state: SupervisorState::AbsentArmed,
        reason: "No matching process was found. Supervision is armed, so a launch is due.".into(),
        operational: false,
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        matched_by: ProcessMatch::NotMatched,
    }
}

fn status_launching(state: &SupervisionState) -> SupervisorStatus {
    SupervisorStatus {
        state: SupervisorState::Launching,
        reason: "A launch was started. Waiting for a matching process to appear.".into(),
        operational: false,
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        matched_by: ProcessMatch::NotMatched,
    }
}

fn status_launch_failed(state: &SupervisionState) -> SupervisorStatus {
    let reason = match &state.last_error {
        Some(error) => format!("Launching Resolume Arena failed: {error}"),
        None => "No matching process appeared within the grace window after launch.".to_string(),
    };
    SupervisorStatus {
        state: SupervisorState::LaunchFailed,
        reason,
        operational: false,
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        matched_by: ProcessMatch::NotMatched,
    }
}

fn status_backoff(state: &SupervisionState) -> SupervisorStatus {
    SupervisorStatus {
        state: SupervisorState::Backoff,
        reason: format!(
            "Waiting before the next launch attempt after {} consecutive failure(s).",
            state.consecutive_failures
        ),
        operational: false,
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        matched_by: ProcessMatch::NotMatched,
    }
}

fn decide_from_idle(
    input: &SupervisionInput,
    state: &SupervisionState,
    now: Instant,
) -> (SupervisionAction, SupervisionState, SupervisorStatus) {
    let next_state = SupervisionState {
        consecutive_failures: state.consecutive_failures,
        last_error: state.last_error.clone(),
        last_spawn_at: state.last_spawn_at,
        phase: Phase::Idle,
    };
    if !input.armed {
        let status = status_absent_disarmed(&next_state);
        return (SupervisionAction::DoNothing, next_state, status);
    }
    if let Some(last_spawn_at) = state.last_spawn_at
        && now.saturating_duration_since(last_spawn_at) < MIN_SPAWN_INTERVAL
    {
        let status = status_awaiting_spawn_interval(&next_state);
        return (SupervisionAction::AwaitSpawnInterval, next_state, status);
    }
    let status = status_absent_armed(&next_state);
    (SupervisionAction::Spawn, next_state, status)
}

pub fn decide(
    input: &SupervisionInput,
    state: &SupervisionState,
    now: Instant,
) -> (SupervisionAction, SupervisionState, SupervisorStatus) {
    if !input.enabled {
        return (
            SupervisionAction::DoNothing,
            SupervisionState::default(),
            SupervisorStatus::disabled(),
        );
    }
    if !input.path_configured {
        return (
            SupervisionAction::DoNothing,
            SupervisionState::default(),
            SupervisorStatus::not_configured(),
        );
    }
    if !input.path_exists {
        return (
            SupervisionAction::DoNothing,
            SupervisionState::default(),
            SupervisorStatus {
                state: SupervisorState::PathMissing,
                reason: "The configured Resolume Arena application path does not exist on disk."
                    .into(),
                operational: false,
                consecutive_failures: 0,
                last_error: None,
                matched_by: ProcessMatch::NotMatched,
            },
        );
    }

    if input.matched != ProcessMatch::NotMatched {
        let running_since = match state.phase {
            Phase::Running { since } => since,
            _ => now,
        };
        let stable = now.saturating_duration_since(running_since) >= STABILITY_WINDOW;
        let next_state = SupervisionState {
            consecutive_failures: if stable {
                0
            } else {
                state.consecutive_failures
            },
            last_error: if stable {
                None
            } else {
                state.last_error.clone()
            },
            last_spawn_at: state.last_spawn_at,
            phase: Phase::Running {
                since: running_since,
            },
        };
        let status = status_present(input.matched, &next_state);
        return (SupervisionAction::DoNothing, next_state, status);
    }

    match state.phase {
        Phase::Launching { since } => {
            let elapsed = now.saturating_duration_since(since);
            if elapsed < GRACE_WINDOW {
                let next_state = state.clone();
                let status = status_launching(&next_state);
                (SupervisionAction::AwaitGraceWindow, next_state, status)
            } else {
                let next_state = failed_attempt(state, now, None);
                let status = status_launch_failed(&next_state);
                (SupervisionAction::AwaitBackoff, next_state, status)
            }
        }
        Phase::Running { since } => {
            if now.saturating_duration_since(since) < STABILITY_WINDOW {
                let next_state = failed_attempt(state, now, None);
                let status = status_launch_failed(&next_state);
                (SupervisionAction::AwaitBackoff, next_state, status)
            } else {
                let recovered = SupervisionState {
                    consecutive_failures: 0,
                    last_error: None,
                    last_spawn_at: state.last_spawn_at,
                    phase: Phase::Idle,
                };
                decide_from_idle(input, &recovered, now)
            }
        }
        Phase::Backoff { until } => {
            if now < until {
                let next_state = state.clone();
                let status = status_backoff(&next_state);
                (SupervisionAction::AwaitBackoff, next_state, status)
            } else {
                let idle_state = SupervisionState {
                    consecutive_failures: state.consecutive_failures,
                    last_error: state.last_error.clone(),
                    last_spawn_at: state.last_spawn_at,
                    phase: Phase::Idle,
                };
                decide_from_idle(input, &idle_state, now)
            }
        }
        Phase::Idle => decide_from_idle(input, state, now),
    }
}

fn failed_attempt(
    state: &SupervisionState,
    now: Instant,
    error: Option<String>,
) -> SupervisionState {
    let failures = state.consecutive_failures + 1;
    SupervisionState {
        consecutive_failures: failures,
        last_error: error,
        last_spawn_at: state.last_spawn_at,
        phase: Phase::Backoff {
            until: now + next_backoff(failures),
        },
    }
}

pub fn record_spawn_result(
    state: &SupervisionState,
    now: Instant,
    result: Result<(), String>,
) -> SupervisionState {
    match result {
        Ok(()) => SupervisionState {
            consecutive_failures: state.consecutive_failures,
            last_error: None,
            last_spawn_at: Some(now),
            phase: Phase::Launching { since: now },
        },
        Err(error) => {
            let attempted = SupervisionState {
                last_spawn_at: Some(now),
                ..state.clone()
            };
            failed_attempt(&attempted, now, Some(error))
        }
    }
}

pub fn status_for(state: &SupervisionState) -> SupervisorStatus {
    match state.phase {
        Phase::Launching { .. } => status_launching(state),
        Phase::Backoff { .. } => {
            if state.last_error.is_some() && state.consecutive_failures > 0 {
                status_launch_failed(state)
            } else {
                status_backoff(state)
            }
        }
        Phase::Running { .. } => status_absent_armed(state),
        Phase::Idle => status_absent_armed(state),
    }
}

fn normalize_path_string(path: &Path) -> String {
    let mut value = path.to_string_lossy().into_owned();
    while value.ends_with('/') || value.ends_with('\\') {
        value.pop();
    }
    value
}

fn is_bundle_path(path: &Path) -> bool {
    path.extension()
        .map(|extension| extension.eq_ignore_ascii_case("app"))
        .unwrap_or(false)
}

pub fn match_process_path(configured: &Path, exe: &Path, case_insensitive: bool) -> bool {
    let configured_norm = normalize_path_string(configured);
    let exe_norm = normalize_path_string(exe);
    let (configured_cmp, exe_cmp) = if case_insensitive {
        (configured_norm.to_lowercase(), exe_norm.to_lowercase())
    } else {
        (configured_norm, exe_norm)
    };

    if is_bundle_path(configured) {
        if exe_cmp.len() == configured_cmp.len() {
            return exe_cmp == configured_cmp;
        }
        if exe_cmp.len() > configured_cmp.len() && exe_cmp.starts_with(&configured_cmp) {
            let boundary = exe_cmp.as_bytes()[configured_cmp.len()];
            return boundary == b'/' || boundary == b'\\';
        }
        false
    } else {
        exe_cmp == configured_cmp
    }
}

fn stem_for_compare(path: &Path, case_insensitive: bool) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    if case_insensitive {
        stem.to_lowercase()
    } else {
        stem
    }
}

pub fn match_process(
    configured: &Path,
    exe: Option<&Path>,
    process_name: &str,
    case_insensitive: bool,
) -> ProcessMatch {
    if let Some(exe_path) = exe
        && match_process_path(configured, exe_path, case_insensitive)
    {
        return ProcessMatch::MatchedByPath;
    }

    let configured_stem = stem_for_compare(configured, case_insensitive);
    let name_cmp = if case_insensitive {
        process_name.to_lowercase()
    } else {
        process_name.to_string()
    };
    let exe_stem_cmp = exe
        .and_then(|path| path.file_stem())
        .map(|stem| stem.to_string_lossy().into_owned())
        .map(|stem| {
            if case_insensitive {
                stem.to_lowercase()
            } else {
                stem
            }
        });

    if !configured_stem.is_empty()
        && (name_cmp == configured_stem
            || exe_stem_cmp.as_deref() == Some(configured_stem.as_str()))
    {
        ProcessMatch::MatchedByName
    } else {
        ProcessMatch::NotMatched
    }
}

fn observe(system: &mut System, configured_path: &Path) -> ProcessMatch {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let mut best = ProcessMatch::NotMatched;
    for process in system.processes().values() {
        let exe = process.exe();
        let name = process.name().to_string_lossy();
        match match_process(configured_path, exe, &name, true) {
            ProcessMatch::MatchedByPath => return ProcessMatch::MatchedByPath,
            ProcessMatch::MatchedByName => best = ProcessMatch::MatchedByName,
            ProcessMatch::NotMatched => {}
        }
    }
    best
}

fn spawn_process(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        match Command::new("open").arg("-a").arg(path).status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("open exited with {status}.")),
            Err(error) => Err(error.to_string()),
        }
    }
    #[cfg(target_os = "windows")]
    {
        Command::new(path)
            .spawn()
            .map(|_child| ())
            .map_err(|error| error.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        Err("Process supervision launching is only implemented for macOS and Windows.".into())
    }
}

pub fn tick(
    settings: &ResolumeSettings,
    armed: bool,
    system: &mut System,
    state: &SupervisionState,
    now: Instant,
) -> (SupervisorStatus, SupervisionState) {
    if !settings.enabled {
        return (SupervisorStatus::disabled(), SupervisionState::default());
    }
    let configured = settings.application_path.trim();
    if configured.is_empty() {
        return (
            SupervisorStatus::not_configured(),
            SupervisionState::default(),
        );
    }
    let configured_path = Path::new(configured);
    if !configured_path.exists() {
        return (
            SupervisorStatus {
                state: SupervisorState::PathMissing,
                reason: "The configured Resolume Arena application path does not exist on disk."
                    .into(),
                operational: false,
                consecutive_failures: 0,
                last_error: None,
                matched_by: ProcessMatch::NotMatched,
            },
            SupervisionState {
                phase: Phase::Idle,
                ..state.clone()
            },
        );
    }

    let matched = observe(system, configured_path);
    let input = SupervisionInput {
        enabled: settings.enabled,
        path_configured: true,
        path_exists: true,
        armed,
        matched,
    };

    let (action, next_state, status) = decide(&input, state, now);
    if action == SupervisionAction::Spawn {
        let result = spawn_process(configured_path);
        let spawned_state = record_spawn_result(&next_state, now, result);
        let spawned_status = status_for(&spawned_state);
        (spawned_status, spawned_state)
    } else {
        (status, next_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(matched: ProcessMatch, armed: bool) -> SupervisionInput {
        SupervisionInput {
            enabled: true,
            path_configured: true,
            path_exists: true,
            armed,
            matched,
        }
    }

    #[test]
    fn disarmed_and_absent_produces_no_spawn() {
        let now = Instant::now();
        let state = SupervisionState::default();
        let (action, _, status) = decide(&input(ProcessMatch::NotMatched, false), &state, now);
        assert_eq!(action, SupervisionAction::DoNothing);
        assert_eq!(status.state, SupervisorState::AbsentDisarmed);
    }

    #[test]
    fn armed_and_absent_produces_a_spawn() {
        let now = Instant::now();
        let state = SupervisionState::default();
        let (action, _, status) = decide(&input(ProcessMatch::NotMatched, true), &state, now);
        assert_eq!(action, SupervisionAction::Spawn);
        assert_eq!(status.state, SupervisorState::AbsentArmed);
    }

    #[test]
    fn armed_and_present_produces_no_spawn() {
        let now = Instant::now();
        let state = SupervisionState::default();
        let (action, _, status) = decide(&input(ProcessMatch::MatchedByPath, true), &state, now);
        assert_eq!(action, SupervisionAction::DoNothing);
        assert_eq!(status.state, SupervisorState::Present);
    }

    #[test]
    fn a_spawn_is_not_repeated_while_inside_the_grace_window() {
        let t0 = Instant::now();
        let launching = SupervisionState {
            consecutive_failures: 0,
            last_error: None,
            last_spawn_at: None,
            phase: Phase::Launching { since: t0 },
        };
        let t1 = t0 + Duration::from_secs(5);
        let (action, next_state, status) =
            decide(&input(ProcessMatch::NotMatched, true), &launching, t1);
        assert_eq!(action, SupervisionAction::AwaitGraceWindow);
        assert_eq!(status.state, SupervisorState::Launching);
        assert!(matches!(next_state.phase, Phase::Launching { since } if since == t0));
    }

    #[test]
    fn grace_window_expiring_with_no_process_counts_one_failure_and_enters_backoff() {
        let t0 = Instant::now();
        let launching = SupervisionState {
            consecutive_failures: 0,
            last_error: None,
            last_spawn_at: None,
            phase: Phase::Launching { since: t0 },
        };
        let t1 = t0 + GRACE_WINDOW + Duration::from_secs(1);
        let (action, next_state, status) =
            decide(&input(ProcessMatch::NotMatched, true), &launching, t1);
        assert_eq!(action, SupervisionAction::AwaitBackoff);
        assert_eq!(status.state, SupervisorState::LaunchFailed);
        assert_eq!(next_state.consecutive_failures, 1);
        assert!(matches!(next_state.phase, Phase::Backoff { .. }));
    }

    #[test]
    fn backoff_doubles_across_consecutive_failures_and_clamps_at_sixty_seconds() {
        assert_eq!(next_backoff(1), Duration::from_secs(2));
        assert_eq!(next_backoff(2), Duration::from_secs(4));
        assert_eq!(next_backoff(3), Duration::from_secs(8));
        assert_eq!(next_backoff(4), Duration::from_secs(16));
        assert_eq!(next_backoff(5), Duration::from_secs(32));
        assert_eq!(next_backoff(6), Duration::from_secs(60));
        assert_eq!(next_backoff(20), Duration::from_secs(60));
    }

    #[test]
    fn observing_the_process_starts_a_running_phase_without_clearing_failures() {
        let t0 = Instant::now();
        let backing_off = SupervisionState {
            consecutive_failures: 4,
            last_error: Some("previous failure".into()),
            last_spawn_at: None,
            phase: Phase::Backoff {
                until: t0 + Duration::from_secs(16),
            },
        };
        let (action, next_state, status) =
            decide(&input(ProcessMatch::MatchedByPath, true), &backing_off, t0);
        assert_eq!(action, SupervisionAction::DoNothing);
        assert_eq!(status.state, SupervisorState::Present);
        assert_eq!(next_state.consecutive_failures, 4);
        assert_eq!(next_state.phase, Phase::Running { since: t0 });
    }

    #[test]
    fn a_process_that_stays_up_past_the_stability_window_clears_failures() {
        let t0 = Instant::now();
        let running = SupervisionState {
            consecutive_failures: 4,
            last_error: Some("previous failure".into()),
            last_spawn_at: Some(t0),
            phase: Phase::Running { since: t0 },
        };
        let (_, next_state, status) = decide(
            &input(ProcessMatch::MatchedByPath, true),
            &running,
            t0 + STABILITY_WINDOW,
        );
        assert_eq!(next_state.consecutive_failures, 0);
        assert!(next_state.last_error.is_none());
        assert!(status.operational);
    }

    #[test]
    fn a_process_that_vanishes_before_the_stability_window_counts_as_a_failure() {
        let t0 = Instant::now();
        let running = SupervisionState {
            consecutive_failures: 0,
            last_error: None,
            last_spawn_at: Some(t0),
            phase: Phase::Running { since: t0 },
        };
        let (action, next_state, status) = decide(
            &input(ProcessMatch::NotMatched, true),
            &running,
            t0 + Duration::from_secs(6),
        );
        assert_eq!(action, SupervisionAction::AwaitBackoff);
        assert_eq!(next_state.consecutive_failures, 1);
        assert_eq!(status.state, SupervisorState::LaunchFailed);
        assert!(matches!(next_state.phase, Phase::Backoff { .. }));
    }

    #[test]
    fn a_crash_loop_escalates_backoff_instead_of_relaunching_every_poll() {
        let t0 = Instant::now();
        let mut state = SupervisionState::default();
        let mut now = t0;
        let mut spawns = 0;
        let mut seen_backoffs = Vec::new();

        for step in 0..150 {
            let appears = match state.phase {
                Phase::Running { .. } => step % 4 != 0,
                Phase::Launching { .. } => true,
                _ => false,
            };
            let matched = if appears {
                ProcessMatch::MatchedByPath
            } else {
                ProcessMatch::NotMatched
            };
            let (action, next_state, _) = decide(&input(matched, true), &state, now);
            state = next_state;
            if action == SupervisionAction::Spawn {
                spawns += 1;
                state = record_spawn_result(&state, now, Ok(()));
            }
            if let Phase::Backoff { until } = state.phase {
                seen_backoffs.push(until.saturating_duration_since(now));
            }
            now += Duration::from_secs(2);
        }

        assert!(
            spawns <= 12,
            "a crash loop must not relaunch on every poll, saw {spawns} spawns"
        );
        assert!(
            seen_backoffs
                .iter()
                .any(|wait| *wait >= Duration::from_secs(32)),
            "backoff must escalate across repeated failures"
        );
    }

    #[test]
    fn a_recent_spawn_blocks_another_launch_even_from_idle() {
        let t0 = Instant::now();
        let just_spawned = SupervisionState {
            consecutive_failures: 0,
            last_error: None,
            last_spawn_at: Some(t0),
            phase: Phase::Idle,
        };
        let (action, _, _) = decide(
            &input(ProcessMatch::NotMatched, true),
            &just_spawned,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(action, SupervisionAction::AwaitSpawnInterval);

        let (action, _, _) = decide(
            &input(ProcessMatch::NotMatched, true),
            &just_spawned,
            t0 + MIN_SPAWN_INTERVAL,
        );
        assert_eq!(action, SupervisionAction::Spawn);
    }

    #[test]
    fn a_name_only_match_is_not_reported_as_operational() {
        let t0 = Instant::now();
        let (action, _, status) = decide(
            &input(ProcessMatch::MatchedByName, true),
            &SupervisionState::default(),
            t0,
        );
        assert_eq!(action, SupervisionAction::DoNothing);
        assert_eq!(status.state, SupervisorState::Present);
        assert_eq!(status.matched_by, ProcessMatch::MatchedByName);
        assert!(!status.operational);
    }

    #[test]
    fn recording_a_spawn_result_tracks_the_attempt_time() {
        let t0 = Instant::now();
        let launched = record_spawn_result(&SupervisionState::default(), t0, Ok(()));
        assert_eq!(launched.last_spawn_at, Some(t0));
        assert_eq!(launched.consecutive_failures, 0);
        assert!(launched.last_error.is_none());
        assert_eq!(launched.phase, Phase::Launching { since: t0 });

        let failed = record_spawn_result(
            &SupervisionState::default(),
            t0,
            Err("open exited with 1.".into()),
        );
        assert_eq!(failed.last_spawn_at, Some(t0));
        assert_eq!(failed.consecutive_failures, 1);
        assert_eq!(failed.last_error.as_deref(), Some("open exited with 1."));
        assert!(matches!(failed.phase, Phase::Backoff { .. }));
    }

    #[test]
    fn a_transient_path_miss_does_not_discard_launch_history() {
        let t0 = Instant::now();
        let mut system = System::new();
        let launching = SupervisionState {
            consecutive_failures: 3,
            last_error: None,
            last_spawn_at: Some(t0),
            phase: Phase::Launching { since: t0 },
        };
        let settings = ResolumeSettings {
            enabled: true,
            application_path: "/nonexistent/Arena.app".into(),
        };
        let (status, next_state) = tick(&settings, true, &mut system, &launching, t0);
        assert_eq!(status.state, SupervisorState::PathMissing);
        assert_eq!(next_state.consecutive_failures, 3);
        assert_eq!(next_state.last_spawn_at, Some(t0));
    }

    #[test]
    fn a_missing_application_path_never_spawns() {
        let now = Instant::now();
        let mut missing_path_input = input(ProcessMatch::NotMatched, true);
        missing_path_input.path_exists = false;
        let state = SupervisionState::default();
        let (action, _, status) = decide(&missing_path_input, &state, now);
        assert_eq!(action, SupervisionAction::DoNothing);
        assert_eq!(status.state, SupervisorState::PathMissing);
    }

    #[test]
    fn supervision_disabled_never_spawns_even_when_armed() {
        let now = Instant::now();
        let mut disabled_input = input(ProcessMatch::NotMatched, true);
        disabled_input.enabled = false;
        let state = SupervisionState::default();
        let (action, _, status) = decide(&disabled_input, &state, now);
        assert_eq!(action, SupervisionAction::DoNothing);
        assert_eq!(status.state, SupervisorState::Disabled);
    }

    #[test]
    fn backoff_waits_before_the_next_attempt() {
        let t0 = Instant::now();
        let backing_off = SupervisionState {
            consecutive_failures: 2,
            last_error: None,
            last_spawn_at: None,
            phase: Phase::Backoff {
                until: t0 + Duration::from_secs(4),
            },
        };
        let (action, next_state, status) = decide(
            &input(ProcessMatch::NotMatched, true),
            &backing_off,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(action, SupervisionAction::AwaitBackoff);
        assert_eq!(status.state, SupervisorState::Backoff);
        assert_eq!(next_state.consecutive_failures, 2);
    }

    #[test]
    fn backoff_expiring_while_disarmed_does_not_spawn() {
        let t0 = Instant::now();
        let backing_off = SupervisionState {
            consecutive_failures: 3,
            last_error: Some("boom".into()),
            last_spawn_at: None,
            phase: Phase::Backoff {
                until: t0 + Duration::from_secs(4),
            },
        };
        let (action, next_state, status) = decide(
            &input(ProcessMatch::NotMatched, false),
            &backing_off,
            t0 + Duration::from_secs(5),
        );
        assert_eq!(action, SupervisionAction::DoNothing);
        assert_eq!(status.state, SupervisorState::AbsentDisarmed);
        assert_eq!(next_state.phase, Phase::Idle);
        assert_eq!(next_state.consecutive_failures, 3);
    }

    #[test]
    fn bundle_path_matches_the_macos_executable_inside_it() {
        let configured = Path::new("/Applications/Resolume Arena/Arena.app");
        let exe = Path::new("/Applications/Resolume Arena/Arena.app/Contents/MacOS/Arena");
        assert!(match_process_path(configured, exe, false));
    }

    #[test]
    fn bundle_path_does_not_match_a_similarly_named_directory() {
        let configured = Path::new("/Applications/Resolume Arena/Arena.app");
        let exe = Path::new("/Applications/Resolume Arena/Arena.app-imposter/Contents/MacOS/Arena");
        assert!(!match_process_path(configured, exe, false));
    }

    #[test]
    fn windows_path_match_is_exact_and_case_insensitive() {
        let configured = Path::new(r"C:\Program Files\Resolume Arena\Arena.exe");
        let exe = Path::new(r"C:\PROGRAM FILES\Resolume Arena\ARENA.EXE");
        assert!(match_process_path(configured, exe, true));
        assert!(!match_process_path(configured, exe, false));
    }

    #[test]
    fn trailing_separators_are_normalized() {
        let configured = Path::new("/Applications/Resolume Arena/Arena.app/");
        let exe = Path::new("/Applications/Resolume Arena/Arena.app/Contents/MacOS/Arena");
        assert!(match_process_path(configured, exe, false));
    }

    #[test]
    fn a_process_with_only_a_matching_name_is_labelled_matched_by_name() {
        let configured = Path::new("/Applications/Resolume Arena/Arena.app");
        let exe = Path::new("/opt/other/place/Arena");
        assert_eq!(
            match_process(configured, Some(exe), "Arena", false),
            ProcessMatch::MatchedByName
        );
    }

    #[test]
    fn a_process_matching_neither_path_nor_name_is_not_matched() {
        let configured = Path::new("/Applications/Resolume Arena/Arena.app");
        let exe = Path::new("/usr/bin/unrelated");
        assert_eq!(
            match_process(configured, Some(exe), "unrelated", false),
            ProcessMatch::NotMatched
        );
    }
}
