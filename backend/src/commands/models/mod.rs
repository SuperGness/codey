//! Model management commands, split by concern. Every submodule keeps
//! `pub(crate)` items so the glob re-exports below present the same flat API
//! that `commands.rs` consumed when this was a single 3.5K-line file.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    AppState, STARTUP_PROVIDER_MODEL_SYNC_TIMEOUT, SubagentHotReloadOutcome,
    ensure_local_route_config_writable, hot_reload_runtime_subagent_config, redacted_config,
    runtime_config_requires_restart, save_config_to_store,
};
use crate::cdp;
use crate::codex_config::codex_home;
use crate::codex_provider;
use crate::config::{
    CodeyConfig, DERIVED_OFFICIAL_PROFILE_ID, OFFICIAL_ROUTE_SHORT_NAME, ProviderProfile,
    validate_provider_profiles,
};
use crate::error_log;
use crate::local_router;
use crate::model_catalog;
use crate::model_id;
use crate::provider_models;
use crate::subagent_policy;

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
