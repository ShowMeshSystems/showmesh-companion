mod arming;
mod audio_devices;
mod capabilities;
mod capture;
#[allow(dead_code)]
mod ltc;
mod settings;
mod state;
mod supervisor;
mod timecode;
mod tray;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arming::{ArmingOverride, NightSessionState};
use audio_devices::AudioInputDevice;
use capture::{CaptureManager, CaptureTarget};
use serde::Serialize;
use settings::{AppSettings, SettingsStore, TimecodeSettings};
use state::StateStore;
use supervisor::{SupervisionState, SupervisorStatus};
use sysinfo::System;
use tauri::{AppHandle, Emitter, Manager, State};
use timecode::TimecodeStatus;

const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MENU_LINE_LIMIT: usize = 78;
const DEVICE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TIMECODE_TICK_INTERVAL: Duration = Duration::from_millis(100);
const TIMECODE_STATUS_EVENT: &str = "timecode-status";
const SNAPSHOT_EVENT: &str = "app-snapshot";

struct AppState {
    settings: Mutex<AppSettings>,
    settings_notice: Mutex<Option<String>>,
    settings_store: SettingsStore,
    state_store: StateStore,
    arming_override: Mutex<ArmingOverride>,
    night_state: Mutex<NightSessionState>,
    supervisor: Mutex<SupervisorStatus>,
    supervision_runtime: Mutex<SupervisionState>,
    tray_handles: Mutex<Option<tray::TrayHandles>>,
    device_cache: Arc<audio_devices::DeviceCache>,
    capture: CaptureManager,
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
    settings_notice: Option<String>,
}

impl AppState {
    fn snapshot(&self) -> Result<AppSnapshot, String> {
        let settings = self
            .settings
            .lock()
            .map_err(|_| "settings lock is poisoned")?
            .clone();
        let night_guard = self
            .night_state
            .lock()
            .map_err(|_| "night-state lock is poisoned")?;
        let override_guard = self
            .arming_override
            .lock()
            .map_err(|_| "arming-override lock is poisoned")?;
        let night_state = *night_guard;
        let arming_override = *override_guard;
        drop(override_guard);
        drop(night_guard);
        let arming = arming::resolve(settings.arming_policy, night_state, arming_override);
        let capabilities = capabilities::derive(&settings);
        let supervisor = self
            .supervisor
            .lock()
            .map_err(|_| "supervisor lock is poisoned")?
            .clone();
        let timecode = self.timecode_status(&settings.timecode);
        let settings_notice = self
            .settings_notice
            .lock()
            .map_err(|_| "settings-notice lock is poisoned")?
            .clone();

        Ok(AppSnapshot {
            settings,
            capabilities,
            night_state,
            arming_override,
            effective_armed: arming.armed,
            arming_reason: arming.reason,
            supervisor,
            timecode,
            settings_notice,
        })
    }

    fn poll_supervisor(&self, system: &mut System) {
        let settings = match self.settings.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        let night_guard = match self.night_state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let override_guard = match self.arming_override.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let night_state = *night_guard;
        let arming_override = *override_guard;
        drop(override_guard);
        drop(night_guard);
        let decision = arming::resolve(settings.arming_policy, night_state, arming_override);

        let runtime = match self.supervision_runtime.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };

        let (status, next_runtime) = supervisor::tick(
            &settings.resolume,
            decision.armed,
            system,
            &runtime,
            Instant::now(),
        );
        let timecode = self.timecode_status(&settings.timecode);
        self.refresh_tray(
            decision.armed,
            &decision.reason,
            &status.reason,
            &timecode.reason,
        );

        if let Ok(mut guard) = self.supervision_runtime.lock() {
            *guard = next_runtime;
        }
        if let Ok(mut guard) = self.supervisor.lock() {
            *guard = status;
        }
    }

    fn refresh_tray(
        &self,
        armed: bool,
        arming_reason: &str,
        supervisor_reason: &str,
        timecode_reason: &str,
    ) {
        let Some(handles) = self.tray_handles() else {
            return;
        };
        let _ = handles.armed_item.set_text(format!(
            "{}: {arming_reason}",
            if armed { "Armed" } else { "Disarmed" }
        ));
        let _ = handles
            .supervisor_item
            .set_text(menu_line("Resolume", supervisor_reason));
        let _ = handles
            .timecode_item
            .set_text(menu_line("LTC", timecode_reason));
    }

    fn tray_handles(&self) -> Option<tray::TrayHandles> {
        let guard = self.tray_handles.lock().ok()?;
        guard.as_ref().cloned()
    }

    fn report_tray_error(&self, error: &str) {
        let Some(handles) = self.tray_handles() else {
            return;
        };
        let _ = handles.armed_item.set_text(menu_line("Warning", error));
    }

    fn timecode_status(&self, settings: &TimecodeSettings) -> TimecodeStatus {
        let settings_status =
            TimecodeStatus::from_settings(settings, self.device_cache.devices().as_deref());
        let observation = self.capture.observation();
        TimecodeStatus::combine(settings_status, Some(&observation), capture::now_ms())
    }

    fn reconfigure_capture(&self, settings: &TimecodeSettings) {
        self.capture.configure(capture_target(settings));
    }

    fn apply_override(&self, value: ArmingOverride) -> Result<AppSnapshot, String> {
        let mut arming_override = self
            .arming_override
            .lock()
            .map_err(|_| "arming-override lock is poisoned")?;
        self.state_store
            .save(value)
            .map_err(|error| format!("Could not save the temporary override: {error}"))?;
        *arming_override = value;
        drop(arming_override);
        let snapshot = self.snapshot()?;
        self.refresh_tray(
            snapshot.effective_armed,
            &snapshot.arming_reason,
            &snapshot.supervisor.reason,
            &snapshot.timecode.reason,
        );
        Ok(snapshot)
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
        *night_state = next;
        *arming_override = resolved_override;
        drop(arming_override);
        drop(night_state);
        self.state_store.save(resolved_override)
    }
}

fn capture_target(settings: &TimecodeSettings) -> Option<CaptureTarget> {
    if !settings.enabled || settings.channel == 0 {
        return None;
    }
    let device_id = settings.device_id.trim();
    if device_id.is_empty() {
        return None;
    }
    Some(CaptureTarget {
        device_id: device_id.to_string(),
        channel: settings.channel,
    })
}

fn menu_line(label: &str, reason: &str) -> String {
    let mut line = format!("{label}: {reason}");
    if line.chars().count() > MENU_LINE_LIMIT {
        line = line.chars().take(MENU_LINE_LIMIT - 1).collect();
        line.push('\u{2026}');
    }
    line
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    state.snapshot()
}

#[tauri::command]
fn save_settings(settings: AppSettings, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    settings.validate()?;
    state.settings_store.save(&settings)?;
    state.reconfigure_capture(&settings.timecode);
    *state
        .settings_notice
        .lock()
        .map_err(|_| "settings-notice lock is poisoned")? = None;
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
    state.apply_override(value)
}

#[tauri::command]
fn list_audio_inputs(state: State<'_, AppState>) -> Vec<AudioInputDevice> {
    state.device_cache.devices().unwrap_or_default()
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
            let loaded = settings_store
                .load()
                .map_err(|error| format!("could not load settings: {error}"))?;
            let settings = loaded.settings;
            let state_store = StateStore::new(config_dir);
            let arming_override = state_store
                .load()
                .map_err(|error| format!("could not load saved state: {error}"))?;
            let decision = arming::resolve(
                settings.arming_policy,
                NightSessionState::Unknown,
                arming_override,
            );
            let device_cache = Arc::new(audio_devices::DeviceCache::new());
            let devices_ready =
                audio_devices::spawn_refresh(Arc::clone(&device_cache), DEVICE_REFRESH_INTERVAL);
            audio_devices::await_first_enumeration(&devices_ready);
            let initial_timecode = TimecodeStatus::from_settings(
                &settings.timecode,
                device_cache.devices().as_deref(),
            );
            let initial_timecode_settings = settings.timecode.clone();
            let mut system = System::new();
            let (initial_status, initial_runtime) = supervisor::tick(
                &settings.resolume,
                decision.armed,
                &mut system,
                &SupervisionState::default(),
                Instant::now(),
            );
            app.manage(AppState {
                settings: Mutex::new(settings),
                settings_notice: Mutex::new(loaded.notice),
                settings_store,
                state_store,
                arming_override: Mutex::new(arming_override),
                night_state: Mutex::new(NightSessionState::Unknown),
                supervisor: Mutex::new(initial_status.clone()),
                supervision_runtime: Mutex::new(initial_runtime),
                tray_handles: Mutex::new(None),
                device_cache,
                capture: CaptureManager::start(),
            });

            app.state::<AppState>()
                .reconfigure_capture(&initial_timecode_settings);

            #[cfg(target_os = "macos")]
            app.handle()
                .set_activation_policy(tauri::ActivationPolicy::Accessory)?;

            let tray_handles = tray::build(app.handle())?;
            *app.state::<AppState>()
                .tray_handles
                .lock()
                .map_err(|_| "tray lock is poisoned")? = Some(tray_handles);
            app.state::<AppState>().refresh_tray(
                decision.armed,
                &decision.reason,
                &initial_status.reason,
                &initial_timecode.reason,
            );

            if let Some(window) = app.get_webview_window("main") {
                let close_window = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = close_window.set_skip_taskbar(true);
                        let _ = close_window.hide();
                    }
                });
            }

            let handle: AppHandle = app.handle().clone();
            std::thread::spawn(move || {
                let mut system = system;
                loop {
                    std::thread::sleep(SUPERVISOR_POLL_INTERVAL);
                    let state = handle.state::<AppState>();
                    state.poll_supervisor(&mut system);
                }
            });

            let timecode_handle: AppHandle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(TIMECODE_TICK_INTERVAL);
                    let state = timecode_handle.state::<AppState>();
                    let Ok(settings) = state.settings.lock() else {
                        continue;
                    };
                    let timecode = state.timecode_status(&settings.timecode);
                    drop(settings);
                    let _ = timecode_handle.emit(TIMECODE_STATUS_EVENT, timecode);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_settings,
            set_arming_override,
            list_audio_inputs
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
            settings_notice: Mutex::new(None),
            settings_store: SettingsStore::new(dir.clone()),
            state_store: StateStore::new(dir),
            arming_override: Mutex::new(ArmingOverride::None),
            night_state: Mutex::new(NightSessionState::Unknown),
            supervisor: Mutex::new(SupervisorStatus::not_configured()),
            supervision_runtime: Mutex::new(SupervisionState::default()),
            tray_handles: Mutex::new(None),
            device_cache: Arc::new(audio_devices::DeviceCache::new()),
            capture: CaptureManager::start(),
        }
    }

    #[test]
    fn apply_night_state_expires_override_across_night_boundary() {
        let state = temp_state();
        state
            .state_store
            .save(ArmingOverride::ArmUntilNightEnds)
            .unwrap();
        *state.night_state.lock().unwrap() = NightSessionState::FadingOut;
        *state.arming_override.lock().unwrap() = ArmingOverride::ArmUntilNightEnds;

        state.apply_night_state(NightSessionState::Stopped).unwrap();

        assert_eq!(
            *state.night_state.lock().unwrap(),
            NightSessionState::Stopped
        );
        assert_eq!(*state.arming_override.lock().unwrap(), ArmingOverride::None);
        assert_eq!(state.state_store.load().unwrap(), ArmingOverride::None);
    }

    #[test]
    fn apply_night_state_keeps_override_across_a_non_boundary_transition() {
        let state = temp_state();
        state
            .state_store
            .save(ArmingOverride::ArmUntilNightEnds)
            .unwrap();
        *state.night_state.lock().unwrap() = NightSessionState::Preshow;
        *state.arming_override.lock().unwrap() = ArmingOverride::ArmUntilNightEnds;

        state.apply_night_state(NightSessionState::Live).unwrap();

        assert_eq!(*state.night_state.lock().unwrap(), NightSessionState::Live);
        assert_eq!(
            *state.arming_override.lock().unwrap(),
            ArmingOverride::ArmUntilNightEnds
        );
        assert_eq!(
            state.state_store.load().unwrap(),
            ArmingOverride::ArmUntilNightEnds
        );
    }
}
