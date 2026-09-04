use serde::Serialize;

use crate::settings::AppSettings;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvertisedCapability {
    pub id: &'static str,
    pub version: u8,
}

pub fn derive(settings: &AppSettings) -> Vec<AdvertisedCapability> {
    let mut capabilities = Vec::with_capacity(2);
    if settings.resolume.enabled && !settings.resolume.application_path.trim().is_empty() {
        capabilities.push(AdvertisedCapability {
            id: "process.supervise",
            version: 1,
        });
    }
    if settings.timecode.enabled && !settings.timecode.device_id.trim().is_empty() {
        capabilities.push(AdvertisedCapability {
            id: "timecode.ltc.observe",
            version: 1,
        });
    }
    capabilities
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_only_configured_modules() {
        let mut settings = AppSettings::default();
        assert!(derive(&settings).is_empty());
        settings.resolume.enabled = true;
        settings.resolume.application_path = "/Applications/Resolume Arena.app".into();
        assert_eq!(
            derive(&settings),
            vec![AdvertisedCapability {
                id: "process.supervise",
                version: 1
            }]
        );
        settings.timecode.enabled = true;
        settings.timecode.device_id = "device-1".into();
        assert_eq!(derive(&settings).len(), 2);
    }
}
