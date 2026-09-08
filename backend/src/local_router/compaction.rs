use super::*;

pub(crate) const COMPACTION_TIMEOUT: Duration = Duration::from_secs(120);

fn input_items(body: &Value) -> &[Value] {
    match body.get("input") {
        Some(Value::Array(items)) => items,
        Some(item @ Value::Object(_)) => std::slice::from_ref(item),
        _ => &[],
    }
}

pub(crate) fn is_compaction_request(body: &Value, kind: ResponsesRequestKind) -> bool {
    kind == ResponsesRequestKind::Compact
        || input_items(body)
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
}

pub(crate) fn validate_portable_context(body: &Value) -> Result<()> {
    let is_compaction = |item: &Value| {
        matches!(
            item.get("type").and_then(Value::as_str),
            Some("compaction" | "compaction_trigger")
        )
    };
    for item in input_items(body) {
        let content = item.get("content");
        if is_compaction(item)
            || content.is_some_and(|content| {
                is_compaction(content)
                    || content
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(is_compaction))
            })
        {
            anyhow::bail!(
                "context_not_portable: compaction 历史不能转换到当前线路；请回到原线路完成本地摘要后再切换"
            );
        }
    }
    Ok(())
}

// Only in-flight work is owned here. Codex remains responsible for history
// versions, retries and installing a successful compaction result.
pub(crate) struct CompactionGuard {
    bindings: Arc<Mutex<RouteBindings>>,
    keys: Vec<String>,
}

impl CompactionGuard {
    pub(crate) fn acquire(bindings: &Arc<Mutex<RouteBindings>>, keys: Vec<String>) -> Result<Self> {
        let mut state = bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if keys.iter().any(|key| state.compacting.contains(key)) {
            anyhow::bail!("同一会话已有压缩请求正在执行，请等待完成后重试");
        }
        state.compacting.extend(keys.iter().cloned());
        Ok(Self {
            bindings: Arc::clone(bindings),
            keys,
        })
    }
}

impl Drop for CompactionGuard {
    fn drop(&mut self) {
        let mut state = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in &self.keys {
            state.compacting.remove(key);
        }
    }
}

impl RouterServer {
    pub(crate) async fn proxy_with_compaction_budget<D: ResponsesDownstream + ?Sized>(
        &self,
        request: HttpRequest,
        body: Value,
        encoded_body: Option<Vec<u8>>,
        kind: ResponsesRequestKind,
        downstream: &mut D,
    ) -> Result<()> {
        if !is_compaction_request(&body, kind) {
            return self
                .proxy_parsed_responses_inner(request, body, encoded_body, kind, downstream)
                .await;
        }
        let keys = request_binding_keys(&request);
        // ponytail: without a session identifier only identical requests can be
        // deduplicated; a host revision is required for stronger idempotency.
        let keys = if keys.is_empty() {
            let bytes = serde_json::to_vec(&body)?;
            vec![format!("compact-input:{:x}", Sha256::digest(bytes))]
        } else {
            keys
        };
        let _guard = match CompactionGuard::acquire(&self.bindings, keys) {
            Ok(guard) => guard,
            Err(error) => {
                return downstream
                    .write_error(409, "compaction_in_progress", error.to_string(), None)
                    .await;
            }
        };
        // The reqwest deadline covers sending through reading the last upstream
        // byte. Never time out the downstream replay and append a JSON error to
        // an already-started SSE/HTTP response.
        self.proxy_parsed_responses_inner(request, body, encoded_body, kind, downstream)
            .await
    }
}

pub(crate) fn validate_compaction_result(value: &Value, v2: bool) -> Result<()> {
    check_context_length_error(value)?;
    // A valid candidate must also fit in the next request. Ciphertext byte
    // length cannot establish token count or semantic quality; Codex rechecks
    // the target model budget before installing/sending its history.
    bounded_json_bytes(value, MAX_REQUEST_BYTES)?;
    if v2 && value.get("status").and_then(Value::as_str) != Some("completed") {
        anyhow::bail!("远程压缩未成功完成");
    }
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("远程压缩缺少 output 数组"))?;
    let mut count = 0;
    for item in output {
        if item.get("type").and_then(Value::as_str).is_none() {
            anyhow::bail!("远程压缩包含无效的输出项");
        }
        if item["type"] == "compaction" {
            count += 1;
            if item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_none_or(|s| s.trim().is_empty())
            {
                anyhow::bail!("远程压缩缺少有效的 encrypted_content");
            }
        }
    }
    if count != 1 {
        anyhow::bail!("远程压缩必须返回且仅返回一个 compaction 项");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_guard_releases_on_failure_and_serializes_overlapping_sessions() {
        let bindings = Arc::new(Mutex::new(RouteBindings::default()));
        let guard =
            CompactionGuard::acquire(&bindings, vec!["thread:a".into(), "session:a".into()])
                .unwrap();
        assert!(CompactionGuard::acquire(&bindings, vec!["session:a".into()]).is_err());
        let other = CompactionGuard::acquire(&bindings, vec!["thread:b".into()]).unwrap();
        drop(guard);
        assert!(CompactionGuard::acquire(&bindings, vec!["thread:a".into()]).is_ok());
        drop(other);
        assert!(bindings.lock().unwrap().compacting.is_empty());
    }

    #[test]
    fn compaction_candidate_requires_complete_single_nonempty_snapshot() {
        let valid = json!({"status":"completed","output":[{"type":"compaction","encrypted_content":"opaque"}]});
        assert!(validate_compaction_result(&valid, true).is_ok());
        for invalid in [
            json!({"output":[]}),
            json!({"status":"incomplete","output":valid["output"]}),
            json!({"status":"completed","output":[{"type":"compaction","encrypted_content":""}]}),
            json!({"status":"completed","output":[valid["output"][0],valid["output"][0]]}),
            json!({"status":"completed","output":[{"type":"message","content":"summary"}]}),
        ] {
            assert!(validate_compaction_result(&invalid, true).is_err());
        }
        let mut accumulator = CompactionAccumulator::default();
        assert!(parse_sse_frames(b"data: [DONE]\n\n", &mut accumulator).is_err());
        assert!(!accumulator.finished());
        let failure = json!({"type":"response.failed","response":{"error":{"code":"context_length_exceeded"}}});
        assert!(
            accumulator
                .ingest_frame(&failure.to_string(), false)
                .unwrap_err()
                .is::<ContextLengthExceeded>()
        );
        let oversized = json!({"status":"completed","output":[{"type":"compaction","encrypted_content":"x".repeat(MAX_REQUEST_BYTES)}]});
        assert!(validate_compaction_result(&oversized, true).is_err());
    }

    #[test]
    fn compaction_tool_parts_do_not_restore_filtered_ciphertext() {
        for mixed in [
            json!([{"type":"encrypted_content","encrypted_content":"secret"},{"type":"unknown","value":1}]),
            json!([17,{"type":"encrypted_content","encrypted_content":"secret"}]),
            json!([{"type":"text","text":"visible"},{"custom":"json"}]),
            json!([{"type":"compaction","encrypted_content":"secret"}]),
        ] {
            assert!(responses_tool_output_content(&mixed).is_err());
        }
        assert!(
            responses_tool_output_content(&json!({"type":"custom_json","value":17}))
                .unwrap()
                .is_none()
        );
        assert_eq!(responses_tool_output_content(&json!([{"type":"text","text":"visible"},{"type":"reasoning","encrypted_content":"secret"}])).unwrap().unwrap().0, "visible");
    }

    #[test]
    fn compaction_context_errors_survive_json_and_stream_adapters() {
        for error in [
            json!({"error":{"code":"context_length_exceeded","message":"limit"}}),
            json!({"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 100 > 50"}}),
        ] {
            assert!(
                check_context_length_error(&error)
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                ChatSseAccumulator::new("model")
                    .ingest(&error)
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                AnthropicSseAccumulator::new("model")
                    .ingest(&error)
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                chat_completion_to_responses_body(error.clone(), "model")
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                anthropic_message_to_responses_body(&error, "model")
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
        }
        assert!(!is_context_length_error(
            &json!({"error":{"message":"image too large","code":"invalid_request_error"}})
        ));
    }
}

#[derive(Default)]
struct CompactionAccumulator {
    response: Option<Value>,
}
impl SseFrameAccumulator for CompactionAccumulator {
    const PROTOCOL_LABEL: &'static str = "Responses compaction";
    const READ_OPERATION: &'static str = "读取远程压缩响应失败";
    fn ingest_frame(&mut self, data: &str, trailing: bool) -> Result<()> {
        let event = sse_json_frame(data, Self::PROTOCOL_LABEL, trailing)?;
        check_context_length_error(&event)?;
        match event.get("type").and_then(Value::as_str) {
            Some("response.completed") => {
                let mut response = event["response"].clone();
                // The event itself establishes completion, even if a provider
                // omits the redundant response.status field.
                if response.get("status").is_none() {
                    response["status"] = json!("completed");
                }
                validate_compaction_result(&response, true)?;
                self.response = Some(response);
            }
            Some("response.failed" | "response.incomplete" | "error") => {
                anyhow::bail!("远程压缩返回失败或未完成终态")
            }
            _ => {}
        }
        Ok(())
    }
    fn finished(&self) -> bool {
        self.response.is_some()
    }
}

pub(crate) async fn write_validated_compaction<D: ResponsesDownstream + ?Sized>(
    downstream: &mut D,
    response: reqwest::Response,
    v2: bool,
    stream: bool,
    route: &RouteTarget,
) -> Result<()> {
    let probe = downstream.request_log_probe().cloned();
    let result = await_upstream(downstream, async {
        let mut prepared =
            prepare_upstream_response(response, "读取远程压缩响应失败", probe.as_ref()).await?;
        if prepared.is_sse {
            if !v2 {
                anyhow::bail!("旧版压缩端点必须返回 JSON");
            }
            let mut accumulator = CompactionAccumulator::default();
            collect_sse_frames(&mut prepared, &mut accumulator, probe.as_ref()).await?;
            accumulator
                .response
                .ok_or_else(|| anyhow::anyhow!("远程压缩未返回完成事件"))
        } else {
            let bytes = read_bounded_prepared_upstream_body(
                &mut prepared,
                MAX_REQUEST_BYTES,
                "读取远程压缩响应失败",
                probe.as_ref(),
            )
            .await?;
            let value: Value = serde_json::from_slice(&bytes).context("远程压缩返回无效 JSON")?;
            validate_compaction_result(&value, v2)?;
            Ok(value)
        }
    })
    .await?;
    match result {
        Ok(value) if stream => write_responses_response_as_events(downstream, &value).await,
        Ok(value) => downstream.write_json(200, &value).await,
        Err(error) => {
            let timeout = error
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
                || error.is::<UpstreamReadIdleTimeout>();
            let code = if timeout {
                "compaction_timeout"
            } else if error.is::<ContextLengthExceeded>() {
                CONTEXT_LENGTH_EXCEEDED
            } else {
                "invalid_compaction_response"
            };
            downstream
                .write_error(
                    if timeout { 504 } else { 502 },
                    code,
                    error.to_string(),
                    Some(route),
                )
                .await
        }
    }
}
