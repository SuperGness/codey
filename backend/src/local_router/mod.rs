use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hyper_util::client::proxy::matcher::Matcher as SystemProxyMatcher;
use reqwest::header::{
    AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue,
};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse as WebSocketErrorResponse, Request as WebSocketRequest,
    Response as WebSocketResponse,
};
use tokio_tungstenite::tungstenite::http::{
    StatusCode as WebSocketStatusCode, Uri as WebSocketUri,
};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{
    Error as WebSocketError, Message as WebSocketMessage, Utf8Bytes as WebSocketText,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, accept_hdr_async_with_config, connect_async_with_config,
};
use uuid::Uuid;

use crate::codex_config::CHATGPT_CODEX_BASE_URL;
use crate::config::{
    CodeyConfig, ProviderProfile, RouteRequestLogBackend, UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS, UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
};
use crate::model_id;
use crate::route_request_log::{
    FirstByteSource, RequestProtocol, RouteRequestLogClearResult, RouteRequestLogController,
    RouteRequestLogGuard, RouteRequestLogProbe, RouteRequestLogQuery, RouteRequestLogReconfigure,
    RouteRequestLogStart, UpstreamTransport,
};

pub(crate) const ROUTER_PROVIDER_ID: &str = "codey_router";
pub(crate) const ROUTER_AUTH_HEADER: &str = "x-codey-router-token";
const TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";
const ROUTE_METADATA_KEY: &str = "codey_route";
const CHATGPT_ACCOUNT_ID_HEADER: &str = "chatgpt-account-id";
const PROMPT_CACHE_KEY_HEADER: &str = "prompt-cache-key";
const PROMPT_CACHE_KEY_COMPAT_HEADER: &str = "prompt_cache_key";
const PROMPT_CACHE_KEY_BODY_FIELD: &str = "prompt_cache_key";
pub(crate) const CODEX_AUTO_REVIEW_MODEL: &str = "codex-auto-review";

const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_UPSTREAM_ERROR_BYTES: usize = crate::route_request_log::MAX_LOG_ERROR_BYTES;
const MAX_UPSTREAM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_UPSTREAM_SSE_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const RETAINED_RESPONSE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const MAX_CACHED_RESPONSE_IDS: usize = 1024;
const DOWNSTREAM_WEBSOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UPSTREAM_SSE_SNIFF_BYTES: usize = 1024;
const REQUEST_JSON_OFFLOAD_BYTES: usize = 256 * 1024;
const MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_CUSTOM_TOOL_SOURCE_DESCRIPTION_BYTES: usize = 2 * 1024;
const MAX_CONCURRENT_CONNECTIONS: usize = 64;
const MAX_CONCURRENT_REJECTIONS: usize = 4;
const REQUEST_BODY_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const REQUEST_BODY_BUDGET_UNIT_BYTES: usize = 64 * 1024;
// serde_json trees and protocol conversion buffers live alongside the encoded
// request. Reserve a conservative multiple of the wire size so the semaphore
// represents the request's working set instead of only its first Vec<u8>.
const REQUEST_MEMORY_BUDGET_MULTIPLIER: usize = 4;
const REQUEST_BODY_BUDGET_PERMITS: usize =
    REQUEST_BODY_BUDGET_BYTES / REQUEST_BODY_BUDGET_UNIT_BYTES;
const MAX_ROUTE_BINDINGS: usize = 4096;
const MAX_UPSTREAM_WEBSOCKET_BACKOFFS: usize = 128;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UPSTREAM_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
// A non-streaming upstream may not send response headers until generation is
// complete, so its header wait is also the model's total generation budget.
const UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UPSTREAM_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const UPSTREAM_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DOWNSTREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_HTTP_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UPSTREAM_HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const UPSTREAM_HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
const UPSTREAM_TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(15);
const UPSTREAM_TCP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const UPSTREAM_TCP_KEEPALIVE_RETRIES: u32 = 3;
const UPSTREAM_WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const UPSTREAM_WEBSOCKET_UNSUPPORTED_TTL: Duration = Duration::from_secs(60 * 60);
const UPSTREAM_WEBSOCKET_BACKOFF_STEPS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
const UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
const UPSTREAM_WEBSOCKET_PONG_TIMEOUT: Duration = Duration::from_secs(10);
const UPSTREAM_WEBSOCKET_MAX_REUSE_AGE: Duration = Duration::from_secs(55 * 60);
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const RESPONSES_WEBSOCKET_PATHS: [&str; 2] = ["/v1/responses", "/responses"];
const REQUEST_LOG_PAGE_PATH: &str = "/codey/request-logs";
const REQUEST_LOG_SCRIPT_PATH: &str = "/codey/request-logs.js";
const REQUEST_LOG_PAGE: &str = r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Codey 请求日志</title></head>
<body><div id="root"></div><script src="/codey/request-logs.js"></script></body>
</html>"#;
#[cfg(not(test))]
const ROUTER_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const ROUTER_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

tokio::task_local! {
    static ROUTER_REQUEST_ID: String;
    static ROUTER_REQUEST_STARTED_AT: Instant;
}

// The router used to be one 18.7K-line file. Each submodule owns one concern
// and exposes `pub(crate)` items; the glob re-exports keep every existing
// `local_router::…` path (and the `use super::*` inside submodules) working.
mod adapt;
mod anthropic_request;
mod auth;
mod chat_request;
mod chat_tools;
mod compaction;
mod downstream;
mod errors;
mod http;
mod native_history;
mod request_log_tap;
mod request_meta;
mod resource_budget;
mod responses;
mod server;
mod sse;
mod sse_anthropic;
mod sse_chat;
mod sse_responses;
mod upstream;
mod upstream_response;
mod websocket;
mod websocket_context;

pub(crate) use adapt::*;
pub(crate) use anthropic_request::*;
pub(crate) use auth::*;
pub(crate) use chat_request::*;
pub(crate) use chat_tools::*;
pub(crate) use compaction::*;
pub(crate) use downstream::*;
pub(crate) use errors::*;
pub(crate) use http::*;
pub(crate) use native_history::*;
pub(crate) use request_log_tap::*;
pub(crate) use request_meta::*;
pub(crate) use resource_budget::*;
pub(crate) use server::*;
pub(crate) use sse::*;
pub(crate) use sse_anthropic::*;
pub(crate) use sse_chat::*;
pub(crate) use sse_responses::*;
pub(crate) use upstream::*;
pub(crate) use upstream_response::*;
pub(crate) use websocket::*;
pub(crate) use websocket_context::*;

#[cfg(test)]
#[path = "../local_router_bench.rs"]
mod latency_bench;

#[cfg(test)]
#[path = "../local_router_stability_tests.rs"]
mod stability_tests;

#[cfg(test)]
#[path = "../local_router_tail_tests.rs"]
mod tail_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod safety_tests;
