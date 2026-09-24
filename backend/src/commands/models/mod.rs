//! Model management commands, split by concern. Every submodule keeps
//! `pub(crate)` items so the glob re-exports below present the same flat API
//! that `commands.rs` consumed when this was a single 3.5K-line file.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    AppState, STARTUP_PROVIDER_MODEL_SYNC_TIMEOUT, SubagentHotReloadOutcome, argument,
    ensure_local_route_config_writable, hot_reload_runtime_subagent_config, optional_argument,
    redacted_config, runtime_config_requires_restart, save_config_to_store, string_argument,
};
use crate::cdp;
use crate::codex_config::codex_home;
use crate::codex_provider;
use crate::config::{
    CodeyConfig, OFFICIAL_ROUTE_SHORT_NAME, ProviderProfile, validate_provider_profiles,
};
use crate::error_log;
use crate::local_router;
use crate::model_catalog;
use crate::model_id;
use crate::provider_models;
use crate::subagent_policy;

// Record only stage names and durations, never route credentials or model payloads.
pub(crate) struct ModelOperationTimings {
    started: std::time::Instant,
    previous: std::time::Instant,
    detail: serde_json::Map<String, Value>,
}

impl ModelOperationTimings {
    pub(crate) fn new(operation: &'static str) -> Self {
        let started = std::time::Instant::now();
        Self {
            started,
            previous: started,
            detail: serde_json::Map::from_iter([("operation".into(), json!(operation))]),
        }
    }

    pub(crate) fn mark(&mut self, stage: &'static str) {
        let now = std::time::Instant::now();
        self.detail.insert(
            stage.into(),
            json!(now.duration_since(self.previous).as_millis() as u64),
        );
        self.previous = now;
    }
}

impl Drop for ModelOperationTimings {
    fn drop(&mut self) {
        self.detail.insert(
            "totalMs".into(),
            json!(self.started.elapsed().as_millis() as u64),
        );
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "models.operation_timings",
            Value::Object(std::mem::take(&mut self.detail)),
        );
    }
}

mod catalog_refresh;
mod defaults;
mod native;
mod routes;
mod selection;
mod state;
mod sync;
#[cfg(test)]
mod tests;

pub(crate) use catalog_refresh::*;
pub use defaults::*;
pub(crate) use native::*;
pub use routes::*;
pub use selection::*;
pub(crate) use state::*;
pub use sync::*;

pub(super) async fn invoke(
    state: &Arc<AppState>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    match command {
        "sync_current_provider" => sync_current_provider_command(state).await,
        "set_route_enabled" => match (
            string_argument(args, "routeId"),
            argument::<bool>(args, "enabled"),
            argument::<u64>(args, "expectedRevision"),
        ) {
            (Ok(route_id), Ok(enabled), Ok(expected_revision)) => {
                set_route_enabled(state, route_id, enabled, expected_revision).await
            }
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
        },
        "reorder_route_models" => match (
            string_argument(args, "routeId"),
            argument::<Vec<String>>(args, "models"),
            argument::<u64>(args, "expectedRevision"),
        ) {
            (Ok(route_id), Ok(models), Ok(expected_revision)) => {
                reorder_route_models(state, route_id, models, expected_revision).await
            }
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
        },
        "delete_route" => match (
            string_argument(args, "routeId"),
            argument::<u64>(args, "expectedRevision"),
        ) {
            (Ok(route_id), Ok(expected_revision)) => {
                delete_route(state, route_id, expected_revision).await
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "fetch_route_models" => match (
            string_argument(args, "routeId"),
            argument::<u64>(args, "expectedRevision"),
        ) {
            (Ok(route_id), Ok(expected_revision)) => {
                fetch_route_models(state, route_id, expected_revision).await
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "save_selected_models" => match (
            argument::<Vec<String>>(args, "officialModels"),
            argument::<Vec<String>>(args, "thirdPartyModels"),
            optional_argument::<Vec<String>>(args, "manualThirdPartyModels"),
            optional_argument::<Vec<String>>(args, "deletedThirdPartyModels"),
            optional_argument::<bool>(args, "supportsAutoReview"),
            optional_argument::<Option<String>>(args, "routeId").map(Option::flatten),
            optional_argument::<BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>>(
                args,
                "reasoningEfforts",
            ),
            optional_argument::<BTreeMap<String, crate::config::ModelContextConfig>>(
                args,
                "modelContexts",
            ),
        ) {
            (
                Ok(official_models),
                Ok(third_party_models),
                Ok(manual_third_party_models),
                Ok(deleted_third_party_models),
                Ok(supports_auto_review),
                Ok(route_id),
                Ok(model_reasoning_efforts),
                Ok(model_contexts),
            ) => {
                save_selected_models(
                    state,
                    official_models,
                    third_party_models,
                    manual_third_party_models.unwrap_or_default(),
                    deleted_third_party_models.unwrap_or_default(),
                    supports_auto_review,
                    route_id,
                    model_reasoning_efforts,
                    model_contexts,
                )
                .await
            }
            (Err(error), _, _, _, _, _, _, _)
            | (_, Err(error), _, _, _, _, _, _)
            | (_, _, Err(error), _, _, _, _, _)
            | (_, _, _, Err(error), _, _, _, _)
            | (_, _, _, _, Err(error), _, _, _)
            | (_, _, _, _, _, Err(error), _, _)
            | (_, _, _, _, _, _, Err(error), _)
            | (_, _, _, _, _, _, _, Err(error)) => Err(error),
        },
        "save_default_model" => match (
            string_argument(args, "model"),
            optional_argument::<Option<String>>(args, "routeId").map(Option::flatten),
        ) {
            (Ok(model), Ok(route_id)) => save_default_model(state, model, route_id).await,
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "save_official_route_models" => match (
            string_argument(args, "routeId"),
            argument::<Vec<String>>(args, "models"),
            optional_argument::<BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>>(
                args,
                "reasoningEfforts",
            ),
            optional_argument::<bool>(args, "enabled"),
            optional_argument::<bool>(args, "showAccountUsageInHeader"),
            optional_argument::<BTreeMap<String, crate::config::ModelContextConfig>>(
                args,
                "modelContexts",
            ),
            optional_argument::<String>(args, "upstreamProxy"),
        ) {
            (
                Ok(route_id),
                Ok(models),
                Ok(_context_models),
                Ok(enabled),
                Ok(show_usage),
                Ok(_model_contexts),
                Ok(upstream_proxy),
            ) => {
                match (
                    optional_argument::<String>(args, "accountId"),
                    optional_argument::<String>(args, "routeName"),
                    optional_argument::<String>(args, "routeShortName"),
                    optional_argument::<String>(args, "baseUrl"),
                ) {
                    (Ok(account_id), Ok(route_name), Ok(route_short_name), Ok(base_url)) => {
                        save_official_route_models(
                            state,
                            OfficialRouteModelSave {
                                route_id,
                                models,
                                enabled,
                                show_account_usage: show_usage,
                                upstream_proxy,
                                base_url,
                                account_id,
                                route_name,
                                route_short_name,
                            },
                        )
                        .await
                    }
                    (Err(error), _, _, _)
                    | (_, Err(error), _, _)
                    | (_, _, Err(error), _)
                    | (_, _, _, Err(error)) => Err(error),
                }
            }
            (Err(error), _, _, _, _, _, _)
            | (_, Err(error), _, _, _, _, _)
            | (_, _, Err(error), _, _, _, _)
            | (_, _, _, Err(error), _, _, _)
            | (_, _, _, _, Err(error), _, _)
            | (_, _, _, _, _, Err(error), _)
            | (_, _, _, _, _, _, Err(error)) => Err(error),
        },
        _ => Err(format!("未知 Codey API 命令：{command}")),
    }
}
