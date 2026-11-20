use codey_plugin_sdk::status_panel::{Panel, READ_METHOD};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Descriptor {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) generation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Failure {
    Inactive,
    Busy,
    Failed,
    Invalid,
    Timeout,
}

pub(crate) fn list() -> Result<Vec<Descriptor>, String> {
    Ok(super::manager()?
        .live
        .iter()
        .filter_map(|(id, plugin)| {
            Some(Descriptor {
                id: id.clone(),
                name: plugin.manifest.name.clone(),
                generation: plugin.status_generation?.to_string(),
            })
        })
        .collect())
}

pub(crate) fn read(id: &str, generation: Uuid) -> Result<Panel, Failure> {
    let instance = super::manager()
        .map_err(|_| Failure::Failed)?
        .live
        .get(id)
        .filter(|live| live.status_generation == Some(generation))
        .map(|live| live.instance.clone())
        .ok_or(Failure::Inactive)?;
    let reply = {
        let mut guard = instance.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::Busy,
            std::sync::TryLockError::Poisoned(_) => Failure::Failed,
        })?;
        guard.invoke(READ_METHOD, Value::Null)
    };
    if !super::manager()
        .map_err(|_| Failure::Failed)?
        .live
        .get(id)
        .is_some_and(|live| live.status_generation == Some(generation))
    {
        return Err(Failure::Inactive);
    }
    let panel: Panel = serde_json::from_value(reply.map_err(|_| Failure::Failed)?)
        .map_err(|_| Failure::Invalid)?;
    panel.validate().map_err(|_| Failure::Invalid)?;
    Ok(panel)
}
