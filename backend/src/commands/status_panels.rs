use crate::codey_plugins::status_panel::{self, Failure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub(super) fn list() -> Value {
    match status_panel::list() {
        Ok(panels) => json!({"status":"ok","panels":panels}),
        Err(_) => json!({"status":"failed"}),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    generation: String,
}

pub(super) async fn read(payload: &Value) -> Value {
    let result = match serde_json::from_value::<Request>(payload.clone()) {
        Ok(request) => match uuid::Uuid::parse_str(&request.generation) {
            Ok(generation) => {
                let mut work = tokio::task::spawn_blocking(move || {
                    status_panel::read(&request.id, generation)
                });
                match tokio::time::timeout(Duration::from_secs(8), &mut work).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err(Failure::Failed),
                    Err(_) => {
                        work.abort();
                        Err(Failure::Timeout)
                    }
                }
            }
            Err(_) => Err(Failure::Inactive),
        },
        Err(_) => Err(Failure::Invalid),
    };
    match result {
        Ok(panel) => json!({"status":"ok","panel":panel}),
        Err(code) => json!({"status":"failed","code":code}),
    }
}
