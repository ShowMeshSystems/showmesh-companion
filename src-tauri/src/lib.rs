mod arming;
mod capabilities;
mod settings;
mod supervisor;

use std::sync::Mutex;

use arming::{ArmingOverride, NightSessionState};
use serde::Serialize;
use settings::{AppSettings, SettingsStore};
use supervisor::SupervisorStatus;
use tauri::{Manager, State};

struct AppState {
    settings: Mutex<AppSettings>,
    settings_store: SettingsStore,
    arming_override: Mutex<ArmingOverride>,
    night_state: Mutex<NightSessionState>,
    supervisor: Mutex<SupervisorStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSnapshot {
    settings: AppSettings,
    capabilities: Vec<capabilities::AdvertisedCapability>,
    night_state: NightSessionState,
    arming_override: ArmingOverride,
    effective_armed: bool,
    arming_reason: String,
    supervisor: SupervisorStatus,
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

        Ok(AppSnapshot {
            settings,
            capabilities,
            night_state,
            arming_override,
            effective_armed: arming.armed,
            arming_reason: arming.reason,
            supervisor,
        })
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
    state.snapshot()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let settings_store = SettingsStore::new(
                app.path()
                    .app_config_dir()
                    .map_err(|error| error.to_string())?,
            );
            let settings = settings_store
                .load()
                .map_err(|error| format!("could not load settings: {error}"))?;
            app.manage(AppState {
                settings: Mutex::new(settings),
                settings_store,
                arming_override: Mutex::new(ArmingOverride::None),
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
