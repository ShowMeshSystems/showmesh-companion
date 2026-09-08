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

    pub fn load(&self) -> ArmingOverride {
        let path = self.directory.join("state.json");
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedState>(&bytes) {
                Ok(state) => state.arming_override,
                Err(_) => self.recover_invalid(&path),
            },
            Err(_) => ArmingOverride::None,
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
        assert_eq!(store.load(), ArmingOverride::DisarmUntilNextNight);
    }

    #[test]
    fn falls_back_to_none_when_the_state_file_is_corrupt() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("state.json"), b"not json").unwrap();
        let store = StateStore::new(dir);
        assert_eq!(store.load(), ArmingOverride::None);
    }

    #[test]
    fn falls_back_to_none_when_the_state_file_is_missing() {
        let store = StateStore::new(temp_dir());
        assert_eq!(store.load(), ArmingOverride::None);
    }
}
