use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

const ENUMERATION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInputDevice {
    pub id: String,
    pub name: String,
    pub channels: Option<u16>,
    pub default_sample_rate: Option<u32>,
    pub config_known: bool,
}

pub struct DeviceCache {
    devices: Mutex<Vec<AudioInputDevice>>,
    refreshed_at: Mutex<Option<Instant>>,
}

impl DeviceCache {
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(Vec::new()),
            refreshed_at: Mutex::new(None),
        }
    }

    pub fn devices(&self) -> Option<Vec<AudioInputDevice>> {
        let refreshed_at = match self.refreshed_at.lock() {
            Ok(guard) => *guard,
            Err(_) => return None,
        };
        refreshed_at?;
        match self.devices.lock() {
            Ok(guard) => Some(guard.clone()),
            Err(_) => None,
        }
    }

    fn store(&self, devices: Vec<AudioInputDevice>) {
        if let Ok(mut guard) = self.devices.lock() {
            *guard = devices;
        }
        if let Ok(mut guard) = self.refreshed_at.lock() {
            *guard = Some(Instant::now());
        }
    }
}

impl Default for DeviceCache {
    fn default() -> Self {
        Self::new()
    }
}

pub fn spawn_refresh(cache: Arc<DeviceCache>, interval: Duration) -> mpsc::Receiver<()> {
    let (ready, first_result) = mpsc::channel();
    std::thread::spawn(move || {
        let mut announced = false;
        loop {
            cache.store(enumerate());
            if !announced {
                announced = true;
                let _ = ready.send(());
            }
            std::thread::sleep(interval);
        }
    });
    first_result
}

pub fn await_first_enumeration(ready: &mpsc::Receiver<()>) {
    let _ = ready.recv_timeout(ENUMERATION_TIMEOUT);
}

fn enumerate() -> Vec<AudioInputDevice> {
    let host = cpal::default_host();
    let devices = match host.input_devices() {
        Ok(devices) => devices,
        Err(_) => return Vec::new(),
    };
    devices.map(describe).collect()
}

fn describe(device: cpal::Device) -> AudioInputDevice {
    let name = device
        .name()
        .unwrap_or_else(|_| "Unknown audio input device".into());
    match device.default_input_config() {
        Ok(config) => AudioInputDevice {
            id: name.clone(),
            name,
            channels: Some(config.channels()),
            default_sample_rate: Some(config.sample_rate().0),
            config_known: true,
        },
        Err(_) => AudioInputDevice {
            id: name.clone(),
            name,
            channels: None,
            default_sample_rate: None,
            config_known: false,
        },
    }
}

pub fn find<'a>(devices: &'a [AudioInputDevice], device_id: &str) -> Option<&'a AudioInputDevice> {
    devices.iter().find(|device| device.id == device_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_matches_only_the_configured_device_id() {
        let devices = [
            AudioInputDevice {
                id: "Built-in Microphone".into(),
                name: "Built-in Microphone".into(),
                channels: Some(2),
                default_sample_rate: Some(48_000),
                config_known: true,
            },
            AudioInputDevice {
                id: "USB Interface".into(),
                name: "USB Interface".into(),
                channels: Some(8),
                default_sample_rate: Some(48_000),
                config_known: true,
            },
        ];
        assert_eq!(
            find(&devices, "USB Interface").map(|device| &device.id),
            Some(&"USB Interface".to_string())
        );
        assert!(find(&devices, "Not Plugged In").is_none());
    }

    #[test]
    fn enumerating_the_real_host_does_not_panic() {
        for device in enumerate() {
            assert!(!device.id.is_empty());
        }
    }

    #[test]
    fn an_unpopulated_cache_reports_no_device_list_rather_than_an_empty_one() {
        let cache = DeviceCache::new();
        assert!(cache.devices().is_none());
        cache.store(Vec::new());
        assert_eq!(cache.devices(), Some(Vec::new()));
    }

    #[test]
    fn the_refresher_populates_the_cache_once_before_announcing() {
        let cache = Arc::new(DeviceCache::new());
        let ready = spawn_refresh(Arc::clone(&cache), Duration::from_secs(60));
        await_first_enumeration(&ready);
        assert!(cache.devices().is_some());
    }
}
