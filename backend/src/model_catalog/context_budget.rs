use super::*;
use crate::codex_config::GlobalContextBudget;
use crate::config::CodeyConfig;

struct LaunchBudget {
    fields: serde_json::Map<String, Value>,
    follows_official_catalog: bool,
    global: GlobalContextBudget,
}

/// 按线路和上游模型保存 app-server 启动时使用的预算。同步目录可以改变模型别名。
#[derive(Default)]
pub(crate) struct RuntimeContextBudgets {
    models: BTreeMap<(String, String), LaunchBudget>,
}

impl RuntimeContextBudgets {
    pub(crate) fn from_catalog(
        home: &Path,
        config: &CodeyConfig,
        catalog: Option<&Path>,
        global: GlobalContextBudget,
    ) -> Result<Self> {
        let models = match catalog {
            Some(path) => Arc::new(read_context_fallback_models(path)?),
            None => read_official_entries_in_context(
                home,
                &CatalogContext::new(&config.codex_app_path),
            )?,
        };
        let by_slug: BTreeMap<_, _> = models
            .iter()
            .filter_map(|model| Some((model_id::key(model["slug"].as_str()?), model)))
            .collect();
        let shared_path = home.join(relative_path());
        let uses_shared = catalog.is_none_or(|path| path == shared_path);
        let explicit: HashSet<_> = config
            .runtime_enabled_model_contexts()
            .into_keys()
            .map(|model| model_id::key(&model))
            .collect();
        let mut budgets = Self::default();
        for target in config.runtime_model_targets() {
            let alias = model_id::key(&config.runtime_catalog_id_for_target(&target));
            let model = by_slug.get(&alias).copied();
            // 内置目录按 Codex 的原始声明运行。自备目录只跟随从共享目录补入的模型。
            let declaration = model.map(|model| match catalog {
                None => model.get("codey_context_base").unwrap_or(model),
                Some(_) => model,
            });
            let mut fields = CONTEXT_FIELDS
                .into_iter()
                .filter_map(|field| Some((field.to_owned(), declaration?.get(field)?.clone())))
                .collect();
            let global = if explicit.contains(&alias) {
                GlobalContextBudget::default()
            } else {
                global
            };
            global.apply(&mut fields);
            let follows_official_catalog = target.official
                && (uses_shared
                    || model
                        .is_some_and(|model| model["codey_context_from_runtime_catalog"] == true));
            budgets.models.insert(
                (
                    model_id::key(&target.provider_id),
                    model_id::key(&target.upstream_model),
                ),
                LaunchBudget {
                    fields,
                    follows_official_catalog,
                    global,
                },
            );
        }
        Ok(budgets)
    }

    /// 给页面写回启动预算，同时判断这次目录同步是否需要重启。
    pub(crate) fn pin_renderer_catalog(&self, catalog: &mut Value) -> bool {
        let Some(entries) = catalog["model_metadata"].as_array_mut() else {
            return false;
        };
        let mut changed = false;
        for entry in entries {
            let (Some(provider), Some(model)) = (
                entry["route_provider_id"].as_str(),
                entry["upstream_model"].as_str(),
            ) else {
                continue;
            };
            let Some(applied) = self
                .models
                .get(&(model_id::key(provider), model_id::key(model)))
            else {
                continue;
            };
            let metadata = entry.as_object_mut().unwrap();
            applied.global.apply(metadata);
            if applied.follows_official_catalog
                && metadata.get("codey_context_source").and_then(Value::as_str)
                    == Some("official_catalog")
                && effective_budget(&applied.fields) != effective_budget(metadata)
            {
                changed = true;
            }
            for field in CONTEXT_FIELDS {
                match applied.fields.get(field) {
                    Some(value) => {
                        metadata.insert(field.to_owned(), value.clone());
                    }
                    None => {
                        metadata.remove(field);
                    }
                }
            }
        }
        changed
    }
}

fn effective_budget(fields: &serde_json::Map<String, Value>) -> (Option<u64>, u64, Option<u64>) {
    let number = |key| fields.get(key).and_then(Value::as_u64);
    let window = number("context_window");
    let percent = number("effective_context_window_percent")
        .unwrap_or(DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT)
        .min(100);
    let threshold = number("auto_compact_token_limit")
        .or_else(|| window.map(|window| default_compact_limit(window, percent)));
    (window, percent, threshold)
}
