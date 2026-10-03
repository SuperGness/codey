// Static model catalog: routing table, thinking budgets, max output tokens,
// model enums, and dynamic-catalog grouping (port of upstream models.ts +
// grouping.ts). Pure logic; no I/O.
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl Effort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
        }
    }
    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_lowercase().as_str() {
            "off" | "none" => Some(Effort::Off),
            "minimal" => Some(Effort::Minimal),
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "xhigh" => Some(Effort::Xhigh),
            _ => None,
        }
    }
}

/// Extract reasoning effort from a Responses body: `reasoning.effort`,
/// the chat-style `reasoning_effort` alias, then the effort suffix embedded
/// in the model id (gemini-3.8-flash-high).
pub fn extract_effort(body: &Value, model: &str) -> Option<Effort> {
    if let Some(e) = body
        .pointer("/reasoning/effort")
        .and_then(|v| v.as_str())
        .and_then(Effort::parse)
    {
        return Some(e);
    }
    if let Some(e) = body
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        .and_then(Effort::parse)
    {
        return Some(e);
    }
    suffix_effort(model)
}

pub fn suffix_effort(model: &str) -> Option<Effort> {
    let lower = model.to_lowercase();
    for suffix in ["minimal", "low", "medium", "high", "xhigh"] {
        if lower.ends_with(&format!("-{suffix}")) {
            return Effort::parse(suffix);
        }
    }
    None
}

type Routing = BTreeMap<&'static str, &'static str>; // effort -> runtime id

fn routing(entries: &[(&'static str, &'static str)]) -> Routing {
    entries.iter().copied().collect()
}

struct Route {
    off: &'static str,
    map: Routing,
}

fn static_routing() -> HashMap<&'static str, Route> {
    let mut m: HashMap<&'static str, Route> = HashMap::new();
    m.insert(
        "claude-opus-4-6",
        Route {
            off: "claude-opus-4-6-thinking",
            map: routing(&[
                ("minimal", "claude-opus-4-6-thinking"),
                ("low", "claude-opus-4-6-thinking"),
                ("medium", "claude-opus-4-6-thinking"),
                ("high", "claude-opus-4-6-thinking"),
                ("xhigh", "claude-opus-4-6-thinking"),
            ]),
        },
    );
    m.insert(
        "claude-sonnet-4-6",
        Route {
            off: "claude-sonnet-4-6",
            map: routing(&[
                ("minimal", "claude-sonnet-4-6"),
                ("low", "claude-sonnet-4-6"),
                ("medium", "claude-sonnet-4-6"),
                ("high", "claude-sonnet-4-6"),
                ("xhigh", "claude-sonnet-4-6"),
            ]),
        },
    );
    m.insert(
        "gemini-3.1-pro",
        Route {
            off: "gemini-3.1-pro-low",
            map: routing(&[
                ("minimal", "gemini-3.1-pro-low"),
                ("low", "gemini-3.1-pro-low"),
                ("medium", "gemini-3.1-pro-low"),
                ("high", "gemini-pro-agent"),
                ("xhigh", "gemini-pro-agent"),
            ]),
        },
    );
    m.insert(
        "gemini-3.8-flash",
        Route {
            off: "gemini-3.8-flash-low",
            map: routing(&[
                ("minimal", "gemini-3.8-flash-low"),
                ("low", "gemini-3.8-flash-low"),
                ("medium", "gemini-3.8-flash-medium"),
                ("high", "gemini-3.8-flash-high"),
                ("xhigh", "gemini-3.8-flash-high"),
            ]),
        },
    );
    m.insert(
        "gemini-3.7-flash",
        Route {
            off: "gemini-3.7-flash-low",
            map: routing(&[
                ("minimal", "gemini-3.7-flash-low"),
                ("low", "gemini-3.7-flash-low"),
                ("medium", "gemini-3.7-flash-medium"),
                ("high", "gemini-3.7-flash-high"),
                ("xhigh", "gemini-3.7-flash-high"),
            ]),
        },
    );
    m.insert(
        "gemini-3.6-flash",
        Route {
            off: "gemini-3.6-flash-low",
            map: routing(&[
                ("minimal", "gemini-3.6-flash-low"),
                ("low", "gemini-3.6-flash-low"),
                ("medium", "gemini-3.6-flash-medium"),
                ("high", "gemini-3.6-flash-high"),
                ("xhigh", "gemini-3.6-flash-high"),
            ]),
        },
    );
    m.insert(
        "gemini-3.5-flash",
        Route {
            off: "gemini-3.5-flash-extra-low",
            map: routing(&[
                ("minimal", "gemini-3.5-flash-extra-low"),
                ("low", "gemini-3.5-flash-extra-low"),
                ("medium", "gemini-3.5-flash-low"),
                ("high", "gemini-3-flash-agent"),
                ("xhigh", "gemini-3-flash-agent"),
            ]),
        },
    );
    m.insert(
        "gpt-oss-120b",
        Route {
            off: "gpt-oss-120b-medium",
            map: routing(&[
                ("minimal", "gpt-oss-120b-medium"),
                ("low", "gpt-oss-120b-medium"),
                ("medium", "gpt-oss-120b-medium"),
                ("high", "gpt-oss-120b-medium"),
            ]),
        },
    );
    m
}

/// Resolve a public model id + thinking effort to the Antigravity runtime
/// model id. Runtime-suffixed ids pass through; unknown ids pass through
/// (dynamic discovery may resolve them later).
pub fn get_request_model_id(model_id: &str, effort: Option<Effort>) -> String {
    let table = static_routing();
    if let Some(route) = table.get(model_id) {
        let key = match effort {
            None | Some(Effort::Off) => {
                return route.off.to_string();
            }
            Some(Effort::Xhigh) => {
                return route
                    .map
                    .get("xhigh")
                    .or_else(|| route.map.get("high"))
                    .or_else(|| route.map.get("low"))
                    .or_else(|| route.map.get("minimal"))
                    .copied()
                    .unwrap_or(route.off)
                    .to_string();
            }
            Some(e) => e,
        };
        return route
            .map
            .get(key.as_str())
            .or_else(|| route.map.get("low"))
            .or_else(|| route.map.get("minimal"))
            .copied()
            .unwrap_or(route.off)
            .to_string();
    }
    model_id.to_string()
}

/// True when the requested id is a known public model with a routing entry.
pub fn is_known_public_model(model_id: &str) -> bool {
    static_routing().contains_key(model_id)
}

/// Verified maximum output tokens accepted by the backend per runtime id.
/// Requesting more returns 400 (port of RUNTIME_MAX_OUTPUT_TOKENS).
pub fn get_max_output_tokens(model_id: &str, runtime_model: &str) -> i64 {
    const TABLE: &[(&str, i64)] = &[
        ("gemini-3.8-flash", 65536),
        ("gemini-3.8-flash-low", 65536),
        ("gemini-3.8-flash-medium", 65536),
        ("gemini-3.8-flash-high", 65536),
        ("gemini-3.7-flash", 65536),
        ("gemini-3.7-flash-tiered", 65536),
        ("gemini-3.7-flash-low", 65536),
        ("gemini-3.7-flash-medium", 65536),
        ("gemini-3.7-flash-high", 65536),
        ("gemini-3.6-flash", 65536),
        ("gemini-3.6-flash-low", 65536),
        ("gemini-3.6-flash-medium", 65536),
        ("gemini-3.6-flash-high", 65536),
        ("gemini-3.5-flash", 65536),
        ("gemini-3.5-flash-extra-low", 65536),
        ("gemini-3.5-flash-low", 65536),
        ("gemini-3-flash-agent", 65536),
        ("gemini-3.1-pro", 65535),
        ("gemini-3.1-pro-low", 65535),
        ("gemini-3.1-pro-high", 65535),
        ("gemini-pro-agent", 65535),
        ("claude-opus-4-6", 64000),
        ("claude-opus-4-6-thinking", 64000),
        ("claude-sonnet-4-6", 64000),
        ("gpt-oss-120b", 32768),
        ("gpt-oss-120b-medium", 32768),
    ];
    for (id, max) in TABLE {
        if *id == runtime_model {
            return *max;
        }
    }
    for (id, max) in TABLE {
        if *id == model_id {
            return *max;
        }
    }
    if runtime_model.starts_with("claude-") {
        return 64000;
    }
    if runtime_model.starts_with("gpt-oss-") {
        return 32768;
    }
    if runtime_model.starts_with("gemini-3.1-pro") || runtime_model == "gemini-pro-agent" {
        return 65535;
    }
    if runtime_model.starts_with("gemini-") {
        return 65536;
    }
    8192
}

/// Wire thinkingConfig per model family and effort (port of getThinkingConfig).
/// Gemini 3.8/3.7/3.6: high/xhigh -> -1 (dynamic), medium -> 4000, else 1000.
/// Gemini 3.5: 10000/4000/1000. 3.1 Pro: 10001/1001. Claude: 1024. GPT-OSS: 8192.
pub fn get_thinking_config(model_id: &str, effort: Option<Effort>) -> Option<(bool, i64)> {
    let off = || Some((false, 0));
    let on = |budget: i64| Some((true, budget));
    if model_id.starts_with("claude-") {
        return match effort {
            None | Some(Effort::Off) => off(),
            _ => on(1024),
        };
    }
    if model_id.starts_with("gpt-oss-") {
        return match effort {
            None | Some(Effort::Off) => off(),
            _ => on(8192),
        };
    }
    if model_id.starts_with("gemini-3.5-flash") || model_id == "gemini-3-flash-agent" {
        return match effort {
            None | Some(Effort::Off) => off(),
            Some(Effort::High) | Some(Effort::Xhigh) => on(10_000),
            Some(Effort::Medium) => on(4_000),
            _ => on(1_000),
        };
    }
    if model_id.starts_with("gemini-3.1-pro") || model_id == "gemini-pro-agent" {
        return match effort {
            None | Some(Effort::Off) => off(),
            Some(Effort::High) | Some(Effort::Xhigh) => on(10_001),
            _ => on(1_001),
        };
    }
    if model_id.starts_with("gemini-") {
        return match effort {
            None | Some(Effort::Off) => off(),
            Some(Effort::High) | Some(Effort::Xhigh) => on(-1),
            Some(Effort::Medium) => on(4_000),
            _ => on(1_000),
        };
    }
    None
}

/// Next-generation fallback chain: 3.8 -> 3.7 -> 3.6 (port of getFallbackRuntimeModel).
pub fn get_fallback_runtime_model(runtime_model: &str, effort: Option<Effort>) -> Option<String> {
    if let Some(rest) = runtime_model.strip_prefix("gemini-3.8-flash-") {
        return Some(format!("gemini-3.7-flash-{rest}"));
    }
    if runtime_model == "gemini-3.8-flash" {
        return Some("gemini-3.7-flash-low".into());
    }
    if runtime_model == "gemini-3.7-flash-tiered" {
        return Some(get_request_model_id("gemini-3.6-flash", effort));
    }
    if let Some(rest) = runtime_model.strip_prefix("gemini-3.7-flash-") {
        return Some(format!("gemini-3.6-flash-{rest}"));
    }
    if runtime_model == "gemini-3.7-flash" {
        return Some("gemini-3.6-flash-low".into());
    }
    None
}

// --- Model enums (MODEL_PLACEHOLDER_* wire labels) ---------------------------

pub fn static_model_enum(wire_model_id: &str) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("gemini-3.8-flash", "MODEL_PLACEHOLDER_M318"),
        ("gemini-3.8-flash-high", "MODEL_PLACEHOLDER_M318"),
        ("gemini-3.8-flash-medium", "MODEL_PLACEHOLDER_M319"),
        ("gemini-3.8-flash-low", "MODEL_PLACEHOLDER_M320"),
        ("gemini-3.8-flash-tiered", "MODEL_PLACEHOLDER_M322"),
        ("gemini-3.7-flash", "MODEL_PLACEHOLDER_M298"),
        ("gemini-3.7-flash-high", "MODEL_PLACEHOLDER_M298"),
        ("gemini-3.7-flash-medium", "MODEL_PLACEHOLDER_M299"),
        ("gemini-3.7-flash-low", "MODEL_PLACEHOLDER_M300"),
        ("gemini-3.7-flash-tiered", "MODEL_PLACEHOLDER_M301"),
        ("gemini-3.6-flash", "MODEL_PLACEHOLDER_M71"),
        ("gemini-3.6-flash-high", "MODEL_PLACEHOLDER_M71"),
        ("gemini-3.6-flash-medium", "MODEL_PLACEHOLDER_M72"),
        ("gemini-3.6-flash-low", "MODEL_PLACEHOLDER_M73"),
        ("gemini-3.6-flash-tiered", "MODEL_PLACEHOLDER_M196"),
        ("gemini-3.5-flash", "MODEL_PLACEHOLDER_M20"),
        ("gemini-3.5-flash-extra-low", "MODEL_PLACEHOLDER_M187"),
        ("gemini-3.5-flash-low", "MODEL_PLACEHOLDER_M20"),
        ("gemini-3-flash-agent", "MODEL_PLACEHOLDER_M84"),
        ("gemini-3.1-pro", "MODEL_PLACEHOLDER_M36"),
        ("gemini-3.1-pro-low", "MODEL_PLACEHOLDER_M36"),
        ("gemini-3.1-pro-high", "MODEL_PLACEHOLDER_M37"),
        ("gemini-pro-agent", "MODEL_PLACEHOLDER_M16"),
        ("claude-sonnet-4-6", "MODEL_PLACEHOLDER_M35"),
        ("claude-opus-4-6", "MODEL_PLACEHOLDER_M26"),
        ("claude-opus-4-6-thinking", "MODEL_PLACEHOLDER_M26"),
        ("gpt-oss-120b", "MODEL_OPENAI_GPT_OSS_120B_MEDIUM"),
        ("gpt-oss-120b-medium", "MODEL_OPENAI_GPT_OSS_120B_MEDIUM"),
    ];
    TABLE
        .iter()
        .find(|(id, _)| *id == wire_model_id)
        .map(|(_, e)| *e)
}

fn enum_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, String>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_model_enum(wire_model_id: &str, model_enum: &str) {
    if wire_model_id.is_empty() || model_enum.is_empty() {
        return;
    }
    if let Ok(mut cache) = enum_cache().lock() {
        cache.insert(wire_model_id.to_string(), model_enum.to_string());
    }
}

/// Register the model enums learned from a fetchAvailableModels payload.
pub fn register_discovered_model_enums(models: &Value) {
    if let Some(map) = models.get("models").and_then(|m| m.as_object()) {
        for (wire_id, info) in map {
            if let Some(e) = info.get("model").and_then(|m| m.as_str()) {
                register_model_enum(wire_id, e);
            }
        }
    }
}

/// model_enum label for a wire model id: dynamic cache first, then static
/// table, then the enum of the effort-routed runtime id.
pub fn get_model_enum(wire_model_id: &str) -> Option<String> {
    if let Ok(cache) = enum_cache().lock() {
        if let Some(e) = cache.get(wire_model_id) {
            return Some(e.clone());
        }
    }
    if let Some(e) = static_model_enum(wire_model_id) {
        return Some(e.to_string());
    }
    let routed = get_request_model_id(wire_model_id, None);
    if let Ok(cache) = enum_cache().lock() {
        if let Some(e) = cache.get(&routed) {
            return Some(e.clone());
        }
    }
    static_model_enum(&routed).map(|e| e.to_string())
}

pub fn snapshot_model_enums() -> HashMap<String, String> {
    enum_cache().lock().map(|c| c.clone()).unwrap_or_default()
}

pub fn restore_model_enums(map: HashMap<String, String>) {
    if let Ok(mut cache) = enum_cache().lock() {
        cache.clear();
        cache.extend(map);
    }
}

// --- Grouping: runtime catalog -> public model ids ---------------------------

#[derive(Debug, Clone)]
pub struct PublicModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    /// advertised levels ("" entries dropped); mirrors thinkingLevelMap
    pub levels: Vec<Effort>,
    pub input_image: bool,
    pub context_window: i64,
    pub max_tokens: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub models: Vec<PublicModel>,
    /// public id -> effort -> runtime id
    pub routing: HashMap<String, HashMap<String, String>>,
    /// public id -> default runtime id (the `off` choice)
    pub default_request: HashMap<String, String>,
}

struct RuntimeGroup {
    public_id: String,
    /// level -> runtime id
    variants: HashMap<String, String>,
    unsuffixed: Option<String>,
    display_names: Vec<String>,
    supports_thinking: Option<bool>,
    supports_images: Option<bool>,
}

const THINKING_SUFFIXES: &[(&str, &str)] = &[
    // (suffix, level) — first match wins (checked in this order)
    ("extra-low", "low"),
    ("extra-high", "xhigh"),
    ("thinking", "high"),
    ("minimal", "minimal"),
    ("medium", "medium"),
    ("high", "high"),
    ("low", "low"),
];

fn level_from_display_name(name: Option<&str>) -> Option<String> {
    let name = name?;
    let lower = name.to_lowercase();
    let patterns: &[(&str, &str)] = &[
        ("(extra low)", "low"),
        ("(extra high)", "xhigh"),
        ("(thinking)", "high"),
        ("(minimal)", "minimal"),
        ("(medium)", "medium"),
        ("(high)", "high"),
        ("(low)", "low"),
    ];
    for (pat, level) in patterns {
        if lower.contains(pat) {
            return Some(level.to_string());
        }
    }
    None
}

fn known_runtime_alias(runtime_id: &str) -> Option<(&'static str, &'static str)> {
    match runtime_id {
        "gemini-3-flash-agent" => Some(("gemini-3.5-flash", "high")),
        "gemini-pro-agent" => Some(("gemini-3.1-pro", "high")),
        _ => None,
    }
}

pub fn is_selectable_runtime_model_id(id: &str) -> bool {
    let usable =
        (id.starts_with("gemini-") || id.starts_with("claude-") || id.starts_with("gpt-oss-"))
            && !id.contains(' ')
            && !id.starts_with("MODEL_")
            && !id.starts_with("chat_")
            && !id.starts_with("tab_")
            && !id.to_lowercase().contains("image");
    usable
}

fn model_display_name(info: &Value) -> Option<String> {
    for key in ["displayName", "label", "modelName"] {
        if let Some(s) = info.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn display_family(name: Option<&str>) -> Option<String> {
    let name = name?;
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\s*\((?:extra\s*low|extra\s*high|low|medium|high|minimal|thinking)\)\s*$",
        )
        .unwrap()
    });
    Some(re.replace(name, "").trim().to_lowercase())
}

fn parse_thinking_suffix(runtime_id: &str) -> Option<(String, String)> {
    let lower = runtime_id.to_lowercase();
    for (suffix, level) in THINKING_SUFFIXES {
        if lower.ends_with(&format!("-{suffix}")) {
            return Some((
                runtime_id[..runtime_id.len() - suffix.len() - 1].to_string(),
                level.to_string(),
            ));
        }
    }
    None
}

/// Static fallback catalog (mirrors ANTIGRAVITY_MODELS in models.ts).
pub fn fallback_catalog() -> Catalog {
    let entries: &[(&str, &str, bool, &[&str], i64, i64)] = &[
        // (id, name, reasoning, levels, contextWindow, maxTokens)
        (
            "gemini-3.8-flash",
            "Gemini 3.8 Flash (Antigravity)",
            true,
            &["low", "medium", "high"],
            1048576,
            65536,
        ),
        (
            "gemini-3.7-flash",
            "Gemini 3.7 Flash (Antigravity)",
            true,
            &["low", "medium", "high"],
            1048576,
            65536,
        ),
        (
            "gemini-3.6-flash",
            "Gemini 3.6 Flash (Antigravity)",
            true,
            &["low", "medium", "high"],
            1048576,
            65536,
        ),
        (
            "claude-opus-4-6",
            "Claude Opus 4.6 (Antigravity)",
            true,
            &["high"],
            250000,
            64000,
        ),
        (
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6 (Antigravity)",
            true,
            &["high"],
            200000,
            64000,
        ),
        (
            "gemini-3.1-pro",
            "Gemini 3.1 Pro (Antigravity)",
            true,
            &["low", "high"],
            1048576,
            65535,
        ),
        (
            "gemini-3.5-flash",
            "Gemini 3.5 Flash (Antigravity)",
            true,
            &["low", "medium", "high"],
            1048576,
            65536,
        ),
        (
            "gpt-oss-120b",
            "GPT-OSS 120B (Antigravity)",
            true,
            &["medium"],
            131072,
            32768,
        ),
    ];
    let table = static_routing();
    let mut models = Vec::new();
    let mut routing = HashMap::new();
    let mut default_request = HashMap::new();
    for (id, name, reasoning, levels, ctx, max) in entries {
        models.push(PublicModel {
            id: id.to_string(),
            name: name.to_string(),
            reasoning: *reasoning,
            levels: levels.iter().filter_map(|l| Effort::parse(l)).collect(),
            input_image: true,
            context_window: *ctx,
            max_tokens: *max,
        });
        if let Some(route) = table.get(*id) {
            let map: HashMap<String, String> = route
                .map
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            default_request.insert(id.to_string(), route.off.to_string());
            routing.insert(id.to_string(), map);
        }
    }
    Catalog {
        models,
        routing,
        default_request,
    }
}

/// Build a public catalog from a raw fetchAvailableModels `models` map
/// (port of buildAntigravityCatalog in grouping.ts).
pub fn build_catalog(raw_models: &Value, fallback: &Catalog) -> Catalog {
    let Some(map) = raw_models.as_object() else {
        return fallback.clone();
    };
    let mut groups: Vec<RuntimeGroup> = Vec::new();
    let ensure = |groups: &mut Vec<RuntimeGroup>, public_id: String| -> usize {
        if let Some(i) = groups.iter().position(|g| g.public_id == public_id) {
            return i;
        }
        groups.push(RuntimeGroup {
            public_id,
            variants: HashMap::new(),
            unsuffixed: None,
            display_names: Vec::new(),
            supports_thinking: None,
            supports_images: None,
        });
        groups.len() - 1
    };

    for (runtime_id, info) in map {
        if !is_selectable_runtime_model_id(runtime_id) {
            continue;
        }
        if info
            .get("isInternal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let display_name = model_display_name(info);
        if let Some(base_id) = runtime_id.strip_suffix("-tiered") {
            let i = ensure(&mut groups, base_id.to_string());
            absorb(&mut groups[i], info, display_name.as_deref());
            if groups[i].unsuffixed.is_none() {
                groups[i].unsuffixed = Some(runtime_id.clone());
            }
            continue;
        }
        let alias = known_runtime_alias(runtime_id);
        let suffix = alias
            .and(None::<(String, String)>)
            .or_else(|| parse_thinking_suffix(runtime_id));
        let public_id = alias
            .map(|(p, _)| p.to_string())
            .or_else(|| suffix.as_ref().map(|(b, _)| b.clone()))
            .unwrap_or_else(|| runtime_id.clone());
        let i = ensure(&mut groups, public_id);
        absorb(&mut groups[i], info, display_name.as_deref());
        let level = alias
            .map(|(_, l)| l.to_string())
            .or_else(|| level_from_display_name(display_name.as_deref()))
            .or_else(|| suffix.as_ref().map(|(_, l)| l.clone()));
        if let Some(level) = level {
            groups[i].variants.insert(level, runtime_id.clone());
        } else if groups[i].unsuffixed.is_none() {
            groups[i].unsuffixed = Some(runtime_id.clone());
        }
    }

    merge_agent_singletons(&mut groups);
    if groups.is_empty() {
        return fallback.clone();
    }

    let mut models: Vec<PublicModel> = Vec::new();
    let mut routing: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut default_request: HashMap<String, String> = HashMap::new();

    for group in &groups {
        if let (Some(fm), Some(fr), Some(dr)) = (
            fallback.models.iter().find(|m| m.id == group.public_id),
            fallback.routing.get(&group.public_id),
            fallback.default_request.get(&group.public_id),
        ) {
            models.push(fm.clone());
            routing.insert(group.public_id.clone(), fr.clone());
            default_request.insert(group.public_id.clone(), dr.clone());
            continue;
        }
        let (model, route, def) = synthesize(group, &fallback.models);
        models.push(model);
        routing.insert(group.public_id.clone(), route);
        default_request.insert(group.public_id.clone(), def);
    }
    // Keep fallback models whose runtime ids did not appear in the discovery.
    for fm in &fallback.models {
        if routing.contains_key(&fm.id) {
            continue;
        }
        if let (Some(fr), Some(dr)) = (
            fallback.routing.get(&fm.id),
            fallback.default_request.get(&fm.id),
        ) {
            models.push(fm.clone());
            routing.insert(fm.id.clone(), fr.clone());
            default_request.insert(fm.id.clone(), dr.clone());
        }
    }

    models.sort_by(compare_public_models);
    Catalog {
        models,
        routing,
        default_request,
    }
}

fn absorb(group: &mut RuntimeGroup, info: &Value, display_name: Option<&str>) {
    if let Some(d) = display_name {
        group.display_names.push(d.to_string());
    }
    match info.get("supportsThinking").and_then(|v| v.as_bool()) {
        Some(true) => group.supports_thinking = Some(true),
        Some(false) if group.supports_thinking != Some(true) => {
            group.supports_thinking = Some(false)
        }
        _ => {}
    }
    match info.get("supportsImages").and_then(|v| v.as_bool()) {
        Some(true) => group.supports_images = Some(true),
        Some(false) if group.supports_images.is_none() => group.supports_images = Some(false),
        _ => {}
    }
}

/// `*-agent` singletons join the family group matching their display name.
fn merge_agent_singletons(groups: &mut Vec<RuntimeGroup>) {
    let singletons: Vec<(String, String)> = groups
        .iter()
        .filter(|g| g.public_id.ends_with("-agent") && g.variants.is_empty())
        .filter_map(|g| {
            let family = display_family(g.display_names.first().map(|s| s.as_str()))?;
            g.unsuffixed.clone().map(|u| (family, u))
        })
        .collect();
    for (family, unsuffixed) in singletons {
        let Some(idx) = groups.iter().position(|g| {
            g.public_id.ends_with("-agent")
                && g.variants.is_empty()
                && display_family(g.display_names.first().map(|s| s.as_str())).as_deref()
                    == Some(&family)
        }) else {
            continue;
        };
        let level = level_from_display_name(groups[idx].display_names.first().map(|s| s.as_str()))
            .unwrap_or_else(|| "high".to_string());
        let display_names = groups[idx].display_names.clone();
        let singleton_public_id = groups[idx].public_id.clone();
        let target = groups.iter_mut().find(|c| {
            c.public_id != singleton_public_id
                && c.display_names
                    .iter()
                    .any(|n| display_family(Some(n)).as_deref() == Some(&family))
        });
        if let Some(target) = target {
            target.variants.insert(level, unsuffixed);
            target.display_names.extend(display_names);
        }
        groups.remove(idx);
    }
}

fn synthesize(
    group: &RuntimeGroup,
    fallback_models: &[PublicModel],
) -> (PublicModel, HashMap<String, String>, String) {
    let template = family_template(&group.public_id, fallback_models);
    let mut levels: Vec<String> = group.variants.keys().cloned().collect();
    if levels.is_empty() {
        if group.supports_thinking != Some(false)
            && (group.supports_thinking == Some(true) || group.unsuffixed.is_some())
        {
            levels.push("high".to_string());
        }
    }
    let reasoning = !levels.is_empty()
        || group.supports_thinking == Some(true)
        || (group.supports_thinking.is_none()
            && template.as_ref().map(|t| t.reasoning).unwrap_or(false));
    let supports_images = group
        .supports_images
        .unwrap_or_else(|| template.as_ref().map(|t| t.input_image).unwrap_or(true));

    let pick = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(id) = group.variants.get(*k) {
                return id.clone();
            }
        }
        group
            .unsuffixed
            .clone()
            .unwrap_or_else(|| default_of(group))
    };
    let default_id = default_of(group);
    let mut route = HashMap::new();
    route.insert("minimal", pick(&["minimal", "low", "medium", "high"]));
    route.insert("low", pick(&["low", "minimal", "medium", "high"]));
    route.insert("medium", pick(&["medium", "low", "high", "minimal"]));
    route.insert("high", pick(&["high", "medium", "low", "minimal"]));
    route.insert("xhigh", pick(&["xhigh", "high", "medium", "low"]));
    let routing: HashMap<String, String> =
        route.into_iter().map(|(k, v)| (k.to_string(), v)).collect();

    let model = PublicModel {
        id: group.public_id.clone(),
        name: public_model_name(group),
        reasoning,
        levels: levels.iter().filter_map(|l| Effort::parse(l)).collect(),
        input_image: supports_images,
        context_window: template
            .as_ref()
            .map(|t| t.context_window)
            .unwrap_or(128000),
        max_tokens: template.as_ref().map(|t| t.max_tokens).unwrap_or(8192),
    };
    (model, routing, default_id)
}

fn default_of(group: &RuntimeGroup) -> String {
    for k in ["low", "minimal", "medium", "high"] {
        if let Some(id) = group.variants.get(k) {
            return id.clone();
        }
    }
    group
        .unsuffixed
        .clone()
        .unwrap_or_else(|| group.public_id.clone())
}

fn family_template(public_id: &str, fallback_models: &[PublicModel]) -> Option<PublicModel> {
    let lower = public_id.to_lowercase();
    if lower.starts_with("gemini-") && lower.contains("-flash") {
        return fallback_models
            .iter()
            .find(|m| m.id.starts_with("gemini-") && m.id.contains("-flash"))
            .cloned();
    }
    if lower.starts_with("gemini-") && lower.contains("-pro") {
        return fallback_models
            .iter()
            .find(|m| m.id.starts_with("gemini-") && m.id.contains("-pro"))
            .cloned();
    }
    if lower.starts_with("claude-opus") {
        return fallback_models
            .iter()
            .find(|m| m.id.starts_with("claude-opus"))
            .cloned();
    }
    if lower.starts_with("claude-") {
        return fallback_models
            .iter()
            .find(|m| m.id.starts_with("claude-sonnet"))
            .or_else(|| fallback_models.iter().find(|m| m.id.starts_with("claude-")))
            .cloned();
    }
    if lower.starts_with("gpt-oss") {
        return fallback_models
            .iter()
            .find(|m| m.id.starts_with("gpt-oss"))
            .cloned();
    }
    if lower.starts_with("gemini-") {
        return fallback_models
            .iter()
            .find(|m| m.id.starts_with("gemini-"))
            .cloned();
    }
    None
}

fn public_model_name(group: &RuntimeGroup) -> String {
    if let Some(family) = group
        .display_names
        .iter()
        .find_map(|n| display_family(Some(n)))
    {
        return format!("{} (Antigravity)", title_case(&family));
    }
    format!("{} (Antigravity)", humanize_public_id(&group.public_id))
}

fn title_case(value: &str) -> String {
    let mut out = String::new();
    let mut at_word_start = true;
    for c in value.chars() {
        if at_word_start && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
        at_word_start = c == ' ';
    }
    out
}

pub fn humanize_public_id(id: &str) -> String {
    let tokens: Vec<&str> = id.split('-').collect();
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if token.is_empty() {
            i += 1;
            continue;
        }
        let next = tokens.get(i + 1).copied();
        if token == "gpt" && next == Some("oss") {
            words.push("GPT-OSS".into());
            i += 2;
            continue;
        }
        if let Some(n) = next {
            if token.chars().all(|c| c.is_ascii_digit())
                && !token.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
                && !n.is_empty()
            {
                words.push(format!("{token}.{n}"));
                i += 2;
                continue;
            }
        }
        if token
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            words.push(token.to_uppercase());
        } else {
            let mut c = token.chars();
            let first = c.next().unwrap().to_uppercase();
            words.push(format!("{}{}", first, c.as_str()));
        }
        i += 1;
    }
    words.join(" ")
}

fn parse_gemini_version(id: &str) -> i64 {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)^gemini-(\d+)(?:\.(\d+))?").unwrap());
    let Some(caps) = re.captures(id) else {
        return 0;
    };
    let major: i64 = caps
        .get(1)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);
    let minor: i64 = caps
        .get(2)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);
    major * 1000 + minor
}

fn model_rank(id: &str) -> (i64, i64) {
    let version = parse_gemini_version(id);
    let lower = id.to_lowercase();
    if lower.starts_with("gemini-") && lower.contains("flash") && !lower.contains("pro") {
        return (0, -version);
    }
    if lower.starts_with("claude-opus") {
        return (1, 0);
    }
    if lower.starts_with("claude-sonnet") {
        return (2, 0);
    }
    if lower.starts_with("claude-") {
        return (3, 0);
    }
    if lower.starts_with("gemini-") && lower.contains("pro") {
        return (4, -version);
    }
    if lower.starts_with("gemini-") {
        return (5, -version);
    }
    if lower.starts_with("gpt-oss") {
        return (6, 0);
    }
    (7, 0)
}

fn compare_public_models(a: &PublicModel, b: &PublicModel) -> std::cmp::Ordering {
    let (ra, va) = model_rank(&a.id);
    let (rb, vb) = model_rank(&b.id);
    ra.cmp(&rb).then(va.cmp(&vb)).then(a.id.cmp(&b.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn routes_public_models() {
        assert_eq!(
            get_request_model_id("gemini-3.8-flash", Some(Effort::High)),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            get_request_model_id("gemini-3.8-flash", None),
            "gemini-3.8-flash-low"
        );
        assert_eq!(
            get_request_model_id("gemini-3.1-pro", Some(Effort::High)),
            "gemini-pro-agent"
        );
        assert_eq!(
            get_request_model_id("gemini-3.5-flash", Some(Effort::High)),
            "gemini-3-flash-agent"
        );
        assert_eq!(
            get_request_model_id("gemini-3.5-flash", Some(Effort::Low)),
            "gemini-3.5-flash-extra-low"
        );
        assert_eq!(
            get_request_model_id("claude-opus-4-6", Some(Effort::Low)),
            "claude-opus-4-6-thinking"
        );
        assert_eq!(
            get_request_model_id("gpt-oss-120b", Some(Effort::High)),
            "gpt-oss-120b-medium"
        );
        // runtime ids pass through
        assert_eq!(
            get_request_model_id("gemini-3.8-flash-medium", Some(Effort::Medium)),
            "gemini-3.8-flash-medium"
        );
        assert_eq!(get_request_model_id("custom-model", None), "custom-model");
    }

    #[test]
    fn thinking_budgets_match_upstream_table() {
        assert_eq!(
            get_thinking_config("gemini-3.8-flash-medium", Some(Effort::Medium)),
            Some((true, 4000))
        );
        assert_eq!(
            get_thinking_config("gemini-3.8-flash-high", Some(Effort::High)),
            Some((true, -1))
        );
        assert_eq!(
            get_thinking_config("gemini-3.5-flash-high", Some(Effort::High)),
            Some((true, 10000))
        );
        assert_eq!(
            get_thinking_config("gemini-3.5-flash-low", Some(Effort::Low)),
            Some((true, 1000))
        );
        assert_eq!(
            get_thinking_config("gemini-pro-agent", Some(Effort::High)),
            Some((true, 10001))
        );
        assert_eq!(
            get_thinking_config("gemini-3.1-pro-low", Some(Effort::Low)),
            Some((true, 1001))
        );
        assert_eq!(
            get_thinking_config("claude-sonnet-4-6", Some(Effort::High)),
            Some((true, 1024))
        );
        assert_eq!(
            get_thinking_config("claude-opus-4-6-thinking", Some(Effort::High)),
            Some((true, 1024))
        );
        assert_eq!(
            get_thinking_config("gpt-oss-120b-medium", Some(Effort::Medium)),
            Some((true, 8192))
        );
        assert_eq!(
            get_thinking_config("gemini-3.8-flash-medium", Some(Effort::Off)),
            Some((false, 0))
        );
        assert_eq!(
            get_thinking_config("gemini-3.8-flash-medium", None),
            Some((false, 0))
        );
    }

    #[test]
    fn max_output_tokens() {
        assert_eq!(
            get_max_output_tokens("gemini-3.8-flash", "gemini-3.8-flash-medium"),
            65536
        );
        assert_eq!(get_max_output_tokens("x", "claude-sonnet-4-6"), 64000);
        assert_eq!(get_max_output_tokens("x", "gpt-oss-120b-medium"), 32768);
        assert_eq!(get_max_output_tokens("x", "gemini-pro-agent"), 65535);
        assert_eq!(get_max_output_tokens("x", "unknown-model"), 8192);
    }

    #[test]
    fn fallback_chain() {
        assert_eq!(
            get_fallback_runtime_model("gemini-3.8-flash-high", None),
            Some("gemini-3.7-flash-high".into())
        );
        assert_eq!(
            get_fallback_runtime_model("gemini-3.7-flash-low", None),
            Some("gemini-3.6-flash-low".into())
        );
        assert_eq!(get_fallback_runtime_model("claude-sonnet-4-6", None), None);
    }

    #[test]
    fn effort_extraction() {
        let body = json!({"reasoning": {"effort": "high"}});
        assert_eq!(extract_effort(&body, "m"), Some(Effort::High));
        let body = json!({"reasoning_effort": "low"});
        assert_eq!(extract_effort(&body, "m"), Some(Effort::Low));
        assert_eq!(
            extract_effort(&json!({}), "gemini-3.8-flash-medium"),
            Some(Effort::Medium)
        );
        assert_eq!(extract_effort(&json!({}), "gemini-3.8-flash"), None);
    }

    #[test]
    fn groups_runtime_variants() {
        let raw = json!({
            "gemini-3.8-flash-low": {"displayName": "Gemini 3.8 Flash (Low)", "supportsImages": true, "model": "MODEL_PLACEHOLDER_M320"},
            "gemini-3.8-flash-medium": {"displayName": "Gemini 3.8 Flash (Medium)"},
            "gemini-3.8-flash-high": {"displayName": "Gemini 3.8 Flash (High)"},
            "gemini-pro-agent": {"displayName": "Gemini 3.1 Pro (High)"},
            "gemini-3.1-pro-low": {"displayName": "Gemini 3.1 Pro (Low)"},
            "chat_x": {"displayName": "hidden"},
            "gemini-3-pro-image": {"displayName": "image model"}
        });
        let fallback = fallback_catalog();
        let cat = build_catalog(&raw, &fallback);
        let m38 = cat
            .models
            .iter()
            .find(|m| m.id == "gemini-3.8-flash")
            .expect("3.8 grouped");
        assert_eq!(m38.levels.len(), 3);
        let route = cat.routing.get("gemini-3.8-flash").unwrap();
        assert_eq!(
            route.get("high").map(String::as_str),
            Some("gemini-3.8-flash-high")
        );
        // gemini-pro-agent merged into gemini-3.1-pro
        assert!(!cat.models.iter().any(|m| m.id == "gemini-pro-agent"));
        let pro = cat
            .models
            .iter()
            .find(|m| m.id == "gemini-3.1-pro")
            .expect("pro");
        assert!(pro.levels.contains(&Effort::Low) && pro.levels.contains(&Effort::High));
        assert_eq!(
            cat.routing
                .get("gemini-3.1-pro")
                .unwrap()
                .get("high")
                .map(String::as_str),
            Some("gemini-pro-agent")
        );
    }
}
