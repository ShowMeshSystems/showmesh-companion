mod arming;
mod capabilities;
mod settings;
mod state;
mod supervisor;
mod timecode;

use std::sync::Mutex;

use arming::{ArmingOverride, NightSessionState};
use serde::Serialize;
use settings::{AppSettings, SettingsStore};
use state::StateStore;
use supervisor::SupervisorStatus;
use tauri::{Manager, State};
use timecode::TimecodeStatus;

struct AppState {
    settings: Mutex<AppSettings>,
    settings_store: SettingsStore,
    state_store: StateStore,
    arming_override: Mutex<ArmingOverride>,
    night_state: Mutex<NightSessionState>,
    supervisor: Mutex<SupervisorStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSnapshot {
    settings: AppSettings,
    capabilities: Vec<capabilities::CapabilityCandidate>,
    night_state: NightSessionState,
    arming_override: ArmingOverride,
    effective_armed: bool,
    arming_reason: String,
    supervisor: SupervisorStatus,
    timecode: TimecodeStatus,
}

impl AppState {
    fn snapshot(&self) -> Result<AppSnapshot, String> {
        let settings = self
            .settings
            .lock()
            .map_err(|_| "settings lock is poisoned")?
            .clone();
        let night_state = *self
            .night_state
            .lock()
            .map_err(|_| "night-state lock is poisoned")?;
        let arming_override = *self
            .arming_override
            .lock()
            .map_err(|_| "arming-override lock is poisoned")?;
        let arming = arming::resolve(settings.arming_policy, night_state, arming_override);
        let capabilities = capabilities::derive(&settings);
        let supervisor = self
            .supervisor
            .lock()
            .map_err(|_| "supervisor lock is poisoned")?
            .clone();
        let timecode = TimecodeStatus::from_settings(&settings.timecode);

        Ok(AppSnapshot {
            settings,
            capabilities,
            night_state,
            arming_override,
            effective_armed: arming.armed,
            arming_reason: arming.reason,
            supervisor,
            timecode,
        })
    }

    #[allow(dead_code)]
    fn apply_night_state(&self, next: NightSessionState) -> Result<(), String> {
        let mut night_state = self
            .night_state
            .lock()
            .map_err(|_| "night-state lock is poisoned")?;
        let mut arming_override = self
            .arming_override
            .lock()
            .map_err(|_| "arming-override lock is poisoned")?;
        let previous = *night_state;
        let resolved_override = arming::expire_override(previous, next, *arming_override);
        let override_changed = resolved_override != *arming_override;
        *night_state = next;
        *arming_override = resolved_override;
        if override_changed {
            self.state_store.save(resolved_override)?;
        }
        Ok(())
    }
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    state.snapshot()
}

#[tauri::command]
fn save_settings(settings: AppSettings, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    settings.validate()?;
    state.settings_store.save(&settings)?;
    *state
        .settings
        .lock()
        .map_err(|_| "settings lock is poisoned")? = settings;
    state.snapshot()
}

#[tauri::command]
fn set_arming_override(
    value: ArmingOverride,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    *state
        .arming_override
        .lock()
        .map_err(|_| "arming-override lock is poisoned")? = value;
    state.state_store.save(value)?;
    state.snapshot()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let config_dir = app
                .path()
                .app_config_dir()
                .map_err(|error| error.to_string())?;
            let settings_store = SettingsStore::new(config_dir.clone());
            let settings = settings_store
                .load()
                .map_err(|error| format!("could not load settings: {error}"))?;
            let state_store = StateStore::new(config_dir);
            let arming_override = state_store.load();
            app.manage(AppState {
                settings: Mutex::new(settings),
                settings_store,
                state_store,
                arming_override: Mutex::new(arming_override),
                night_state: Mutex::new(NightSessionState::Unknown),
                supervisor: Mutex::new(SupervisorStatus::foundation()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_settings,
            set_arming_override
        ])
        .run(tauri::generate_context!())
        .expect("error while running showmesh-companion");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_state() -> AppState {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "showmesh-companion-lib-test-{}-{id}",
            std::process::id()
        ));
        AppState {
            settings: Mutex::new(AppSettings::default()),
            settings_store: SettingsStore::new(dir.clone()),
            state_store: StateStore::new(dir),
            arming_override: Mutex::new(ArmingOverride::None),
            night_state: Mutex::new(NightSessionState::Unknown),
            supervisor: Mutex::new(SupervisorStatus::foundation()),
        }
    }

    #[test]
    fn apply_night_state_expires_override_across_night_boundary() {
        let state = temp_state();
        *state.night_state.lock().unwrap() = NightSessionState::FadingOut;
        *state.arming_override.lock().unwrap() = ArmingOverride::ArmUntilNightEnds;

        state.apply_night_state(NightSessionState::Stopped).unwrap();

        assert_eq!(
            *state.night_state.lock().unwrap(),
            NightSessionState::Stopped
        );
        assert_eq!(*state.arming_override.lock().unwrap(), ArmingOverride::None);
        assert_eq!(state.state_store.load(), ArmingOverride::None);
    }

    #[test]
    fn apply_night_state_keeps_override_across_a_non_boundary_transition() {
        let state = temp_state();
        *state.night_state.lock().unwrap() = NightSessionState::Preshow;
        *state.arming_override.lock().unwrap() = ArmingOverride::ArmUntilNightEnds;

        state.apply_night_state(NightSessionState::Live).unwrap();

        assert_eq!(
            *state.arming_override.lock().unwrap(),
            ArmingOverride::ArmUntilNightEnds
        );
    }

    #[test]
    fn persisted_override_round_trips_through_the_store() {
        let state = temp_state();
        *state.arming_override.lock().unwrap() = ArmingOverride::DisarmUntilNextNight;
        state
            .state_store
            .save(ArmingOverride::DisarmUntilNextNight)
            .unwrap();

        assert_eq!(
            state.state_store.load(),
            ArmingOverride::DisarmUntilNextNight
        );
    }
}
