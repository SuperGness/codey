use super::*;
use serde_json::Value as JsonValue;

const KEYS: [&str; 2] = ["model_context_window", "model_auto_compact_token_limit"];

/// 只记录被暂时移除的预算字段，不保存 Provider 或凭据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ContextBudgetLease {
    removed: String,
}

fn remove_fields(document: &mut DocumentMut) -> DocumentMut {
    let mut removed = DocumentMut::new();
    for key in KEYS {
        if let Some(item) = document.as_table_mut().remove(key) {
            removed[key] = item;
        }
    }
    if let Some(profiles) = document
        .get_mut("profiles")
        .and_then(Item::as_table_like_mut)
    {
        for (name, profile) in profiles.iter_mut() {
            if let Some(profile) = profile.as_table_like_mut() {
                for key in KEYS {
                    if let Some(item) = profile.remove(key) {
                        removed["profiles"][name.get()][key] = item;
                    }
                }
            }
        }
    }
    removed
}

fn effective_budget(document: &DocumentMut, key: &str) -> Option<i64> {
    document
        .get("profile")
        .and_then(Item::as_str)
        .and_then(|name| document.get("profiles")?.get(name)?.get(key))
        .or_else(|| document.get(key))
        .and_then(Item::as_integer)
}

pub(super) fn prepare(
    home: &Path,
    persistent: &DocumentMut,
    effective: &mut DocumentMut,
    contexts: Option<&BTreeMap<String, crate::config::ModelContextConfig>>,
) -> Result<Option<ContextBudgetLease>> {
    let Some(contexts) = contexts.filter(|contexts| !contexts.is_empty()) else {
        return Ok(None);
    };
    let mut cleaned = persistent.clone();
    let removed = remove_fields(&mut cleaned);
    if removed.is_empty() {
        return Ok(None);
    }
    let source = resolved_model_catalog_path(effective, home)
        .context("自定义上下文预算缺少运行时模型目录")?;
    let mut catalog: JsonValue = serde_json::from_slice(&fs::read(&source)?)?;
    let models = catalog
        .get_mut("models")
        .and_then(JsonValue::as_array_mut)
        .context("上下文预算目录缺少 models 数组")?;
    let window = effective_budget(persistent, KEYS[0]);
    let threshold = effective_budget(persistent, KEYS[1]);
    for model in models {
        let slug = model
            .get("slug")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if contexts.keys().any(|key| crate::model_id::equal(key, slug)) {
            continue;
        }
        // Codex 原本会先加载目录，再按全局配置覆盖；在副本中保留同样的结果。
        if let Some(window) = window {
            let window = model
                .get("max_context_window")
                .and_then(JsonValue::as_i64)
                .map_or(window, |maximum| window.min(maximum));
            model["context_window"] = window.into();
        }
        if let Some(threshold) = threshold {
            model["auto_compact_token_limit"] = threshold.into();
        }
    }
    let bytes = serde_json::to_vec_pretty(&catalog)?;
    let path = home
        .join("model-catalogs/context-overrides")
        .join(format!("{}.json", crate::fs_util::sha256_hex(&bytes)));
    atomic_write(&path, &bytes)?;
    effective["model_catalog_json"] = value(path.to_string_lossy().into_owned());
    remove_fields(effective);
    Ok(Some(ContextBudgetLease {
        removed: removed.to_string(),
    }))
}

impl ContextBudgetLease {
    fn marker(&self) -> String {
        format!(
            "# Codey context budget lease: {}\n",
            crate::fs_util::sha256_hex(self.removed.as_bytes())
        )
    }

    pub(super) fn apply(&self, home: &Path, original: &[u8]) -> Result<()> {
        let manager = ConfigManager::new(home.join("config.toml"));
        let snapshot = manager.load()?;
        anyhow::ensure!(
            snapshot.raw() == original,
            "Codex 配置已变化，取消应用上下文预算，请重试"
        );
        let mut document = snapshot.document().clone();
        remove_fields(&mut document);
        let trailing = document.trailing().as_str().unwrap_or_default().to_string();
        let rendered = document.to_string();
        let separator = if rendered.is_empty() || rendered.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        document.set_trailing(format!("{trailing}{separator}{}", self.marker()));
        manager.replace_document(
            Some(snapshot.revision()),
            document,
            "temporarily use per-model context budgets",
            "codex_config.context_budget.apply",
        )?;
        Ok(())
    }

    pub(super) fn restore(&self, home: &Path) -> Result<()> {
        let manager = ConfigManager::new(home.join("config.toml"));
        let snapshot = manager.load()?;
        if !snapshot.exists() {
            return Ok(());
        }
        let saved = parse_document(&self.removed)?;
        let raw = std::str::from_utf8(snapshot.raw())?;
        let marker = self.marker();
        // 租约先落盘、配置后提交；若尚未提交就崩溃，不恢复未曾移除的字段。
        if !raw.lines().any(|line| line == marker.trim_end()) {
            return Ok(());
        }
        let cleaned = raw
            .split_inclusive('\n')
            .filter(|line| line.trim_end_matches(['\r', '\n']) != marker.trim_end())
            .collect::<String>();
        let mut document = parse_document(&cleaned)?;
        for key in KEYS {
            if !document.contains_key(key)
                && let Some(item) = saved.get(key)
            {
                document[key] = item.clone();
            }
        }
        if let Some(profiles) = saved.get("profiles").and_then(Item::as_table_like) {
            for (name, profile) in profiles.iter() {
                // 用户删除了 profile 时不重新创建它。
                if let Some(current) = document
                    .get_mut("profiles")
                    .and_then(|profiles| profiles.get_mut(name))
                    .and_then(Item::as_table_like_mut)
                {
                    for key in KEYS {
                        if !current.contains_key(key)
                            && let Some(item) = profile.get(key)
                        {
                            current.insert(key, item.clone());
                        }
                    }
                }
            }
        }
        if document.to_string().as_bytes() != snapshot.raw() {
            manager.replace_document(
                Some(snapshot.revision()),
                document,
                "restore global context settings without replacing user edits",
                "codex_config.context_budget.restore",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(
        home: &Path,
        config: &str,
    ) -> (
        DocumentMut,
        BTreeMap<String, crate::config::ModelContextConfig>,
    ) {
        fs::write(home.join("config.toml"), config).unwrap();
        fs::write(home.join("catalog.json"), serde_json::to_vec(&json!({"models":[
            {"slug":"custom", "context_window":1000000, "max_context_window":1000000, "auto_compact_token_limit":800000},
            {"slug":"other", "context_window":272000, "max_context_window":872000, "auto_compact_token_limit":240000}
        ]})).unwrap()).unwrap();
        let policies = BTreeMap::from([(
            "custom".into(),
            crate::config::ModelContextConfig {
                context_window_tokens: 1000000,
                auto_compact_token_limit: Some(800000),
                reserve_output_tokens: None,
            },
        )]);
        (parse_document(config).unwrap(), policies)
    }

    #[test]
    fn context_budget_preserves_other_models_and_restores_global_config() {
        let temp = tempfile::tempdir().unwrap();
        let config = "model_catalog_json='catalog.json'\nmodel_context_window=372000\nmodel_auto_compact_token_limit=300000\n";
        let (persistent, policies) = fixture(temp.path(), config);
        let mut effective = persistent.clone();
        let lease = prepare(temp.path(), &persistent, &mut effective, Some(&policies))
            .unwrap()
            .unwrap();
        let catalog: JsonValue = serde_json::from_slice(
            &fs::read(effective["model_catalog_json"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog["models"][0]["context_window"], 1000000);
        assert_eq!(catalog["models"][0]["auto_compact_token_limit"], 800000);
        assert_eq!(catalog["models"][1]["context_window"], 372000);
        assert_eq!(catalog["models"][1]["auto_compact_token_limit"], 300000);
        assert_eq!(
            fs::read_to_string(temp.path().join("config.toml")).unwrap(),
            config
        );
        lease.apply(temp.path(), config.as_bytes()).unwrap();
        let applied = read_codex_config_document(&temp.path().join("config.toml")).unwrap();
        assert!(!applied.contains_key(KEYS[0]));
        assert!(!applied.contains_key(KEYS[1]));
        lease.restore(temp.path()).unwrap();
        let restored = read_codex_config_document(&temp.path().join("config.toml")).unwrap();
        assert_eq!(restored[KEYS[0]].as_integer(), Some(372000));
        assert_eq!(restored[KEYS[1]].as_integer(), Some(300000));
        lease.restore(temp.path()).unwrap();
        assert!(
            prepare(
                temp.path(),
                &restored,
                &mut restored.clone(),
                Some(&BTreeMap::new())
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn context_budget_recovery_preserves_concurrent_edits_and_deleted_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let config = "model_catalog_json='catalog.json'\nmodel_context_window=372000\nmodel_auto_compact_token_limit=300000\nprofile='work'\n[profiles.work]\nmodel_context_window=420000\n";
        let (persistent, policies) = fixture(temp.path(), config);
        let mut effective = persistent.clone();
        let lease = prepare(temp.path(), &persistent, &mut effective, Some(&policies))
            .unwrap()
            .unwrap();
        let catalog: JsonValue = serde_json::from_slice(
            &fs::read(effective["model_catalog_json"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog["models"][1]["context_window"], 420000);
        lease.apply(temp.path(), config.as_bytes()).unwrap();
        let path = temp.path().join("config.toml");
        let mut concurrent = read_codex_config_document(&path).unwrap();
        concurrent[KEYS[0]] = value(500000);
        concurrent["model"] = value("user-selected");
        concurrent.as_table_mut().remove("profiles");
        fs::write(&path, concurrent.to_string()).unwrap();
        lease.restore(temp.path()).unwrap();
        let restored = read_codex_config_document(&path).unwrap();
        assert_eq!(restored[KEYS[0]].as_integer(), Some(500000));
        assert_eq!(restored[KEYS[1]].as_integer(), Some(300000));
        assert_eq!(restored["model"].as_str(), Some("user-selected"));
        assert!(!restored.contains_key("profiles"));
    }

    #[test]
    fn context_budget_failed_apply_does_not_restore_uncommitted_fields() {
        let temp = tempfile::tempdir().unwrap();
        let config = "model_catalog_json='catalog.json'\nmodel_context_window=372000\n";
        let (persistent, policies) = fixture(temp.path(), config);
        let lease = prepare(
            temp.path(),
            &persistent,
            &mut persistent.clone(),
            Some(&policies),
        )
        .unwrap()
        .unwrap();
        let concurrent = "model_catalog_json='catalog.json'\nmodel='changed'\n";
        fs::write(temp.path().join("config.toml"), concurrent).unwrap();
        assert!(lease.apply(temp.path(), config.as_bytes()).is_err());
        lease.restore(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join("config.toml")).unwrap(),
            concurrent
        );
    }
}
