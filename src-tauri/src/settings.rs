use std::{fmt, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::arming::ArmingPolicy;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub companion_name: String,
    pub coordinator_url: String,
    pub coordinator_token: Secret,
    pub arming_policy: ArmingPolicy,
    pub launch_at_login: bool,
    pub resolume: ResolumeSettings,
    pub timecode: TimecodeSettings,
    pub crash_reports: CrashReportSettings,
}

/// A credential that never appears in `Debug` output, so it cannot reach a log.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    #[cfg(test)]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if self.0.is_empty() {
            "<empty>"
        } else {
            "<set>"
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CrashReportSettings {
    pub enabled: bool,
}

impl Default for CrashReportSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResolumeSettings {
    pub enabled: bool,
    pub application_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TimecodeSettings {
    pub enabled: bool,
    pub device_id: String,
    pub channel: u16,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            companion_name: "ShowMesh Companion".into(),
            coordinator_url: String::new(),
            coordinator_token: Secret::default(),
            arming_policy: ArmingPolicy::FollowShowNight,
            launch_at_login: true,
            resolume: ResolumeSettings::default(),
            timecode: TimecodeSettings::default(),
            crash_reports: CrashReportSettings::default(),
        }
    }
}

impl Default for TimecodeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            device_id: String::new(),
            channel: 1,
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.companion_name.trim().is_empty() {
            return Err("Companion name is required.".into());
        }
        if !self.coordinator_url.is_empty()
            && !(self.coordinator_url.starts_with("http://")
                || self.coordinator_url.starts_with("https://"))
        {
            return Err("Coordinator address must begin with http:// or https://.".into());
        }
        let token = self.coordinator_token.expose();
        if token.len() > 4096 || !token.chars().all(|character| character.is_ascii_graphic()) {
            return Err(
                "The coordinator token must be pasted exactly as issued, with no spaces.".into(),
            );
        }
        if self.resolume.enabled && self.resolume.application_path.trim().is_empty() {
            return Err(
                "Choose the Resolume Arena application before enabling supervision.".into(),
            );
        }
        if self.timecode.enabled && self.timecode.device_id.trim().is_empty() {
            return Err("Choose an audio input before enabling LTC observation.".into());
        }
        if self.timecode.channel == 0 {
            return Err("LTC input channel must be 1 or greater.".into());
        }
        Ok(())
    }
}

pub struct LoadedSettings {
    pub settings: AppSettings,
    pub notice: Option<String>,
}

pub struct SettingsStore {
    directory: PathBuf,
}

impl SettingsStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn load(&self) -> Result<LoadedSettings, String> {
        let path = self.directory.join("settings.json");
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<AppSettings>(&bytes) {
                Ok(settings) => match settings.validate() {
                    Ok(()) => Ok(LoadedSettings {
                        settings,
                        notice: None,
                    }),
                    Err(error) => self.recover(
                        &path,
                        &format!("The saved settings could not be used: {error}"),
                    ),
                },
                Err(_) => self.recover(&path, "The settings file could not be read."),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LoadedSettings {
                settings: AppSettings::default(),
                notice: None,
            }),
            Err(error) => Err(error.to_string()),
        }
    }

    fn recover(&self, path: &std::path::Path, problem: &str) -> Result<LoadedSettings, String> {
        let backup = self.directory.join("settings.json.previous");
        if let Ok(bytes) = fs::read(&backup)
            && let Ok(settings) = serde_json::from_slice::<AppSettings>(&bytes)
            && settings.validate().is_ok()
        {
            return Ok(LoadedSettings {
                settings,
                notice: Some(format!(
                    "{problem} The previously saved settings were loaded instead."
                )),
            });
        }
        let invalid = self.directory.join("settings.json.invalid");
        if invalid.exists() {
            fs::remove_file(&invalid).map_err(|error| error.to_string())?;
        }
        fs::rename(path, invalid).map_err(|error| error.to_string())?;
        Ok(LoadedSettings {
            settings: AppSettings::default(),
            notice: Some(format!(
                "{problem} It was kept as settings.json.invalid and default settings were loaded."
            )),
        })
    }

    pub fn save(&self, settings: &AppSettings) -> Result<(), String> {
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let path = self.directory.join("settings.json");
        let staging = self.directory.join("settings.json.new");
        let backup = self.directory.join("settings.json.previous");
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
        fs::write(&staging, bytes).map_err(|error| error.to_string())?;
        restrict_to_owner(&staging)?;
        if path.exists() {
            if backup.exists() {
                fs::remove_file(&backup).map_err(|error| error.to_string())?;
            }
            fs::rename(&path, &backup).map_err(|error| error.to_string())?;
        }
        match fs::rename(&staging, &path) {
            Ok(()) => Ok(()),
            Err(error) => {
                if backup.exists() {
                    let _ = fs::rename(&backup, &path);
                }
                Err(error.to_string())
            }
        }
    }
}

/// The settings file holds the coordinator token, so only this user may read it.
#[cfg(unix)]
fn restrict_to_owner(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &std::path::Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "showmesh-companion-settings-test-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn enabled_modules_require_their_targets() {
        let mut settings = AppSettings::default();
        settings.resolume.enabled = true;
        assert_eq!(
            settings.validate().unwrap_err(),
            "Choose the Resolume Arena application before enabling supervision."
        );
        settings.resolume.application_path = "Arena".into();
        settings.timecode.enabled = true;
        assert_eq!(
            settings.validate().unwrap_err(),
            "Choose an audio input before enabling LTC observation."
        );
    }

    #[test]
    fn a_token_with_whitespace_is_rejected() {
        let settings = AppSettings {
            coordinator_token: Secret::new("abc def"),
            ..AppSettings::default()
        };
        assert!(settings.validate().unwrap_err().contains("token"));
    }

    #[test]
    fn the_token_never_appears_in_debug_output() {
        let settings = AppSettings {
            coordinator_token: Secret::new("smt_very_secret"),
            ..AppSettings::default()
        };
        let printed = format!("{settings:?}");
        assert!(!printed.contains("very_secret"));
        assert!(printed.contains("<set>"));
    }

    #[test]
    fn settings_saved_before_crash_reports_existed_send_them() {
        let settings: AppSettings = serde_json::from_str(r#"{"companionName":"Booth"}"#).unwrap();
        assert!(settings.crash_reports.enabled);
        assert!(settings.coordinator_token.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_saved_settings_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        SettingsStore::new(dir.clone())
            .save(&AppSettings::default())
            .unwrap();
        let mode = fs::metadata(dir.join("settings.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_missing_settings_file_loads_defaults_without_a_notice() {
        let store = SettingsStore::new(temp_dir());
        let loaded = store.load().unwrap();
        assert_eq!(loaded.settings.companion_name, "ShowMesh Companion");
        assert!(loaded.notice.is_none());
    }

    #[test]
    fn a_save_retains_the_previous_generation() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.clone());
        let first = AppSettings {
            companion_name: "First".into(),
            ..AppSettings::default()
        };
        store.save(&first).unwrap();
        assert!(!dir.join("settings.json.previous").exists());

        let second = AppSettings {
            companion_name: "Second".into(),
            ..AppSettings::default()
        };
        store.save(&second).unwrap();

        let backup = fs::read(dir.join("settings.json.previous")).unwrap();
        let previous: AppSettings = serde_json::from_slice(&backup).unwrap();
        assert_eq!(previous.companion_name, "First");
        assert_eq!(store.load().unwrap().settings.companion_name, "Second");
    }

    #[test]
    fn a_corrupt_settings_file_is_recovered_from_the_retained_generation() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.clone());
        let first = AppSettings {
            companion_name: "First".into(),
            ..AppSettings::default()
        };
        store.save(&first).unwrap();
        let second = AppSettings {
            companion_name: "Second".into(),
            ..AppSettings::default()
        };
        store.save(&second).unwrap();
        fs::write(dir.join("settings.json"), b"not json").unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.settings.companion_name, "First");
        assert!(loaded.notice.unwrap().contains("previously saved settings"));
    }

    #[test]
    fn a_corrupt_settings_file_without_a_retained_generation_loads_defaults() {
        let dir = temp_dir();
        fs::write(dir.join("settings.json"), b"not json").unwrap();
        let store = SettingsStore::new(dir.clone());

        let loaded = store.load().unwrap();
        assert_eq!(loaded.settings.companion_name, "ShowMesh Companion");
        assert!(dir.join("settings.json.invalid").exists());
        assert!(loaded.notice.unwrap().contains("settings.json.invalid"));
    }

    #[test]
    fn settings_that_fail_validation_are_rejected_on_load() {
        let dir = temp_dir();
        fs::write(
            dir.join("settings.json"),
            br#"{"companionName":"Booth","channel":1,"timecode":{"enabled":true,"deviceId":"USB Interface","channel":0}}"#,
        )
        .unwrap();
        let store = SettingsStore::new(dir.clone());

        let loaded = store.load().unwrap();
        assert_eq!(loaded.settings.timecode.channel, 1);
        assert!(!loaded.settings.timecode.enabled);
        assert!(dir.join("settings.json.invalid").exists());
        let notice = loaded.notice.unwrap();
        assert!(notice.contains("LTC input channel must be 1 or greater."));
    }

    #[test]
    fn a_retained_generation_that_fails_validation_is_not_used() {
        let dir = temp_dir();
        fs::write(
            dir.join("settings.json.previous"),
            br#"{"companionName":""}"#,
        )
        .unwrap();
        fs::write(dir.join("settings.json"), b"not json").unwrap();
        let store = SettingsStore::new(dir.clone());

        let loaded = store.load().unwrap();
        assert_eq!(loaded.settings.companion_name, "ShowMesh Companion");
        assert!(dir.join("settings.json.invalid").exists());
    }
}
