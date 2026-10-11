use std::sync::Arc;
use std::time::Duration;

use super::{
    AppState, SubagentHotReloadOutcome, hot_reload_runtime_subagent_config,
    should_reconcile_runtime_subagent_config, subagent_hot_reload_commit_is_current,
};
use crate::config::CodeyConfig;

#[test]
fn commits_only_to_the_same_live_runtime_and_current_config() {
    assert!(subagent_hot_reload_commit_is_current(
        false, false, 7, 7, true, true, false
    ));
    for stale in [
        (true, false, 7, 7, true, true, false),
        (false, true, 7, 7, true, true, false),
        (false, false, 7, 8, true, true, false),
        (false, false, 7, 7, false, true, false),
        (false, false, 7, 7, true, false, false),
        (false, false, 7, 7, true, true, true),
    ] {
        assert!(!subagent_hot_reload_commit_is_current(
            stale.0, stale.1, stale.2, stale.3, stale.4, stale.5, stale.6,
        ));
    }
}

#[test]
fn enabled_saves_reconcile_runtime_files_even_when_roles_are_unchanged() {
    let enabled = CodeyConfig {
        subagent_optimization: true,
        ..CodeyConfig::default()
    };
    let mut changed_roles = enabled.clone();
    changed_roles
        .subagent_roles
        .get_mut("codey_quick_scan")
        .unwrap()
        .model = "provider-custom-model".into();
    let disabled = CodeyConfig::default();

    assert!(should_reconcile_runtime_subagent_config(&enabled, &enabled));
    assert!(should_reconcile_runtime_subagent_config(
        &enabled,
        &changed_roles
    ));
    assert!(!should_reconcile_runtime_subagent_config(
        &disabled, &enabled
    ));
    assert!(!should_reconcile_runtime_subagent_config(
        &enabled, &disabled
    ));
}

#[test]
fn failed_reconciliation_forces_a_restart_but_superseded_work_does_not() {
    let failed = SubagentHotReloadOutcome::failed("lease unavailable");
    assert!(failed.requires_restart());
    assert_eq!(failed.health(), "restart_required");

    let superseded = SubagentHotReloadOutcome::superseded("newer save won");
    assert!(!superseded.requires_restart());
    assert_eq!(superseded.health(), "superseded");
}

#[test]
fn role_registration_changes_are_pending_restart_without_claiming_application() {
    let outcome = SubagentHotReloadOutcome::pending_restart("roles changed");
    assert!(outcome.requires_restart());
    assert!(!outcome.reloaded());
    assert!(!outcome.repaired());
    assert_eq!(outcome.health(), "pending_restart");
    assert_eq!(outcome.error(), Some("roles changed"));
}

#[tokio::test]
async fn hot_reload_commit_is_serialized_with_config_writers() {
    let state = Arc::new(AppState::default());
    *state.config.write().await = CodeyConfig::default();
    let config = state.config.read().await.clone();
    let guard = state.config_write_lock.lock().await;
    let reload = tokio::spawn({
        let state = Arc::clone(&state);
        async move { hot_reload_runtime_subagent_config(&state, &config).await }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!reload.is_finished());
    drop(guard);
    let outcome = tokio::time::timeout(Duration::from_secs(1), reload)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.health(), "superseded");
}

#[tokio::test]
async fn hot_reload_does_not_hold_the_config_lock_while_waiting_for_lifecycle() {
    let state = Arc::new(AppState::default());
    *state.config.write().await = CodeyConfig::default();
    let config = state.config.read().await.clone();
    let lifecycle_guard = state.runtime_operation.lock().await;
    let reload = tokio::spawn({
        let state = Arc::clone(&state);
        async move { hot_reload_runtime_subagent_config(&state, &config).await }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!reload.is_finished());
    let config_guard =
        tokio::time::timeout(Duration::from_millis(250), state.config_write_lock.lock())
            .await
            .expect("热更新等待生命周期锁时不应占用配置写锁");
    drop(config_guard);
    drop(lifecycle_guard);

    let outcome = tokio::time::timeout(Duration::from_secs(1), reload)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.health(), "superseded");
}
