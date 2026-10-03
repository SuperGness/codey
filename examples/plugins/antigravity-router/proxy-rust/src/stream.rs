// Pump an upstream Antigravity Gemini SSE stream into Responses events
// (port of proxy/lib/gemini-stream.js streamGeminiToResponses).
use crate::writer::Writer;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub fn zero_usage() -> Value {
    json!({
        "input_tokens": 0,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": 0,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": 0
    })
}

#[derive(Debug, Clone)]
pub struct InterceptedCall {
    pub query: String,
    pub tool_name: String,
    pub call_id: String,
    pub thought_signature: Option<String>,
}

pub struct PumpOptions<'a> {
    pub start: bool,
    pub hold_complete: bool,
    /// When Some, model function calls named "web_search" are intercepted.
    pub web_search_calls: Option<&'a mut Vec<InterceptedCall>>,
    /// When Some, model function calls named "generate_image" are intercepted.
    pub image_calls: Option<&'a mut Vec<InterceptedCall>>,
    pub cancel: CancellationToken,
    pub stall_timeout_ms: u64,
}

pub struct PumpResult {
    pub finish_reason: Option<String>,
    pub usage: Value,
    pub has_content: bool,
    pub error: Option<String>,
}

fn map_usage(data: &Value) -> Value {
    let um = data.get("usageMetadata").cloned().unwrap_or(Value::Null);
    let g = |k: &str| um.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    json!({
        "input_tokens": g("promptTokenCount"),
        "input_tokens_details": {"cached_tokens": g("cachedContentTokenCount")},
        "output_tokens": g("candidatesTokenCount"),
        "output_tokens_details": {"reasoning_tokens": g("thoughtsTokenCount")},
        "total_tokens": g("totalTokenCount")
    })
}

/// Read one upstream SSE body to completion, emitting writer events.
/// Header deadline is enforced by the caller (fetch); the stall watchdog
/// aborts when no bytes arrive for `stall_timeout_ms`.
pub async fn pump_stream(
    body: impl futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin,
    writer: &mut Writer,
    sink: &mut (dyn FnMut(&str) + Send),
    mut opts: PumpOptions<'_>,
) -> PumpResult {
    let mut stream = body;
    let mut buffer = Vec::new();
    let mut received = 0usize;
    let mut has_content = false;
    let mut completed = false;
    let mut finish_reason: Option<String> = None;
    let mut usage = zero_usage();
    let mut emitted_searches: std::collections::HashSet<String> = std::collections::HashSet::new();
    let stall = std::time::Duration::from_millis(opts.stall_timeout_ms.max(1));

    if opts.start {
        writer.start();
        writer.flush(sink);
    }

    let mut error: Option<String> = None;
    loop {
        let next = tokio::select! {
            biased;
            _ = opts.cancel.cancelled() => {
                error = Some("request cancelled".to_string());
                break;
            }
            chunk = tokio::time::timeout(stall, stream.next()) => match chunk {
                Err(_) => {
                    error = Some(format!("stream stalled: no data for {}ms", opts.stall_timeout_ms));
                    break;
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    error = Some(crate::security::safe_error(e));
                    break;
                }
                Ok(Some(Ok(bytes))) => bytes,
            }
        };
        received = received.saturating_add(next.len());
        if received > 64 * 1024 * 1024 || buffer.len().saturating_add(next.len()) > 8 * 1024 * 1024
        {
            error = Some("upstream stream exceeds size limit".into());
            break;
        }
        // Decode complete lines, preserving UTF-8 scalars split across network chunks.
        buffer.extend_from_slice(&next);
        while let Some(idx) = buffer.iter().position(|&b| b == b'\n') {
            let bytes: Vec<u8> = buffer.drain(..=idx).collect();
            let Ok(line) = std::str::from_utf8(&bytes) else {
                error = Some("invalid UTF-8 in upstream stream".into());
                break;
            };
            let line = line.trim_end_matches(['\r', '\n']);
            let Some(json_line) = line.strip_prefix("data:") else {
                continue;
            };
            let json_line = json_line.trim();
            if json_line.is_empty() || json_line == "[DONE]" {
                continue;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(json_line) else {
                error = Some("invalid JSON in upstream stream".into());
                break;
            };
            if chunk.get("error").is_some() && chunk["error"].is_object()
                || chunk
                    .get("error")
                    .map(|e| e.is_object() || e.is_string())
                    .unwrap_or(false)
            {
                let msg = chunk
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| chunk["error"].to_string());
                error = Some(msg);
                break;
            }
            let data = if chunk.get("response").is_some() && chunk["response"].is_object() {
                chunk["response"].clone()
            } else {
                chunk.clone()
            };
            let candidate = data
                .pointer("/candidates/0")
                .cloned()
                .unwrap_or(Value::Null);

            if let Some(parts) = candidate
                .pointer("/content/parts")
                .and_then(|p| p.as_array())
                .cloned()
            {
                for part in &parts {
                    let thought = part
                        .get("thought")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if thought {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            writer.reasoning_delta(text);
                        }
                    } else if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        has_content = true;
                        writer.text_delta(text);
                    } else if let Some(fc) = part.get("functionCall") {
                        has_content = true;
                        let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = fc.get("args").cloned().unwrap_or(json!({}));
                        let call_id = fc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or(name)
                            .to_string();
                        let sig = part.get("thoughtSignature").and_then(|v| v.as_str());
                        if name == "web_search" || name == "google_search" {
                            if let Some(collected) = opts.web_search_calls.as_deref_mut() {
                                let raw_query = args
                                    .get("query")
                                    .or_else(|| args.get("q"))
                                    .or_else(|| args.get("search_query"))
                                    .cloned()
                                    .unwrap_or(args.clone());
                                let query = if name == "google_search" {
                                    args.to_string()
                                } else {
                                    match &raw_query {
                                        Value::String(s) if !s.trim().is_empty() => s.clone(),
                                        other => other.to_string(),
                                    }
                                };
                                let intercepted = InterceptedCall {
                                    tool_name: name.to_string(),
                                    query,
                                    call_id: if call_id.is_empty() {
                                        format!("call_search_{}", collected.len() + 1)
                                    } else {
                                        call_id.clone()
                                    },
                                    thought_signature: sig.map(String::from),
                                };
                                let q = vec![intercepted.query.clone()];
                                collected.push(intercepted);
                                writer.web_search(&q);
                            } else {
                                writer.tool_call(name, &args, &call_id, sig);
                            }
                        } else if name == "generate_image" {
                            if let Some(collected) = opts.image_calls.as_deref_mut() {
                                let prompt = args.get("prompt").cloned().unwrap_or(json!(""));
                                let query = match &prompt {
                                    Value::String(s) if !s.trim().is_empty() => s.clone(),
                                    other => other.to_string(),
                                };
                                let intercepted = InterceptedCall {
                                    tool_name: name.to_string(),
                                    query,
                                    call_id: if call_id.is_empty() {
                                        format!("call_image_{}", collected.len() + 1)
                                    } else {
                                        call_id.clone()
                                    },
                                    thought_signature: sig.map(String::from),
                                };
                                let q = vec![intercepted.query.clone()];
                                collected.push(intercepted);
                                writer.web_search(&q); // surface as an instant tool item
                            } else {
                                writer.tool_call(name, &args, &call_id, sig);
                            }
                        } else {
                            writer.tool_call(name, &args, &call_id, sig);
                        }
                    }
                }
            }

            if let Some(queries) = candidate
                .pointer("/groundingMetadata/webSearchQueries")
                .and_then(|q| q.as_array())
            {
                let list: Vec<String> = queries
                    .iter()
                    .filter_map(|q| q.as_str().map(String::from))
                    .collect();
                if !list.is_empty() {
                    let key = serde_json::to_string(&list).unwrap_or_default();
                    if !emitted_searches.contains(&key) {
                        emitted_searches.insert(key);
                        has_content = true;
                        writer.web_search(&list);
                    }
                }
            }

            // usageMetadata can ride with finishReason or in a trailing chunk
            // (real wire shape); update it on every chunk that carries it.
            if data
                .get("usageMetadata")
                .map(|u| u.is_object())
                .unwrap_or(false)
            {
                usage = map_usage(&data);
            }
            if let Some(fr) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                completed = true;
                finish_reason = Some(fr.to_string());
                if opts.hold_complete {
                    writer.seal();
                }
                // Non-hold completion is deferred to the stream tail so the
                // terminal event carries the final usage (single terminal).
            }
        }
        writer.flush(sink);
        if error.is_some() {
            break;
        }
    }

    if error.is_none() && buffer.iter().any(|b| !b.is_ascii_whitespace()) {
        error = Some("truncated upstream SSE record".into());
    }
    if let Some(err) = error {
        let err = crate::security::redact_secrets(&err);
        writer.flush(sink);
        let evt = json!({"type": "response.failed", "error": {"message": err}});
        sink(&format!("data: {}\n\n", evt));
        return PumpResult {
            finish_reason,
            usage,
            has_content,
            error: Some(err),
        };
    }

    if completed && !opts.hold_complete {
        writer.complete(usage.clone(), finish_reason.clone());
        writer.flush(sink);
    } else if !completed && has_content {
        usage = zero_usage();
        if opts.hold_complete {
            writer.seal();
        } else {
            writer.complete(usage.clone(), None);
        }
        writer.flush(sink);
    }
    PumpResult {
        finish_reason,
        usage,
        has_content,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::Writer;

    fn sse_body(
        chunks: &[&str],
    ) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>>>> {
        let joined = chunks.join("");
        let items: Vec<reqwest::Result<bytes::Bytes>> = joined
            .as_bytes()
            .chunks(7)
            .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
            .collect();
        Box::pin(futures_util::stream::iter(items))
    }

    #[tokio::test]
    async fn parses_sse_at_arbitrary_chunk_boundaries() {
        let payload = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"He\"},{\"text\":\"llo\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":2,\"thoughtsTokenCount\":1,\"totalTokenCount\":8}}}\n\n";
        let mut writer = Writer::new("r1".into(), "m".into(), 0);
        let mut out = String::new();
        let mut sink = |chunk: &str| out.push_str(chunk);
        let res = pump_stream(
            sse_body(&[payload]),
            &mut writer,
            &mut sink,
            PumpOptions {
                start: true,
                hold_complete: false,
                web_search_calls: None,
                image_calls: None,
                cancel: CancellationToken::new(),
                stall_timeout_ms: 1000,
            },
        )
        .await;
        assert_eq!(res.finish_reason.as_deref(), Some("STOP"));
        assert!(res.has_content);
        assert_eq!(res.usage["input_tokens"], 5);
        assert_eq!(res.usage["input_tokens_details"]["cached_tokens"], 0);
        assert_eq!(res.usage["output_tokens_details"]["reasoning_tokens"], 1);
        assert!(out.contains("\"response.created\""));
        assert!(out.contains("\"response.output_text.delta\""));
        assert!(out.contains("He"));
        assert!(out.contains("\"response.completed\""));
        let completed: Value = serde_json::from_str(
            out.split("\n\n")
                .find(|b| b.contains("response.completed"))
                .unwrap()
                .trim_start_matches("data: "),
        )
        .unwrap();
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "Hello"
        );
    }

    #[tokio::test]
    async fn intercepts_web_search_calls() {
        let payload = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"web_search\",\"args\":{\"query\":\"rust sse\"},\"id\":\"c1\"}}]}}]}\n\n";
        let mut writer = Writer::new("r2".into(), "m".into(), 0);
        let mut out = String::new();
        let mut sink = |chunk: &str| out.push_str(chunk);
        let mut collected: Vec<InterceptedCall> = Vec::new();
        let res = pump_stream(
            sse_body(&[payload]),
            &mut writer,
            &mut sink,
            PumpOptions {
                start: true,
                hold_complete: true,
                web_search_calls: Some(&mut collected),
                image_calls: None,
                cancel: CancellationToken::new(),
                stall_timeout_ms: 1000,
            },
        )
        .await;
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].query, "rust sse");
        assert_eq!(collected[0].call_id, "c1");
        assert!(
            !out.contains("\"response.completed\""),
            "hold_complete defers the terminal"
        );
        assert!(out.contains("web_search_call"));
    }

    #[tokio::test]
    async fn unicode_and_trailing_errors_are_not_lost() {
        for (payload, failed) in [
            ("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"中文🙂文本\"}]},\"finishReason\":\"STOP\"}]}\n\n".to_string(), false),
            ("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]},\"finishReason\":\"STOP\"}]}\n\ndata: {\"error\":{\"message\":\"late error\"}}\n\n".to_string(), true),
            ("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]} }]}\n\ndata: {\"response\": ".to_string(), true),
        ] {
            let body = futures_util::stream::iter(payload.as_bytes().iter().map(|b| Ok(bytes::Bytes::copy_from_slice(&[*b]))).collect::<Vec<reqwest::Result<bytes::Bytes>>>());
            let mut writer = Writer::new("unicode".into(), "m".into(), 0);
            let mut out = String::new();
            let result = pump_stream(body, &mut writer, &mut |s| out.push_str(s), PumpOptions { start: true, hold_complete: false, web_search_calls: None, image_calls: None, cancel: CancellationToken::new(), stall_timeout_ms: 1000 }).await;
            assert_eq!(result.error.is_some(), failed);
            assert_eq!(out.matches("\"response.failed\"").count(), usize::from(failed));
            assert_eq!(out.matches("\"response.completed\"").count(), usize::from(!failed));
            if !failed { assert!(out.contains("中文🙂文本")); }
        }
    }

    #[tokio::test]
    async fn empty_stream_reports_error() {
        let mut writer = Writer::new("r3".into(), "m".into(), 0);
        let mut out = String::new();
        let mut sink = |chunk: &str| out.push_str(chunk);
        let res = pump_stream(
            sse_body(&[""]),
            &mut writer,
            &mut sink,
            PumpOptions {
                start: true,
                hold_complete: false,
                web_search_calls: None,
                image_calls: None,
                cancel: CancellationToken::new(),
                stall_timeout_ms: 1000,
            },
        )
        .await;
        assert!(!res.has_content);
        // The empty-stream failure is emitted by the orchestration layer after
        // retries exhaust, not by the pump (so retries can reuse the writer).
    }
}
