use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::arming::ArmingPolicy;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub companion_name: String,
    pub coordinator_url: String,
    pub arming_policy: ArmingPolicy,
    pub launch_at_login: bool,
    pub resolume: ResolumeSettings,
    pub timecode: TimecodeSettings,
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
            arming_policy: ArmingPolicy::FollowShowNight,
            launch_at_login: true,
            resolume: ResolumeSettings::default(),
            timecode: TimecodeSettings::default(),
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

pub struct SettingsStore {
    directory: PathBuf,
}

impl SettingsStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn load(&self) -> Result<AppSettings, String> {
        let path = self.directory.join("settings.json");
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(settings) => Ok(settings),
                Err(_) => self.recover_invalid(&path),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(AppSettings::default())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn recover_invalid(&self, path: &std::path::Path) -> Result<AppSettings, String> {
        let backup = self.directory.join("settings.json.previous");
        if let Ok(bytes) = fs::read(&backup)
            && let Ok(settings) = serde_json::from_slice(&bytes)
        {
            return Ok(settings);
        }
        let invalid = self.directory.join("settings.json.invalid");
        if invalid.exists() {
            fs::remove_file(&invalid).map_err(|error| error.to_string())?;
        }
        fs::rename(path, invalid).map_err(|error| error.to_string())?;
        Ok(AppSettings::default())
    }

    pub fn save(&self, settings: &AppSettings) -> Result<(), String> {
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let path = self.directory.join("settings.json");
        let staging = self.directory.join("settings.json.new");
        let backup = self.directory.join("settings.json.previous");
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
        fs::write(&staging, bytes).map_err(|error| error.to_string())?;
        if path.exists() {
            if backup.exists() {
                fs::remove_file(&backup).map_err(|error| error.to_string())?;
            }
            fs::rename(&path, &backup).map_err(|error| error.to_string())?;
        }
        match fs::rename(&staging, &path) {
            Ok(()) => {
                if backup.exists() {
                    fs::remove_file(backup).map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            Err(error) => {
                if backup.exists() {
                    let _ = fs::rename(&backup, &path);
                }
                Err(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
