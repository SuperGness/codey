use super::*;

pub(super) const MODEL_CATALOG_RELATIVE_PATH: &str = "model-catalogs/codey-official.json";

/// 同一次操作复用已验证的运行时来源；后续操作重新检查安装切换和程序更新。
pub(crate) struct CatalogContext<'a> {
    codex_app_path: &'a str,
    runtime: std::sync::OnceLock<(Vec<PathBuf>, Option<RuntimeCatalogSource>)>,
}

impl<'a> CatalogContext<'a> {
    pub(crate) fn new(codex_app_path: &'a str) -> Self {
        Self {
            codex_app_path,
            runtime: std::sync::OnceLock::new(),
        }
    }

    fn runtime(&self) -> &(Vec<PathBuf>, Option<RuntimeCatalogSource>) {
        self.runtime.get_or_init(|| {
            #[cfg(not(test))]
            {
                let candidates = codex_cli_candidates_for(self.codex_app_path);
                let source = runtime_catalog_source(&candidates);
                (candidates, source)
            }
            #[cfg(test)]
            {
                record_catalog_resolution();
                let source = catalog_source_for_app(self.codex_app_path);
                let candidates = source.iter().map(|source| source.path.clone()).collect();
                (candidates, source)
            }
        })
    }

    pub(super) fn source(&self) -> Option<&RuntimeCatalogSource> {
        self.runtime().1.as_ref()
    }

    pub(super) fn candidates(&self) -> &[PathBuf] {
        &self.runtime().0
    }
}

#[cfg(test)]
thread_local! {
    static CATALOG_OPERATION_COUNTS: std::cell::Cell<(usize, usize)> = const {
        std::cell::Cell::new((0, 0))
    };
}

#[cfg(test)]
pub(super) fn catalog_operation_counts() -> (usize, usize) {
    CATALOG_OPERATION_COUNTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn record_catalog_resolution() {
    CATALOG_OPERATION_COUNTS.with(|counts| {
        let (resolutions, probes) = counts.get();
        counts.set((resolutions + 1, probes));
    });
}

#[cfg(test)]
pub(super) fn record_snapshot_probe() {
    CATALOG_OPERATION_COUNTS.with(|counts| {
        let (resolutions, probes) = counts.get();
        counts.set((resolutions, probes + 1));
    });
}

/// Only exact catalog IDs carry route capabilities. A raw model name may exist
/// on several routes and must never inherit capability from a matching suffix.
pub(crate) fn remote_compaction_models_from_catalog(path: &Path) -> HashSet<String> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|catalog| catalog.get("models").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter(|model| {
            model
                .get("codey_remote_compaction")
                .and_then(Value::as_bool)
                == Some(true)
        })
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(model_id::key)
        .collect()
}
/// Raw `codex debug models` output Codey captured itself. Recent Codex builds
/// no longer maintain `models_cache.json` on disk, so this snapshot is the
/// durable source for models that need instruction-bearing entries.
pub(super) const DEBUG_CATALOG_RELATIVE_PATH: &str = "model-catalogs/codey-runtime-catalog.json";

pub fn relative_path() -> &'static str {
    MODEL_CATALOG_RELATIVE_PATH
}

#[derive(Debug)]
pub(crate) struct CatalogSnapshot {
    path: PathBuf,
    contents: Option<Vec<u8>>,
}

pub(crate) fn snapshot(home: &Path) -> Result<CatalogSnapshot> {
    let path = home.join(relative_path());
    let contents = match fs::read(&path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取现有 Codey 模型目录失败：{}", path.display()));
        }
    };
    Ok(CatalogSnapshot { path, contents })
}

pub(crate) fn restore_snapshot(snapshot: CatalogSnapshot) -> Result<()> {
    match snapshot.contents {
        Some(contents) => atomic_write(&snapshot.path, &contents),
        None => crate::fs_util::remove_file_if_exists(&snapshot.path)
            .with_context(|| format!("移除新建的 Codey 模型目录失败：{}", snapshot.path.display())),
    }
}

pub fn is_available(home: &Path) -> bool {
    read_catalog_value(&home.join(relative_path())).is_some_and(|value| {
        let models = catalog_models_from_value(&value);
        runtime_compatible_models(&models)
    })
}

/// Signature of the catalog source files, used to reuse a parse across the
/// back-to-back `refresh_for_provider` + `selection_state` calls on every
/// launch and across repeated config-page lookups. The paths are part of the
/// key so entries can never leak between Codex homes.
type CatalogSignature = Vec<(PathBuf, u64, Option<std::time::SystemTime>)>;
type OfficialEntriesCache = std::sync::Mutex<
    Option<(
        CatalogSignature,
        Option<RuntimeCatalogSource>,
        std::sync::Arc<Vec<Value>>,
    )>,
>;

static OFFICIAL_ENTRIES_CACHE: std::sync::OnceLock<OfficialEntriesCache> =
    std::sync::OnceLock::new();

fn catalog_signature(paths: &[PathBuf]) -> CatalogSignature {
    paths
        .iter()
        .map(|path| match fs::metadata(path) {
            Ok(metadata) => (path.clone(), metadata.len(), metadata.modified().ok()),
            Err(_) => (path.clone(), 0, None),
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn write_test_runtime_catalog(home: &Path, mut catalog: Value, account_snapshot: bool) {
    catalog["codey_runtime_source"] = json!(catalog_source_for_app(""));
    if account_snapshot {
        catalog["codey_account_snapshot"] = json!(true);
    }
    let path = home.join(DEBUG_CATALOG_RELATIVE_PATH);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
}

#[cfg(test)]
pub(super) fn read_official_entries(
    home: &Path,
    codex_app_path: &str,
) -> Result<std::sync::Arc<Vec<Value>>> {
    read_official_entries_in_context(home, &CatalogContext::new(codex_app_path))
}

pub(super) fn read_official_entries_in_context(
    home: &Path,
    context: &CatalogContext<'_>,
) -> Result<std::sync::Arc<Vec<Value>>> {
    let source = context.source();
    let paths = ordered_catalog_sources(home);
    let signature = catalog_signature(&paths);
    let cache = OFFICIAL_ENTRIES_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((cached_signature, cached_source, entries)) = guard.as_ref()
        && *cached_signature == signature
        && cached_source.as_ref() == source
    {
        // 缓存命中只递增引用计数；下游要么只读、要么本来就会拷贝出自己的
        // 工作副本，无需整目录深拷贝。
        return Ok(std::sync::Arc::clone(entries));
    }
    let entries = std::sync::Arc::new(read_official_entries_uncached(&paths, source)?);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((signature, source.cloned(), std::sync::Arc::clone(&entries)));
    }
    Ok(entries)
}

/// Codey-owned runtime catalog sources: raw CLI/account snapshot first, then
/// the rendered catalog for instruction template recovery.
fn ordered_catalog_sources(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(DEBUG_CATALOG_RELATIVE_PATH),
        home.join(relative_path()),
    ]
}

fn fallback_official_metadata_entry(slug: &str, display_name: &str, priority: usize) -> Value {
    let (default_effort, efforts): (&str, &[&str]) = match slug {
        "gpt-5.6-sol" => ("low", &["low", "medium", "high", "xhigh", "max", "ultra"]),
        "gpt-5.5" => ("medium", &["low", "medium", "high", "xhigh"]),
        _ => (
            "medium",
            &["low", "medium", "high", "xhigh", "max", "ultra"],
        ),
    };
    let supported_reasoning_levels = efforts
        .iter()
        .map(|effort| json!({ "effort": effort }))
        .collect::<Vec<_>>();
    let mut entry = json!({
        "slug": slug,
        "display_name": display_name,
        "visibility": "list",
        "supported_in_api": true,
        "default_reasoning_level": default_effort,
        "supported_reasoning_levels": supported_reasoning_levels,
    });
    normalize_official_model(&mut entry, slug, display_name, priority);
    entry
}

fn read_official_entries_uncached(
    paths: &[PathBuf],
    source: Option<&RuntimeCatalogSource>,
) -> Result<Vec<Value>> {
    let mut catalogs = Vec::new();
    let mut account_snapshot_index = None;
    for path in paths {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => continue,
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if !snapshot_matches_runtime_source(&value, source) {
            continue;
        }
        let account_snapshot = path.ends_with(DEBUG_CATALOG_RELATIVE_PATH)
            && value
                .get("codey_account_snapshot")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let mut models = official_models_from_value(&value);
        if path.ends_with(MODEL_CATALOG_RELATIVE_PATH) {
            // 生成目录可能携带线路速度覆盖，只能用它恢复指令等模板字段。
            for model in &mut models {
                clear_template_speed_tiers(model);
            }
        } else {
            for model in &mut models {
                if model.get("codey_context_source").is_none()
                    && declared_context_capacity(model).is_some()
                {
                    model["codey_context_source"] = json!("official_catalog");
                }
            }
        }
        if !models.is_empty() {
            if account_snapshot {
                account_snapshot_index = Some(catalogs.len());
            }
            catalogs.push(models);
        }
    }

    // A Codey-owned CLI snapshot is account-backed and therefore the source
    // of truth. Fill missing instruction templates in-place from the other
    // local catalog entries without deep-cloning every catalog.
    if let Some(index) = account_snapshot_index {
        let mut account_models = catalogs.swap_remove(index);
        complete_account_models_in_place(&mut account_models, &catalogs);
        if snapshot_has_runtime_models(&account_models) {
            let mut seen = HashSet::new();
            let models = account_models
                .into_iter()
                .filter(|model| {
                    model_instruction_source(model).is_some()
                        && model
                            .get("visibility")
                            .and_then(Value::as_str)
                            .is_none_or(|visibility| visibility != "hide")
                })
                .filter_map(|mut model| {
                    let slug = model
                        .get("slug")
                        .and_then(Value::as_str)?
                        .trim()
                        .to_string();
                    if slug.is_empty() || !seen.insert(model_id::key(&slug)) {
                        return None;
                    }
                    let display_name = model
                        .get("display_name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&slug)
                        .to_string();
                    normalize_official_model(&mut model, &slug, &display_name, seen.len() - 1);
                    Some(model)
                })
                .collect::<Vec<_>>();
            if !models.is_empty() {
                return Ok(models);
            }
        } else {
            catalogs.insert(index, account_models);
        }
    }

    Ok(OFFICIAL_MODELS
        .iter()
        .enumerate()
        .map(|(priority, (slug, display_name))| {
            let matching_models = catalogs
                .iter()
                .flat_map(|models| models.iter())
                .filter(|model| model.get("slug").and_then(Value::as_str) == Some(*slug))
                .collect::<Vec<_>>();
            let Some(mut model) = matching_models
                .iter()
                .find(|model| model_instruction_source(model).is_some())
                .or_else(|| matching_models.first())
                .map(|model| (*model).clone())
            else {
                return fallback_official_metadata_entry(slug, display_name, priority);
            };
            let fallbacks = matching_models;
            complete_reasoning_metadata(&mut model, &fallbacks);
            normalize_official_model(&mut model, slug, display_name, priority);
            model
        })
        .collect())
}

/// Writes the catalog and returns the exact bytes that now live on disk.
pub(super) fn write_catalog(home: &Path, models: &[Value]) -> Result<Vec<u8>> {
    let source = read_catalog_value(&home.join(relative_path()))
        .and_then(|value| value.get("codey_runtime_source").cloned());
    write_catalog_with_source(home, models, source)
}

pub(super) fn write_catalog_with_source(
    home: &Path,
    models: &[Value],
    source: Option<Value>,
) -> Result<Vec<u8>> {
    let mut value = json!({ "models": models });
    if let Some(source) = source {
        value["codey_runtime_source"] = source;
    }
    let mut catalog = serde_json::to_vec_pretty(&value).context("序列化 Codey 模型目录失败")?;
    catalog.push(b'\n');
    let path = home.join(relative_path());
    if fs::read(&path).is_ok_and(|current| current == catalog) {
        protect_catalog_file(&path)?;
        return Ok(catalog);
    }
    atomic_write(&path, &catalog)?;
    Ok(catalog)
}

pub(super) fn write_verified_catalog(home: &Path, models: &[Value]) -> Result<usize> {
    let written = write_catalog(home, models)?;
    verify_written_catalog(home, models, &written)
}

pub(super) fn write_verified_catalog_in_context(
    home: &Path,
    models: &[Value],
    context: &CatalogContext<'_>,
) -> Result<usize> {
    let source = context.source().map(serde_json::to_value).transpose()?;
    let written = write_catalog_with_source(home, models, source)?;
    verify_written_catalog(home, models, &written)
}

pub(super) fn verify_written_catalog(
    home: &Path,
    models: &[Value],
    written: &[u8],
) -> Result<usize> {
    // Verify the bytes on disk instead of re-parsing the whole catalog; the
    // serialized form already carries every slug in order.
    let path = home.join(relative_path());
    let on_disk = fs::read(&path)
        .with_context(|| format!("读取 Codey 运行时模型目录失败：{}", path.display()))?;
    if on_disk != written {
        bail!("写入后的 Codey 模型目录与本次生成结果不一致");
    }
    Ok(models.len())
}

pub(super) fn read_catalog_value(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

pub(super) fn read_runtime_catalog_models(home: &Path) -> Result<Vec<Value>> {
    let path = home.join(relative_path());
    read_runtime_catalog_models_at(&path)
}

pub(super) fn read_runtime_catalog_models_at(path: &Path) -> Result<Vec<Value>> {
    let bytes = fs::read(path)
        .with_context(|| format!("读取 Codey 运行时模型目录失败：{}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Codey 运行时模型目录失败：{}", path.display()))?;
    if value.get("models").and_then(Value::as_array).is_none() {
        bail!("Codey 运行时模型目录缺少 models 数组");
    }
    let models = catalog_models_from_value(&value);
    if !models.is_empty() && !runtime_compatible_models(&models) {
        bail!("Codey 运行时模型目录缺少 Codex 必需字段");
    }
    Ok(models)
}

pub(super) fn protect_catalog_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("保护本地模型目录失败：{}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
