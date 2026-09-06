use super::*;

/// Relays a successful Chat Completions or Anthropic Messages response as
/// Responses output. Both adapted protocols share one shape: sniff the body,
/// stream SSE when the client asked for a stream, otherwise collect the full
/// message and emit it as JSON or as a replayed event list.
pub(crate) async fn write_adapted_upstream_as_responses<D>(
    downstream: &mut D,
    response: reqwest::Response,
    bridge: ProtocolBridge,
    model: &str,
    stream_requested: bool,
    route: &RouteTarget,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    debug_assert!(response.status().is_success());
    let anthropic = bridge == ProtocolBridge::ResponsesToAnthropicMessages;
    let (protocol_label, read_operation) = if anthropic {
        ("Anthropic Messages", "读取 Anthropic Messages 上游响应失败")
    } else {
        ("Chat Completions", "读取 Chat Completions 上游响应失败")
    };
    let conversion_error = |error: anyhow::Error| {
        format!(
            "线路「{}」的 {protocol_label} 响应无法转换为 Responses：{error:#}",
            route_display_name(route)
        )
    };
    let probe = downstream.request_log_probe().cloned();
    let prepared = match await_upstream(
        downstream,
        prepare_upstream_response(response, read_operation, probe.as_ref()),
    )
    .await?
    {
        Ok(prepared) => prepared,
        Err(error) => {
            return downstream
                .write_error(
                    502,
                    "upstream_protocol_error",
                    conversion_error(error),
                    Some(route),
                )
                .await;
        }
    };
    if stream_requested && prepared.is_sse {
        return if anthropic {
            stream_anthropic_messages_as_responses(downstream, prepared, model, route, tool_bridge)
                .await
        } else {
            stream_chat_completions_as_responses(downstream, prepared, model, route, tool_bridge)
                .await
        };
    }
    let collected = if anthropic {
        await_upstream(
            downstream,
            read_anthropic_messages_as_responses(prepared, model, tool_bridge, probe.as_ref()),
        )
        .await?
    } else {
        await_upstream(
            downstream,
            read_chat_completions_as_responses(prepared, model, tool_bridge, probe.as_ref()),
        )
        .await?
    };
    let responses = match collected {
        Ok(responses) => responses,
        Err(error) => {
            return downstream
                .write_error(
                    502,
                    "upstream_protocol_error",
                    conversion_error(error),
                    Some(route),
                )
                .await;
        }
    };
    if stream_requested {
        write_responses_response_as_events(downstream, &responses).await
    } else {
        downstream.write_json(200, &responses).await
    }
}

pub(crate) async fn read_chat_completions_as_responses(
    mut prepared: PreparedUpstreamResponse,
    model: &str,
    tool_bridge: &ResponsesToolBridge,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let chat = if prepared.is_sse {
        collect_chat_completion_sse(&mut prepared, model, probe).await?
    } else {
        let body = read_bounded_prepared_upstream_body(
            prepared,
            MAX_UPSTREAM_RESPONSE_BYTES,
            "读取 Chat Completions 上游响应失败",
            probe,
        )
        .await?;
        match serde_json::from_slice::<Value>(&body) {
            Ok(chat) => chat,
            Err(json_error) if body.starts_with(b"data:") || body.windows(6).any(|w| w == b"\ndata:") => {
                parse_chat_completion_sse_bytes(&body, model).with_context(|| {
                    format!("Chat Completions 上游响应既不是有效 JSON，也无法作为 SSE 解析：{json_error}")
                })?
            }
            Err(error) => return Err(error).context("Chat Completions 上游响应不是有效 JSON"),
        }
    };
    chat_completion_to_responses_body_with_tool_bridge(chat, model, tool_bridge)
}

pub(crate) async fn read_bounded_upstream_error_body(
    mut response: reqwest::Response,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) =
        read_upstream_chunk(&mut response, "读取上游错误响应失败", probe).await?
    {
        let remaining = MAX_UPSTREAM_ERROR_BYTES.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() == MAX_UPSTREAM_ERROR_BYTES {
            break;
        }
    }
    Ok(body)
}

pub(crate) async fn read_anthropic_messages_as_responses(
    mut prepared: PreparedUpstreamResponse,
    model: &str,
    tool_bridge: &ResponsesToolBridge,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let message = if prepared.is_sse {
        collect_anthropic_message_sse(&mut prepared, model, probe).await?
    } else {
        let body = read_bounded_prepared_upstream_body(
            prepared,
            MAX_UPSTREAM_RESPONSE_BYTES,
            "读取 Anthropic Messages 上游响应失败",
            probe,
        )
        .await?;
        match serde_json::from_slice::<Value>(&body) {
            Ok(message) => message,
            Err(json_error)
                if body.starts_with(b"event:")
                    || body.starts_with(b"data:")
                    || body.windows(7).any(|window| window == b"\nevent:")
                    || body.windows(6).any(|window| window == b"\ndata:") =>
            {
                parse_anthropic_message_sse_bytes(&body, model).with_context(|| {
                    format!(
                        "Anthropic Messages 上游响应既不是有效 JSON，也无法作为 SSE 解析：{json_error}"
                    )
                })?
            }
            Err(error) => {
                return Err(error).context("Anthropic Messages 上游响应不是有效 JSON");
            }
        }
    };
    anthropic_message_to_responses_body_with_tool_bridge(&message, model, tool_bridge)
}
