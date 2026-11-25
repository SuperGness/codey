use super::*;

/// Makes a previously generated catalog safe to reuse when the upstream model
/// cache cannot be refreshed. Older catalogs may still advertise capabilities
/// whose route settings have since changed.
pub(crate) fn prepare_cached_catalog_for_current_capabilities(
    home: &Path,
    native_web_search_models: &[String],
    image_detail_original_models: &[String],
    remote_compaction_models: &[String],
    codex_app_path: &str,
) -> Result<bool> {
    prepare_cached_catalog_in_context(
        home,
        native_web_search_models,
        image_detail_original_models,
        remote_compaction_models,
        &CatalogContext::new(codex_app_path),
    )
}

pub(crate) fn prepare_cached_catalog_in_context(
    home: &Path,
    native_web_search_models: &[String],
    image_detail_original_models: &[String],
    remote_compaction_models: &[String],
    context: &CatalogContext<'_>,
) -> Result<bool> {
    let Some(value) = read_catalog_value(&home.join(relative_path())) else {
        return Ok(false);
    };
    if !snapshot_matches_runtime_source(&value, context.source()) {
        return Ok(false);
    }
    let mut models = catalog_models_from_value(&value);
    if !runtime_compatible_models(&models) {
        return Ok(false);
    }
    let runtime_source = value.get("codey_runtime_source").cloned();
    drop(value);

    let allowed_model_keys = native_web_search_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let image_detail_original_model_keys = image_detail_original_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let mut changed = apply_remote_compaction_models(&mut models, remote_compaction_models);
    for model in &mut models {
        changed |= gate_cached_native_web_search(model, &allowed_model_keys);
        changed |= gate_cached_image_detail_original(model, &image_detail_original_model_keys);
        changed |= prepare_cached_context_window(model);
        changed |= normalize_third_party_compaction_identity(model);
    }
    if !changed {
        return Ok(true);
    }

    let written = write_catalog_with_source(home, &models, runtime_source)?;
    verify_written_catalog(home, &models, &written)?;
    let mut verify_changed = apply_remote_compaction_models(&mut models, remote_compaction_models);
    for model in &mut models {
        verify_changed |= gate_cached_native_web_search(model, &allowed_model_keys);
        verify_changed |=
            gate_cached_image_detail_original(model, &image_detail_original_model_keys);
        verify_changed |= prepare_cached_context_window(model);
        verify_changed |= normalize_third_party_compaction_identity(model);
    }
    if verify_changed {
        bail!("复用的 Codey 模型目录仍包含与当前线路不匹配的模型能力");
    }
    Ok(true)
}

pub(super) fn apply_remote_compaction_models(models: &mut [Value], allowed: &[String]) -> bool {
    let allowed = allowed
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let mut changed = false;
    for model in models {
        let enabled = model
            .get("slug")
            .and_then(Value::as_str)
            .is_some_and(|slug| allowed.contains(&model_id::key(slug)));
        if model
            .get("codey_remote_compaction")
            .and_then(Value::as_bool)
            != Some(enabled)
        {
            // Codex 忽略扩展字段，Codey 的原生请求入口用它选择会话 Provider。
            model["codey_remote_compaction"] = json!(enabled);
            changed = true;
        }
    }
    changed
}

pub(super) fn gate_synthetic_native_web_search(
    model: &mut Value,
    allowed_model_keys: &HashSet<String>,
) {
    if model.get("codey_source").and_then(Value::as_str) != Some("third_party") {
        return;
    }
    gate_cached_native_web_search(model, allowed_model_keys);
}

fn gate_cached_native_web_search(model: &mut Value, allowed_model_keys: &HashSet<String>) -> bool {
    let allowed = model
        .get("slug")
        .and_then(Value::as_str)
        .is_some_and(|slug| allowed_model_keys.contains(&model_id::key(slug)));
    let source_declares_search = model.get("supports_search_tool").and_then(Value::as_bool)
        == Some(true)
        && model
            .get("web_search_tool_type")
            .and_then(Value::as_str)
            .is_some_and(|tool_type| !tool_type.trim().is_empty());
    if allowed && source_declares_search {
        return false;
    }
    let mut changed = false;
    if let Some(object) = model.as_object_mut() {
        changed |= object.remove("supports_search_tool").is_some();
        changed |= object.remove("web_search_tool_type").is_some();
    }
    changed
}

/// Chat Completions 与 Anthropic Messages 都没有 `input_image.detail=original`
/// 的对应字段，Codex 只会按运行时目录里声明的能力决定是否发出这个值。适配
/// 线路上必须清掉该声明，否则历史里已经存在的原图请求会被本地路由拒绝。
pub(super) fn gate_cached_image_detail_original(
    model: &mut Value,
    allowed_model_keys: &HashSet<String>,
) -> bool {
    let allowed = model
        .get("slug")
        .and_then(Value::as_str)
        .is_some_and(|slug| allowed_model_keys.contains(&model_id::key(slug)));
    if allowed {
        return false;
    }
    model
        .as_object_mut()
        .is_some_and(|object| object.remove("supports_image_detail_original").is_some())
}
