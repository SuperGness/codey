use super::*;
use serde::Deserialize;

pub(super) const FAST_SERVICE_TIER_ID: &str = "priority";
pub(super) const FAST_SPEED_TIER_ID: &str = "fast";
pub(super) const ULTRAFAST_TIER_ID: &str = "ultrafast";
const MANUAL_ULTRAFAST: &str = "codey_manual_ultrafast";

/// 只保存模型目录中与速度有关的字段，保留上游的顺序、文案和扩展属性。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelSpeedTiers {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_tiers: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_speed_tiers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_service_tier: Option<String>,
}

impl ModelSpeedTiers {
    pub(crate) fn from_model(model: &Value) -> Option<Self> {
        let services = model.get("service_tiers").and_then(Value::as_array);
        let speeds = model
            .get("additional_speed_tiers")
            .and_then(Value::as_array);
        let default = model.get("default_service_tier").and_then(Value::as_str);
        if services.is_none() && speeds.is_none() && default.is_none() {
            return None;
        }
        Some(Self {
            service_tiers: services
                .into_iter()
                .flatten()
                .filter(|tier| tier["id"].as_str().is_some_and(|id| !id.trim().is_empty()))
                .cloned()
                .collect(),
            additional_speed_tiers: speeds
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
                .collect(),
            default_service_tier: default
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned),
        })
    }

    fn write_to(&self, model: &mut Value) {
        model["service_tiers"] = Value::Array(self.service_tiers.clone());
        model["additional_speed_tiers"] = json!(self.additional_speed_tiers);
        let fields = model.as_object_mut().unwrap();
        if let Some(id) = &self.default_service_tier {
            fields.insert("default_service_tier".into(), json!(id));
        } else {
            fields.remove("default_service_tier");
        }
    }

    pub(crate) fn supports_ultrafast(&self) -> bool {
        self.additional_speed_tiers
            .iter()
            .any(|id| id == ULTRAFAST_TIER_ID)
            || self
                .service_tiers
                .iter()
                .any(|tier| tier["id"] == ULTRAFAST_TIER_ID)
            || self.default_service_tier.as_deref() == Some(ULTRAFAST_TIER_ID)
    }

    fn remove_ultrafast(&mut self) {
        self.service_tiers
            .retain(|tier| tier["id"] != ULTRAFAST_TIER_ID);
        self.additional_speed_tiers
            .retain(|id| id != ULTRAFAST_TIER_ID);
        if self.default_service_tier.as_deref() == Some(ULTRAFAST_TIER_ID) {
            self.default_service_tier = None;
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelSpeedPolicy {
    /// 有线路自己的同步结果时，它完整替代模板的速度声明，空声明同样生效。
    pub(crate) tiers: Option<ModelSpeedTiers>,
    pub(crate) ultrafast: Option<bool>,
}

impl ModelSpeedPolicy {
    /// 已有会话缓存可发送的档位；名称、说明和显示顺序可以直接更新。
    pub(crate) fn request_tiers(&self) -> (std::collections::BTreeSet<&str>, Option<&str>) {
        let mut ids = std::collections::BTreeSet::from([FAST_SERVICE_TIER_ID]);
        if self.ultrafast == Some(true) {
            ids.insert(ULTRAFAST_TIER_ID);
        }
        let Some(tiers) = &self.tiers else {
            return (ids, None);
        };
        ids.extend(
            tiers
                .service_tiers
                .iter()
                .filter_map(|item| item["id"].as_str()),
        );
        ids.extend(
            tiers
                .additional_speed_tiers
                .iter()
                .map(|id| service_id_for_speed(id)),
        );
        let mut default = tiers.default_service_tier.as_deref();
        ids.extend(default);
        if self.ultrafast == Some(false) {
            ids.remove(ULTRAFAST_TIER_ID);
            default = default.filter(|id| *id != ULTRAFAST_TIER_ID);
        }
        (ids, default)
    }
}

/// 用户提供的目录按完整模型标识匹配，不能把一个账号的档位套到同名线路。
pub(crate) fn user_catalog_model_speed_tiers(home: &Path) -> BTreeMap<String, ModelSpeedTiers> {
    let Ok(Some(path)) =
        crate::codex_config::runtime_model_catalog_path(home, false, None, &Default::default())
    else {
        return BTreeMap::new();
    };
    let catalog = read_catalog_value(&path);
    catalog
        .as_ref()
        .and_then(|catalog| catalog["models"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|model| {
            Some((
                model_id::key(model["slug"].as_str()?),
                ModelSpeedTiers::from_model(model)?,
            ))
        })
        .collect()
}

pub(crate) fn resolve_native_speed_declarations(
    home: &Path,
    context: &CatalogContext<'_>,
    policies: &mut BTreeMap<String, ModelSpeedPolicy>,
) {
    if policies.values().all(|policy| policy.tiers.is_some()) {
        return;
    }
    if let Ok(native) = read_official_entries_in_context(home, context) {
        for model in native.iter() {
            let key = model_id::key(model["slug"].as_str().unwrap());
            if let Some(policy) = policies.get_mut(&key)
                && policy.tiers.is_none()
            {
                policy.tiers = ModelSpeedTiers::from_model(model);
            }
        }
    }
    for policy in policies.values_mut() {
        policy.tiers.get_or_insert_with(ModelSpeedTiers::default);
    }
}

/// 热更新与回滚都按传入的线路配置生成档位，磁盘上的运行时目录可能已属于下一次更新。
pub(crate) fn renderer_speed_metadata(
    home: &Path,
    config: &crate::config::CodeyConfig,
) -> BTreeMap<String, serde_json::Map<String, Value>> {
    let mut policies = config.runtime_model_speed_tiers();
    resolve_native_speed_declarations(
        home,
        &CatalogContext::new(&config.codex_app_path),
        &mut policies,
    );
    let user_tiers = user_catalog_model_speed_tiers(home);
    if !user_tiers.is_empty() {
        let overrides = config.user_catalog_speed_overrides();
        for (key, tiers) in user_tiers {
            if let Some(policy) = policies.get_mut(&key)
                && overrides
                    .get(&key)
                    .is_none_or(|entry| entry.tiers.is_none())
            {
                policy.tiers = Some(tiers);
            }
        }
    }
    policies
        .into_iter()
        .map(|(key, policy)| {
            let mut model = json!({});
            apply_model_speed_policy(&mut model, Some(&policy));
            let Value::Object(mut fields) = model else {
                unreachable!()
            };
            fields.remove(MANUAL_ULTRAFAST);
            (key, fields)
        })
        .collect()
}

#[cfg(test)]
pub(super) fn declares_tier(model: &Value, service: &str, speed: &str) -> bool {
    model["service_tiers"]
        .as_array()
        .is_some_and(|tiers| tiers.iter().any(|tier| tier["id"] == service))
        || model["additional_speed_tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|tier| tier == speed))
}

#[cfg(test)]
pub(super) fn declares_fast_speed_support(model: &Value) -> bool {
    declares_tier(model, FAST_SERVICE_TIER_ID, FAST_SPEED_TIER_ID)
}

fn service_id_for_speed(speed: &str) -> &str {
    if speed == FAST_SPEED_TIER_ID {
        FAST_SERVICE_TIER_ID
    } else {
        speed
    }
}

fn speed_id_for_service(service: &str) -> &str {
    if service == FAST_SERVICE_TIER_ID {
        FAST_SPEED_TIER_ID
    } else {
        service
    }
}

fn tier_labels(id: &str) -> (&str, &str) {
    match id {
        FAST_SERVICE_TIER_ID => ("Fast", "1.5x speed, increased usage"),
        ULTRAFAST_TIER_ID => (
            "Ultrafast",
            "The fastest available responses for latency-sensitive work.",
        ),
        _ => (id, ""),
    }
}

impl ModelSpeedTiers {
    /// Codex 要求每个服务档位有名称和说明。只声明了 ID 时以原 ID 展示。
    fn complete_for_codex(&mut self, manual_ultrafast: bool) -> bool {
        let mut speeds = HashSet::new();
        self.additional_speed_tiers
            .retain(|id| !id.trim().is_empty() && speeds.insert(id.clone()));
        let mut services = HashSet::new();
        self.service_tiers.retain_mut(|row| {
            let Some(id) = row["id"]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
            else {
                return false;
            };
            if !services.insert(id.clone()) {
                return false;
            }
            let (name, description) = tier_labels(&id);
            if row["name"]
                .as_str()
                .is_none_or(|name| name.trim().is_empty())
            {
                row["name"] = json!(name);
            }
            if !row["description"].is_string() {
                row["description"] = json!(description);
            }
            let speed = speed_id_for_service(&id);
            if speeds.insert(speed.to_owned()) {
                self.additional_speed_tiers.push(speed.to_owned());
            }
            true
        });
        // auto 和 default 是请求的默认处理方式；上游显式列出时才作为选项展示。
        let default_speed = self
            .default_service_tier
            .as_deref()
            .filter(|id| !matches!(*id, "" | "auto" | "default"))
            .map(speed_id_for_service);
        let add_manual = manual_ultrafast
            && default_speed != Some(ULTRAFAST_TIER_ID)
            && !speeds.contains(ULTRAFAST_TIER_ID);
        for id in [
            Some(FAST_SPEED_TIER_ID),
            default_speed,
            add_manual.then_some(ULTRAFAST_TIER_ID),
        ]
        .into_iter()
        .flatten()
        {
            if speeds.insert(id.to_owned()) {
                self.additional_speed_tiers.push(id.to_owned());
            }
        }
        for speed in &self.additional_speed_tiers {
            let id = service_id_for_speed(speed);
            if services.insert(id.to_owned()) {
                let (name, description) = tier_labels(id);
                self.service_tiers
                    .push(json!({"id": id, "name": name, "description": description}));
            }
        }
        add_manual
    }
}

pub(super) fn apply_model_speed_policy(model: &mut Value, policy: Option<&ModelSpeedPolicy>) {
    let was_manual =
        model.as_object_mut().unwrap().remove(MANUAL_ULTRAFAST) == Some(Value::Bool(true));
    let mut tiers = if let Some(declared) = policy.and_then(|policy| policy.tiers.as_ref()) {
        declared.clone()
    } else {
        let mut tiers = ModelSpeedTiers::from_model(model).unwrap_or_default();
        if was_manual {
            tiers.remove_ultrafast();
        }
        tiers
    };
    let ultrafast = policy.and_then(|policy| policy.ultrafast);
    if ultrafast == Some(false) {
        tiers.remove_ultrafast();
    }
    if tiers.complete_for_codex(ultrafast == Some(true)) {
        model[MANUAL_ULTRAFAST] = Value::Bool(true);
    }
    tiers.write_to(model);
}

/// 账号可以只返回一种档位字段。两个字段要来自同一份声明，以免旧模板补回已撤掉的档位。
pub(super) fn replace_template_speed_tiers(merged: &mut Value, account: &Value) {
    clear_template_speed_tiers(merged);
    if let Some(tiers) = ModelSpeedTiers::from_model(account) {
        tiers.write_to(merged);
    }
}

pub(super) fn clear_template_speed_tiers(model: &mut Value) {
    let fields = model.as_object_mut().unwrap();
    for key in [
        MANUAL_ULTRAFAST,
        "service_tiers",
        "additional_speed_tiers",
        "default_service_tier",
    ] {
        fields.remove(key);
    }
}
