use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

pub const MODEL_CATALOG_RELATIVE_PATH: &str = "model-catalogs/codey-official.json";
const ALLOWED_REASONING_EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];
const FAST_SERVICE_TIER_ID: &str = "priority";
const FAST_SPEED_TIER_ID: &str = "fast";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialModel {
    pub slug: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialModelAvailability {
    pub slug: String,
    pub display_name: String,
    pub supported: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectionState {
    pub official_models: Vec<OfficialModelAvailability>,
    pub official_model_ids: Vec<String>,
    pub third_party_models: Vec<String>,
    pub upstream_models: Vec<String>,
}

pub fn relative_path() -> &'static str {
    MODEL_CATALOG_RELATIVE_PATH
}

pub fn refresh_for_provider(
    home: &Path,
    official_provider: bool,
    upstream_models: &[String],
    selected_models: &[String],
) -> Result<usize> {
    let official_models = read_official_entries(home)?;
    let official_slugs = official_models
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect::<HashSet<_>>();
    let upstream = upstream_models
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut catalog_models = if official_provider {
        official_models.clone()
    } else {
        official_models
            .iter()
            .filter(|model| is_provider_selectable_official(model))
            .filter(|model| {
                model
                    .get("slug")
                    .and_then(Value::as_str)
                    .is_some_and(|slug| upstream.contains(slug))
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    for model in &mut catalog_models {
        if !official_provider {
            expose_provider_supported_official(model);
        }
        ensure_catalog_compatibility(model);
        expose_supported_model(model);
        ensure_speed_controls(model);
    }

    if !official_provider {
        let template = official_models
            .iter()
            .find(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
            .or_else(|| official_models.first())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("官方账号模型缓存为空，请先使用官方账号启动一次 Codex")
            })?;
        let mut seen = HashSet::new();
        for (index, model_id) in selected_models.iter().enumerate() {
            let model_id = model_id.trim();
            if model_id.is_empty()
                || official_slugs.contains(model_id)
                || !upstream.contains(model_id)
                || !seen.insert(model_id.to_string())
            {
                continue;
            }
            catalog_models.push(synthetic_model(&template, model_id, index));
        }
    }

    if catalog_models.is_empty() {
        bail!("当前 provider 没有可写入 Codex 模型目录的模型，保留上一份合法目录");
    }
    write_catalog(home, &catalog_models)?;
    Ok(catalog_models.len())
}

pub fn selection_state(
    home: &Path,
    official_provider: bool,
    upstream_models: &[String],
    selected_models: &[String],
) -> Result<ModelSelectionState> {
    let official_entries = read_official_entries(home)?;
    let official_model_ids = official_entries
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let official_slugs = official_model_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let upstream = upstream_models
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let official_models = official_entries
        .into_iter()
        .filter(|model| {
            model.get("visibility").and_then(Value::as_str) == Some("list")
                || (!official_provider
                    && is_provider_selectable_official(model)
                    && model
                        .get("slug")
                        .and_then(Value::as_str)
                        .is_some_and(|slug| upstream.contains(slug)))
        })
        .filter_map(|model| {
            let model = official_model_from_value(&model)?;
            let supported = official_provider || upstream.contains(model.slug.as_str());
            Some(OfficialModelAvailability {
                slug: model.slug,
                display_name: model.display_name,
                supported,
            })
        })
        .collect();
    let third_party_models = if official_provider {
        Vec::new()
    } else {
        selected_models
            .iter()
            .map(|model| model.trim())
            .filter(|model| {
                !model.is_empty() && upstream.contains(*model) && !official_slugs.contains(*model)
            })
            .fold(Vec::<String>::new(), |mut models, model| {
                if !models.iter().any(|existing| existing == model) {
                    models.push(model.to_string());
                }
                models
            })
    };
    Ok(ModelSelectionState {
        official_models,
        official_model_ids,
        third_party_models,
        upstream_models: if official_provider {
            Vec::new()
        } else {
            upstream_models.to_vec()
        },
    })
}

pub fn official_model_slugs(home: &Path) -> Result<HashSet<String>> {
    Ok(read_official_entries(home)?
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect())
}

pub fn is_available(home: &Path) -> bool {
    read_catalog_value(&home.join(relative_path()))
        .is_some_and(|value| !catalog_models_from_value(&value).is_empty())
}

fn read_official_entries(home: &Path) -> Result<Vec<Value>> {
    let paths = [home.join("models_cache.json"), home.join(relative_path())];
    let mut last_error = None;
    for path in paths {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let value = match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => value,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let models = official_models_from_value(&value);
        if !models.is_empty() {
            return Ok(models);
        }
    }
    if let Some(value) = codex_plus_core::model_suffix::bundled_model_catalog() {
        let models = official_models_from_value(&value);
        if !models.is_empty() {
            return Ok(models);
        }
    }
    bail!(
        "{}",
        last_error.unwrap_or_else(|| "找不到可用的 Codex 模型模板".to_string())
    )
}

fn official_model_from_value(model: &Value) -> Option<OfficialModel> {
    let slug = model.get("slug")?.as_str()?.trim();
    if slug.is_empty() {
        return None;
    }
    let display_name = model
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(slug);
    Some(OfficialModel {
        slug: slug.to_string(),
        display_name: display_name.to_string(),
    })
}

fn official_models_from_value(value: &Value) -> Vec<Value> {
    catalog_models_from_value(value)
        .into_iter()
        .filter(|model| model.get("codey_source").and_then(Value::as_str) != Some("third_party"))
        .collect()
}

fn catalog_models_from_value(value: &Value) -> Vec<Value> {
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    models
        .iter()
        .filter_map(|model| {
            let slug = model.get("slug")?.as_str()?.trim();
            if slug.is_empty() || !model.is_object() || !seen.insert(slug.to_string()) {
                return None;
            }
            let mut model = model.clone();
            model["slug"] = json!(slug);
            Some(model)
        })
        .collect()
}

fn clamp_reasoning_efforts(model: &mut Value) {
    if let Some(levels) = model
        .get_mut("supported_reasoning_levels")
        .and_then(Value::as_array_mut)
    {
        levels.retain(|level| {
            level
                .get("effort")
                .and_then(Value::as_str)
                .is_some_and(|effort| ALLOWED_REASONING_EFFORTS.contains(&effort))
        });
    }
    let default = model
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !ALLOWED_REASONING_EFFORTS.contains(&default) {
        model["default_reasoning_level"] = json!("xhigh");
    }
}

fn ensure_catalog_compatibility(model: &mut Value) {
    if !model
        .get("supports_reasoning_summaries")
        .is_some_and(Value::is_boolean)
    {
        let supports_reasoning_summaries = model
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some_and(|levels| !levels.is_empty());
        model["supports_reasoning_summaries"] = json!(supports_reasoning_summaries);
    }
}

fn is_provider_selectable_official(model: &Value) -> bool {
    model.get("visibility").and_then(Value::as_str) == Some("list")
        || model.get("upgrade").is_some_and(Value::is_object)
}

fn expose_provider_supported_official(model: &mut Value) {
    if model.get("visibility").and_then(Value::as_str) == Some("list") {
        return;
    }
    model["visibility"] = json!("list");
    if let Some(object) = model.as_object_mut() {
        object.remove("availability_nux");
        object.remove("upgrade");
    }
}

fn expose_supported_model(model: &mut Value) {
    if model.get("visibility").and_then(Value::as_str) == Some("list") {
        model["supported_in_api"] = json!(true);
    }
}

fn ensure_speed_controls(model: &mut Value) {
    let service_tiers = model.get_mut("service_tiers").and_then(Value::as_array_mut);
    if let Some(service_tiers) = service_tiers {
        if !service_tiers
            .iter()
            .any(|tier| tier.get("id").and_then(Value::as_str) == Some(FAST_SERVICE_TIER_ID))
        {
            service_tiers.push(json!({
                "id": FAST_SERVICE_TIER_ID,
                "name": "Fast",
                "description": "1.5x speed, increased usage"
            }));
        }
    } else {
        model["service_tiers"] = json!([{
            "id": FAST_SERVICE_TIER_ID,
            "name": "Fast",
            "description": "1.5x speed, increased usage"
        }]);
    }

    let speed_tiers = model
        .get_mut("additional_speed_tiers")
        .and_then(Value::as_array_mut);
    if let Some(speed_tiers) = speed_tiers {
        if !speed_tiers
            .iter()
            .any(|tier| tier.as_str() == Some(FAST_SPEED_TIER_ID))
        {
            speed_tiers.push(json!(FAST_SPEED_TIER_ID));
        }
    } else {
        model["additional_speed_tiers"] = json!([FAST_SPEED_TIER_ID]);
    }
}

fn synthetic_model(template: &Value, model_id: &str, index: usize) -> Value {
    let mut model = template.clone();
    model["slug"] = json!(model_id);
    model["display_name"] = json!(model_id);
    model["description"] = json!("Third-party API model");
    model["visibility"] = json!("list");
    model["priority"] = json!(1000 + index);
    model["supported_in_api"] = json!(true);
    model["codey_source"] = json!("third_party");
    if let Some(object) = model.as_object_mut() {
        object.remove("availability_nux");
        object.remove("upgrade");
    }
    ensure_catalog_compatibility(&mut model);
    clamp_reasoning_efforts(&mut model);
    ensure_speed_controls(&mut model);
    model
}

fn write_catalog(home: &Path, models: &[Value]) -> Result<()> {
    let mut catalog = serde_json::to_vec_pretty(&json!({ "models": models }))
        .context("序列化 Codey 模型目录失败")?;
    catalog.push(b'\n');
    atomic_write(&home.join(relative_path()), &catalog)
}

fn read_catalog_value(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codey 模型目录路径没有父目录"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建 Codey 模型目录失败：{}", parent.display()))?;
    let temp_path = temp_path_for(path, parent);
    if let Err(error) = fs::write(&temp_path, bytes) {
        let _ = fs::remove_file(&temp_path);
        return Err(error)
            .with_context(|| format!("写入临时模型目录失败：{}", temp_path.display()));
    }
    if let Err(error) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("替换模型目录失败：{}", path.display()));
    }
    Ok(())
}

fn temp_path_for(path: &Path, parent: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("codey-official.json");
    parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()))
}

fn replace_file(temp: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(temp, destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            #[cfg(windows)]
            {
                if destination.exists() {
                    fs::remove_file(destination)?;
                    return fs::rename(temp, destination);
                }
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official_cache() -> Value {
        json!({
            "models": [
                {
                    "slug": "gpt-5.6-sol",
                    "display_name": "GPT-5.6-Sol",
                    "visibility": "list",
                    "priority": 1,
                    "default_reasoning_level": "low",
                    "supported_reasoning_levels": [
                        {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                        {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}
                    ],
                    "service_tiers": [{"id": "priority"}],
                    "additional_speed_tiers": ["fast"]
                },
                {
                    "slug": "gpt-5.5",
                    "display_name": "GPT-5.5",
                    "visibility": "list",
                    "priority": 7,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [{"effort": "low"}, {"effort": "xhigh"}]
                },
                {
                    "slug": "gpt-5.4",
                    "display_name": "GPT-5.4",
                    "visibility": "hide",
                    "priority": 16,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [
                        {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                        {"effort": "xhigh"}, {"effort": "max"}
                    ],
                    "service_tiers": [{"id": "priority"}],
                    "additional_speed_tiers": ["fast"],
                    "upgrade": {"model": "gpt-5.6-sol"}
                },
                {
                    "slug": "gpt-5.3-codex-spark",
                    "display_name": "GPT-5.3-Codex-Spark",
                    "visibility": "list",
                    "priority": 30,
                    "supported_in_api": false,
                    "default_reasoning_level": "high",
                    "supported_reasoning_levels": [
                        {"effort": "low"}, {"effort": "medium"},
                        {"effort": "high"}, {"effort": "xhigh"}
                    ],
                    "service_tiers": [],
                    "additional_speed_tiers": []
                },
                {"slug": "codex-auto-review", "visibility": "hide", "priority": 43}
            ]
        })
    }

    fn write_cache(home: &Path) {
        fs::write(
            home.join("models_cache.json"),
            serde_json::to_vec(&official_cache()).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn official_catalog_preserves_reasoning_and_speed_metadata() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());

        assert_eq!(
            refresh_for_provider(home.path(), true, &[], &[]).unwrap(),
            5
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let efforts = catalog["models"][0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|level| level["effort"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max", "ultra"]);
        assert_eq!(catalog["models"][0]["service_tiers"][0]["id"], "priority");
        assert_eq!(catalog["models"][0]["supports_reasoning_summaries"], true);
        let spark = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == "gpt-5.3-codex-spark")
            .unwrap();
        assert_eq!(spark["supported_in_api"], true);
        assert_eq!(spark["service_tiers"][0]["id"], "priority");
        assert_eq!(spark["additional_speed_tiers"][0], "fast");
        assert_eq!(catalog["models"][4]["supports_reasoning_summaries"], false);
    }

    #[test]
    fn third_party_catalog_preserves_supported_official_capabilities() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.4".into(),
            "gpt-5.3-codex-spark".into(),
            "claude-sonnet".into(),
        ];
        let selected = upstream.clone();

        assert_eq!(
            refresh_for_provider(home.path(), false, &upstream, &selected).unwrap(),
            4
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog["models"][0]["slug"], "gpt-5.6-sol");
        assert_eq!(
            catalog["models"][0]["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|level| level["effort"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(catalog["models"][1]["slug"], "gpt-5.4");
        assert_eq!(catalog["models"][1]["visibility"], "list");
        assert!(catalog["models"][1].get("upgrade").is_none());
        assert_eq!(catalog["models"][1]["service_tiers"][0]["id"], "priority");
        assert_eq!(catalog["models"][1]["additional_speed_tiers"][0], "fast");
        assert_eq!(catalog["models"][2]["slug"], "gpt-5.3-codex-spark");
        assert_eq!(catalog["models"][2]["supported_in_api"], true);
        assert_eq!(catalog["models"][2]["service_tiers"][0]["id"], "priority");
        assert_eq!(catalog["models"][2]["additional_speed_tiers"][0], "fast");
        assert_eq!(catalog["models"][3]["slug"], "claude-sonnet");
        assert_eq!(catalog["models"][3]["codey_source"], "third_party");
        assert_eq!(catalog["models"][3]["service_tiers"][0]["id"], "priority");
        assert_eq!(catalog["models"][3]["additional_speed_tiers"][0], "fast");
        assert_eq!(
            catalog["models"][3]["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|level| level["effort"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "xhigh"]
        );
        assert_eq!(catalog["models"][3]["supports_reasoning_summaries"], true);
    }

    #[test]
    fn api_only_cold_start_builds_third_party_catalog_from_bundled_template() {
        let home = tempfile::tempdir().unwrap();
        let upstream = vec!["provider-fast-coder".into()];
        let selected = upstream.clone();

        assert_eq!(
            refresh_for_provider(home.path(), false, &upstream, &selected).unwrap(),
            1
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let model = &catalog["models"][0];
        assert_eq!(model["slug"], "provider-fast-coder");
        assert_eq!(model["codey_source"], "third_party");
        assert_eq!(model["visibility"], "list");
        assert_eq!(model["supported_in_api"], true);
        assert_eq!(model["service_tiers"][0]["id"], "priority");
        assert_eq!(model["additional_speed_tiers"][0], "fast");
        assert!(
            !model["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn official_cold_start_uses_the_bundled_catalog() {
        let home = tempfile::tempdir().unwrap();

        let count = refresh_for_provider(home.path(), true, &[], &[]).unwrap();
        assert_eq!(count, 8);
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-5.5",
                "gpt-5.4",
                "gpt-5.4-mini",
                "gpt-5.3-codex-spark",
                "codex-auto-review",
            ]
        );
        assert!(
            models[..7]
                .iter()
                .all(|model| model["visibility"] == "list")
        );
        assert_eq!(models[7]["visibility"], "hide");
        assert!(models.iter().all(|model| {
            model["service_tiers"]
                .as_array()
                .is_some_and(|tiers| !tiers.is_empty())
                && model["additional_speed_tiers"]
                    .as_array()
                    .is_some_and(|tiers| !tiers.is_empty())
        }));
    }

    #[test]
    fn empty_provider_catalog_does_not_replace_the_last_valid_catalog() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let catalog_path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        let previous = br#"{"models":[{"slug":"last-known-good"}]}"#;
        fs::write(&catalog_path, previous).unwrap();

        let error = refresh_for_provider(home.path(), false, &[], &[]).unwrap_err();

        assert!(error.to_string().contains("保留上一份合法目录"));
        assert_eq!(fs::read(catalog_path).unwrap(), previous);
    }

    #[test]
    fn selection_state_greys_unsupported_official_models_and_excludes_duplicates() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let state = selection_state(
            home.path(),
            false,
            &["gpt-5.6-sol".into(), "third-model".into()],
            &["gpt-5.6-sol".into(), "third-model".into()],
        )
        .unwrap();

        assert!(state.official_models[0].supported);
        assert!(!state.official_models[1].supported);
        assert_eq!(state.third_party_models, ["third-model"]);
    }

    #[test]
    fn selection_state_exposes_upstream_supported_legacy_official_models() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let state = selection_state(
            home.path(),
            false,
            &["gpt-5.4".into(), "codex-auto-review".into()],
            &[],
        )
        .unwrap();

        assert!(
            state
                .official_models
                .iter()
                .any(|model| model.slug == "gpt-5.4" && model.supported)
        );
        assert!(
            !state
                .official_models
                .iter()
                .any(|model| model.slug == "codex-auto-review")
        );
    }
}
