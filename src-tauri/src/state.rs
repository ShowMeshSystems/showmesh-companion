use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::arming::ArmingOverride;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct PersistedState {
    arming_override: ArmingOverride,
}

pub struct StateStore {
    directory: PathBuf,
}

impl StateStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn load(&self) -> Result<ArmingOverride, String> {
        let path = self.directory.join("state.json");
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedState>(&bytes) {
                Ok(state) => Ok(state.arming_override),
                Err(_) => Ok(self.recover_invalid(&path)),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ArmingOverride::None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn recover_invalid(&self, path: &std::path::Path) -> ArmingOverride {
        let backup = self.directory.join("state.json.previous");
        if let Ok(bytes) = fs::read(&backup)
            && let Ok(state) = serde_json::from_slice::<PersistedState>(&bytes)
        {
            return state.arming_override;
        }
        let invalid = self.directory.join("state.json.invalid");
        let _ = fs::remove_file(&invalid);
        let _ = fs::rename(path, &invalid);
        ArmingOverride::None
    }

    pub fn save(&self, arming_override: ArmingOverride) -> Result<(), String> {
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let path = self.directory.join("state.json");
        let staging = self.directory.join("state.json.new");
        let backup = self.directory.join("state.json.previous");
        let bytes = serde_json::to_vec_pretty(&PersistedState { arming_override })
            .map_err(|error| error.to_string())?;
        fs::write(&staging, bytes).map_err(|error| error.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "showmesh-companion-state-test-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_the_persisted_override() {
        let store = StateStore::new(temp_dir());
        store.save(ArmingOverride::DisarmUntilNextNight).unwrap();
        assert_eq!(store.load().unwrap(), ArmingOverride::DisarmUntilNextNight);
    }

    #[test]
    fn falls_back_to_none_when_the_state_file_is_corrupt() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("state.json"), b"not json").unwrap();
        let store = StateStore::new(dir.clone());
        assert_eq!(store.load().unwrap(), ArmingOverride::None);
        assert!(dir.join("state.json.invalid").exists());
    }

    #[test]
    fn recovers_the_previous_state_when_the_current_file_is_corrupt() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("state.json.previous"),
            br#"{"armingOverride":"disarm-until-next-night"}"#,
        )
        .unwrap();
        fs::write(dir.join("state.json"), b"not json").unwrap();
        let store = StateStore::new(dir);
        assert_eq!(store.load().unwrap(), ArmingOverride::DisarmUntilNextNight);
    }

    #[test]
    fn reports_a_read_failure_instead_of_reporting_no_override() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("state.json")).unwrap();
        let store = StateStore::new(dir);
        assert!(store.load().is_err());
    }

    #[test]
    fn treats_a_missing_state_file_as_no_override() {
        let store = StateStore::new(temp_dir());
        assert_eq!(store.load().unwrap(), ArmingOverride::None);
    }

    #[test]
    fn a_save_retains_the_previous_generation() {
        let dir = temp_dir();
        let store = StateStore::new(dir.clone());
        store.save(ArmingOverride::DisarmUntilNextNight).unwrap();
        assert!(!dir.join("state.json.previous").exists());

        store.save(ArmingOverride::ArmUntilNightEnds).unwrap();

        let backup = fs::read(dir.join("state.json.previous")).unwrap();
        let previous: PersistedState = serde_json::from_slice(&backup).unwrap();
        assert_eq!(
            previous.arming_override,
            ArmingOverride::DisarmUntilNextNight
        );
        assert_eq!(store.load().unwrap(), ArmingOverride::ArmUntilNightEnds);
    }
}
