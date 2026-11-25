use super::*;

/// Application runtime whose catalog Codey already rendered in this process.
/// A different path or binary must be captured even when it is an older build.
#[cfg(not(test))]
static RUNTIME_SNAPSHOT_SYNC_ATTEMPTED: std::sync::Mutex<Option<RuntimeCatalogSource>> =
    std::sync::Mutex::new(None);

/// Claims the right to render the catalog for the current local Codex build.
/// The claim is released again when the render fails so a later launch in the
/// same process can retry.
pub(super) fn claim_snapshot_sync_slot<T: PartialEq>(
    attempted: &std::sync::Mutex<Option<T>>,
    stamp: Option<T>,
) -> bool {
    let Ok(mut attempted) = attempted.lock() else {
        return true;
    };
    if stamp.is_some() && *attempted == stamp {
        return false;
    }
    *attempted = stamp;
    true
}

pub(super) fn release_snapshot_sync_slot<T: PartialEq>(
    attempted: &std::sync::Mutex<Option<T>>,
    stamp: Option<T>,
) {
    if stamp.is_none() {
        return;
    }
    if let Ok(mut attempted) = attempted.lock()
        && *attempted == stamp
    {
        *attempted = None;
    }
}

/// Codex 26.908.x stopped maintaining `models_cache.json` on disk. When no
/// file source carries instruction-bearing entries, ask the bundled Codex CLI
/// to render its catalog and keep the output as a Codey-owned snapshot that
/// `read_official_entries` picks up like any other source.
#[cfg(not(test))]
pub(super) fn sync_runtime_catalog_snapshot(
    home: &Path,
    context: &CatalogContext<'_>,
) -> Result<bool> {
    let Some(source) = context.source() else {
        eprintln!("无法确认所选 Codex 的命令行来源，跳过模型清单探测");
        return Ok(false);
    };
    if !claim_snapshot_sync_slot(&RUNTIME_SNAPSHOT_SYNC_ATTEMPTED, Some(source.clone())) {
        return Ok(false);
    }
    let Some(models) = debug_model_entries(home, context.candidates()) else {
        eprintln!("本机 Codex 命令行未产出可用的模型清单快照");
        // Nothing was captured, so a later launch in this process may retry.
        release_snapshot_sync_slot(&RUNTIME_SNAPSHOT_SYNC_ATTEMPTED, Some(source.clone()));
        return Ok(false);
    };
    let catalog = serde_json::to_vec_pretty(&json!({
        "models": models,
        "codey_account_snapshot": true,
        "codey_runtime_source": source,
    }))
    .context("序列化 Codex 内置模型目录快照失败")?;
    atomic_write(&home.join(DEBUG_CATALOG_RELATIVE_PATH), &catalog)
        .context("写入 Codex 内置模型目录快照失败")?;
    Ok(true)
}

/// Explicit model synchronization must be allowed to observe account changes
/// even when the installed Codex binary has not changed since the last launch.
pub(crate) fn refresh_account_runtime_snapshot_in_context(
    home: &Path,
    context: &CatalogContext<'_>,
) -> Result<bool> {
    #[cfg(not(test))]
    {
        if let Ok(mut attempted) = RUNTIME_SNAPSHOT_SYNC_ATTEMPTED.lock() {
            *attempted = None;
        }
    }
    sync_runtime_catalog_snapshot(home, context)
}

/// Merges the account-specific model response into the Codey-owned runtime
/// catalog. The catalog is shared, while each route keeps its own upstream
/// model IDs and filters this union when building its selection state.
///
/// The account response is the availability list. It usually has no Codex
/// instruction template, so an existing runtime entry keeps those fields, and
/// a slug the local catalog has never seen borrows the closest template.
pub(crate) fn merge_account_runtime_models(
    home: &Path,
    models: &[Value],
    codex_app_path: &str,
) -> Result<()> {
    merge_account_runtime_models_in_context(home, models, &CatalogContext::new(codex_app_path))
}

pub(crate) fn merge_account_runtime_models_in_context(
    home: &Path,
    models: &[Value],
    context: &CatalogContext<'_>,
) -> Result<()> {
    let path = home.join(DEBUG_CATALOG_RELATIVE_PATH);
    let source = context.source();
    let mut catalog = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|value| snapshot_matches_runtime_source(value, source))
        .unwrap_or_else(|| json!({"models": [], "codey_account_snapshot": true}));
    let entries = catalog
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("Codex 模型目录快照格式无效"))?;
    let mut templates = entries.clone();
    let mut positions = entries
        .iter()
        .enumerate()
        .filter_map(|(index, model)| {
            model
                .get("slug")
                .and_then(Value::as_str)
                .map(|slug| (model_id::key(slug), index))
        })
        .collect::<std::collections::HashMap<_, _>>();
    for model in models.iter().filter(|model| {
        model
            .get("slug")
            .and_then(Value::as_str)
            .is_some_and(|slug| !slug.trim().is_empty())
    }) {
        let key = model_id::key(model["slug"].as_str().unwrap_or_default());
        let existing = positions
            .get(&key)
            .copied()
            .map(|index| entries[index].clone());
        let merged = merge_account_model(existing.as_ref(), model, &templates);
        if model_instruction_source(&merged).is_some() {
            templates.push(merged.clone());
        }
        if let Some(index) = positions.get(&key).copied() {
            entries[index] = merged;
        } else {
            positions.insert(key, entries.len());
            entries.push(merged);
        }
    }
    catalog["codey_account_snapshot"] = Value::Bool(true);
    if let Some(source) = source {
        catalog["codey_runtime_source"] = serde_json::to_value(source)?;
    }
    let bytes = serde_json::to_vec_pretty(&catalog).context("序列化官方模型目录快照失败")?;
    atomic_write(&path, &bytes).context("写入官方模型目录快照失败")
}

/// Unit tests must never invoke the Codex CLI installed on the machine running
/// them: the render is slow, needs account state, and would write a real
/// snapshot. The surrounding logic is covered through the pure helpers below.
#[cfg(test)]
pub(super) fn sync_runtime_catalog_snapshot(
    _home: &Path,
    _context: &CatalogContext<'_>,
) -> Result<bool> {
    record_snapshot_probe();
    Ok(false)
}

/// A runtime snapshot is useful even when upstream has added or retired a
/// model that Codey has not learned about yet. The account-backed directory
/// only needs at least one complete model entry; the fixed list remains the
/// fallback for installations that cannot produce a snapshot at all.
pub(super) fn snapshot_has_runtime_models(models: &[Value]) -> bool {
    models
        .iter()
        .any(|model| model_instruction_source(model).is_some())
}

#[cfg(test)]
pub(super) fn snapshot_covers_official_models(models: &[Value]) -> bool {
    OFFICIAL_MODELS.iter().all(|(slug, _)| {
        models.iter().any(|model| {
            model.get("slug").and_then(Value::as_str) == Some(*slug)
                && model_instruction_source(model).is_some()
        })
    })
}
