use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorStatus {
    pub state: &'static str,
    pub reason: &'static str,
    pub operational: bool,
}

impl SupervisorStatus {
    pub fn foundation() -> Self {
        Self {
            state: "not-observing",
            reason: "The supervision module is configured but process observation has not been implemented yet.",
            operational: false,
        }
    }
}
