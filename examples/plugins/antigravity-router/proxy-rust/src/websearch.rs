// Independent Google grounding plus the Responses web_search bridge.
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MODELS: [&str; 3] = ["gemini-3-flash", "gemini-3.6-flash-low", "gemini-2.5-flash"];
const SYSTEM: &str = "You are an expert deep-research investigator and technical analyst. Use Google Search Grounding for rich, high-signal, multi-perspective facts. Formulate distinct targeted queries; prioritize technical details, benchmarks, developer issues and community feedback. Structure clean Markdown with direct source citations.";
pub const SEARCH_TIMEOUT_SECS: u64 = 90;

#[derive(Default, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub instruction: Option<String>,
    pub urls: Vec<String>,
    pub thinking: bool,
}
impl SearchOptions {
    pub fn from_value(v: &Value) -> Result<Self, String> {
        let query = v
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if query.is_empty() || query.len() > 32000 {
            return Err("query is required and must be at most 32000 bytes".into());
        }
        let instruction = match v.get("instruction") {
            None => None,
            Some(Value::String(s)) if s.len() <= 32000 => Some(s.clone()),
            _ => return Err("instruction must be a string at most 32000 bytes".into()),
        };
        let mut urls = vec![];
        if let Some(values) = v.get("urls") {
            let values = values.as_array().ok_or("urls must be an array")?;
            if values.len() > 20 {
                return Err("at most 20 urls are allowed".into());
            }
            for value in values {
                let raw = value.as_str().ok_or("urls must contain strings")?;
                let u = url::Url::parse(raw).map_err(|_| "invalid URL")?;
                if !matches!(u.scheme(), "https" | "http")
                    || !u.username().is_empty()
                    || u.password().is_some()
                    || raw.len() > 8192
                {
                    return Err("urls must be HTTP(S) without credentials".into());
                }
                urls.push(raw.to_string());
            }
        }
        let thinking = match v.get("thinking") {
            None => false,
            Some(Value::Bool(b)) => *b,
            _ => return Err("thinking must be boolean".into()),
        };
        Ok(Self {
            query,
            instruction,
            urls,
            thinking,
        })
    }
}
pub fn build_search_request(opts: &SearchOptions, model: &str, project: &str) -> Value {
    let mut system = SYSTEM.to_string();
    let mut prompt = opts.query.trim().to_string();
    if let Some(directive) = opts
        .instruction
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        system.push_str(&format!("\n\n[SPECIFIC DIRECTIVE FROM LEAD AGENT]:\n{directive}\nYou MUST strictly follow this directive during search query formulation and synthesis."));
        prompt = format!("[Lead Agent Directive]: {directive}\n\nSearch Query: {prompt}");
    }
    let mut tools = vec![json!({"googleSearch":{}})];
    if !opts.urls.is_empty() {
        prompt.push_str(&format!(
            "\n\nURLs to analyze in detail:\n{}",
            opts.urls.join("\n")
        ));
        tools.push(json!({"urlContext":{}}));
    }
    let (id, _, _) = crate::convert::antigravity_request_envelope(
        model,
        false,
        false,
        1,
        "0".into(),
        0,
        None,
        None,
    );
    json!({"project":project,"model":model,"request":{"systemInstruction":{"role":"user","parts":[{"text":system}]},"contents":[{"role":"user","parts":[{"text":prompt}]}],"tools":tools,"generationConfig":{"thinkingConfig":{"thinkingBudget":if opts.thinking {4096} else {2048},"includeThoughts":false}}},"requestType":"agent","userAgent":"antigravity","requestId":id})
}
pub fn parse_search_response(data: &Value) -> Value {
    let data = data.get("response").unwrap_or(data);
    let candidate = data.pointer("/candidates/0");
    let Some(c) = candidate else {
        return json!({"text":"", "sources":[], "queries":[]});
    };
    let text = c
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let sources=c.pointer("/groundingMetadata/groundingChunks").and_then(Value::as_array).map(|p|p.iter().filter_map(|p| {let w=p.get("web")?;let url=w.get("uri")?.as_str()?;Some(json!({"title":w.get("title").and_then(Value::as_str).filter(|s|!s.trim().is_empty()).unwrap_or(url),"url":url}))}).collect::<Vec<_>>()).unwrap_or_default();
    let queries = c
        .pointer("/groundingMetadata/webSearchQueries")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter(|p| p.is_string())
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({"text":text,"sources":sources,"queries":queries})
}
pub fn format_search_result(v: &Value) -> String {
    let mut parts = vec![];
    if let Some(s) = v
        .get("text")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(s.to_string());
    }
    if let Some(a) = v
        .get("sources")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        parts.push(format!(
            "### Sources\n{}",
            a.iter()
                .map(|s| format!(
                    "- [{}]({})",
                    s["title"].as_str().unwrap_or("source"),
                    s["url"].as_str().unwrap_or("")
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let Some(a) = v
        .get("queries")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        parts.push(format!(
            "*Search queries: {}*",
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.join("\n\n")
}
pub async fn execute_search(
    token: &str,
    project: &str,
    opts: &SearchOptions,
    cancel: CancellationToken,
) -> Result<Value, String> {
    tokio::select! { biased; _=cancel.cancelled()=>Err("search cancelled".into()), result=tokio::time::timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS), execute_inner(token,project,opts,cancel.clone()))=>result.map_err(|_|"search timeout".to_string())? }
}
async fn execute_inner(
    token: &str,
    project: &str,
    opts: &SearchOptions,
    cancel: CancellationToken,
) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(crate::security::safe_error)?;
    let endpoints = crate::discovery::endpoint_candidates()?;
    let mut last = "no search endpoint available".to_string();
    for model in MODELS {
        let body = build_search_request(opts, model, project);
        for endpoint in &endpoints {
            let response = tokio::select! {biased; _=cancel.cancelled()=>return Err("search cancelled".into()), r=client.post(format!("{endpoint}/v1internal:generateContent")).headers(crate::upstream::antigravity_headers(token)).json(&body).timeout(Duration::from_secs(30)).send()=>r};
            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    last = crate::security::safe_error(e);
                    continue;
                }
            };
            let status = response.status().as_u16();
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            while let Some(chunk) = tokio::select! {biased; _=cancel.cancelled()=>return Err("search cancelled".into()), r=stream.next()=>r}
            {
                let chunk = chunk.map_err(crate::security::safe_error)?;
                if chunk.len() > 4_194_304usize.saturating_sub(bytes.len()) {
                    return Err("search response exceeds cap".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            if !(200..300).contains(&status) {
                let data: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                let text = String::from_utf8_lossy(&bytes);
                last = crate::security::redact_secrets(
                    data.pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or(&text),
                )
                .chars()
                .take(400)
                .collect();
                if matches!(status, 404 | 429 | 500 | 502 | 503 | 504) {
                    continue;
                }
                return Err(format!("search HTTP {status}: {last}"));
            }
            let data: Value = serde_json::from_slice(&bytes)
                .map_err(|_| "invalid search response".to_string())?;
            let result = parse_search_response(&data);
            if result["text"]
                .as_str()
                .unwrap_or("")
                .contains("is no longer available. Please switch")
            {
                break;
            }
            if data.get("error").is_some()
                || (result["text"].as_str().unwrap_or("").is_empty()
                    && result["sources"]
                        .as_array()
                        .map(Vec::is_empty)
                        .unwrap_or(true))
            {
                return Err("no search candidate returned".into());
            }
            let markdown = format_search_result(&result);
            let mut result = result;
            result["result"] = json!(markdown);
            return Ok(result);
        }
    }
    Err(format!("Antigravity Google Search failed: {last}"))
}
pub async fn run_web_search(
    token: &str,
    project: &str,
    query: &str,
    _model: &str,
    cancel: CancellationToken,
) -> String {
    let opts = SearchOptions {
        query: query.chars().take(2000).collect(),
        ..Default::default()
    };
    match execute_search(token, project, &opts, cancel).await {
        Ok(r) => r["result"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(6000)
            .collect(),
        Err(e) => format!("Web search failed: {}", crate::security::safe_error(e)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn options_wire_and_grounding() {
        let opts=SearchOptions::from_value(&json!({"query":"Rust","instruction":"benchmarks","urls":["https://example.com"],"thinking":true})).unwrap();
        let v = build_search_request(&opts, "gemini-3-flash", "p");
        assert_eq!(v["request"]["tools"].as_array().unwrap().len(), 2);
        assert_eq!(
            v["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            4096
        );
        let r = parse_search_response(
            &json!({"response":{"candidates":[{"content":{"parts":[{"text":"hidden","thought":true},{"text":"answer"}]},"groundingMetadata":{"webSearchQueries":["q",5],"groundingChunks":[{"web":{"uri":"https://example.com","title":"example"}}]}}]}}),
        );
        assert_eq!(r["text"], "answer");
        assert_eq!(r["queries"], json!(["q"]));
        assert!(format_search_result(&r).contains("### Sources"));
    }
    #[test]
    fn options_fail_fast() {
        for v in [
            json!({"query":""}),
            json!({"query":"q","thinking":"yes"}),
            json!({"query":"q","urls":["file:///x"]}),
        ] {
            assert!(SearchOptions::from_value(&v).is_err());
        }
    }
    #[tokio::test]
    async fn cancelled_search_does_not_send() {
        let c = CancellationToken::new();
        c.cancel();
        assert!(execute_search(
            "unused",
            "p",
            &SearchOptions {
                query: "q".into(),
                ..Default::default()
            },
            c
        )
        .await
        .unwrap_err()
        .contains("cancelled"));
    }
}
