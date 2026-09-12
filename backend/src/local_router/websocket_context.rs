use super::*;

#[derive(Debug)]
pub(crate) struct WebSocketRequestContext {
    pub(crate) headers: Vec<(String, String)>,
}

pub(crate) async fn request_looks_like_responses_websocket(stream: &TcpStream) -> Result<bool> {
    tokio::time::timeout(REQUEST_READ_TIMEOUT, async {
        let mut peek = vec![0_u8; 4096];
        loop {
            let read = stream
                .peek(&mut peek)
                .await
                .context("探测 Codey Responses WebSocket 请求失败")?;
            if read == 0 {
                return Ok(false);
            }
            let bytes = &peek[..read];
            let Some(request_line_end) = bytes.windows(2).position(|window| window == b"\r\n")
            else {
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            };
            let request_line = std::str::from_utf8(&bytes[..request_line_end])
                .context("WebSocket HTTP 请求行不是 UTF-8")?;
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or_default();
            let raw_path = parts.next().unwrap_or_default();
            let path = raw_path.split('?').next().unwrap_or(raw_path);
            if method != "GET" || !RESPONSES_WEBSOCKET_PATHS.contains(&path) {
                return Ok(false);
            }
            if let Some(header_end) = find_header_end(bytes) {
                let headers = String::from_utf8_lossy(&bytes[request_line_end + 2..header_end]);
                let mut connection_upgrade = false;
                let mut websocket_upgrade = false;
                for line in headers.split("\r\n") {
                    let Some((name, value)) = line.split_once(':') else {
                        continue;
                    };
                    if name.trim().eq_ignore_ascii_case("connection") {
                        connection_upgrade = value
                            .split([',', ' '])
                            .any(|part| part.trim().eq_ignore_ascii_case("upgrade"));
                    } else if name.trim().eq_ignore_ascii_case("upgrade") {
                        websocket_upgrade = value.trim().eq_ignore_ascii_case("websocket");
                    }
                }
                return Ok(connection_upgrade && websocket_upgrade);
            }
            if read == peek.len() {
                if peek.len() >= MAX_HEADER_BYTES.saturating_add(4) {
                    return Ok(false);
                }
                peek.resize(
                    peek.len()
                        .saturating_mul(2)
                        .min(MAX_HEADER_BYTES.saturating_add(4)),
                    0,
                );
            } else {
                // `peek` leaves the current bytes readable, so wait briefly
                // for another packet instead of spinning on the same prefix.
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    })
    .await
    .context("探测 Codey Responses WebSocket 请求超时")?
}

pub(crate) fn websocket_request_authorized(
    request: &WebSocketRequest,
    token: &str,
    bearer_token: &str,
) -> bool {
    request.headers().iter().any(|(name, value)| {
        let Ok(value) = value.to_str() else {
            return false;
        };
        (name.as_str().eq_ignore_ascii_case(ROUTER_AUTH_HEADER)
            && constant_time_eq(value.trim().as_bytes(), token.as_bytes()))
            || (name.as_str().eq_ignore_ascii_case("authorization")
                && constant_time_eq(value.trim().as_bytes(), bearer_token.as_bytes()))
    })
}

pub(crate) fn websocket_forward_headers(request: &WebSocketRequest) -> Vec<(String, String)> {
    let connection_scoped = connection_scoped_header_names(
        request
            .headers()
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value))),
    );
    request
        .headers()
        .iter()
        .filter(|(name, _)| {
            let name = name.as_str();
            !is_hop_by_hop_header(name)
                && !name.to_ascii_lowercase().starts_with("sec-websocket-")
                && !connection_scoped.contains(&name.to_ascii_lowercase())
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

pub(crate) fn websocket_handshake_error(
    status: WebSocketStatusCode,
    message: &str,
) -> WebSocketErrorResponse {
    WebSocketResponse::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", message.len())
        .body(Some(message.to_string()))
        .expect("static WebSocket handshake error response must be valid")
}

pub(crate) fn responses_websocket_stream_id(body: &Value) -> Result<Option<String>> {
    let Some(stream_id) = body.get("stream_id") else {
        return Ok(None);
    };
    let stream_id = stream_id.as_str().context("stream_id 必须是字符串")?;
    if stream_id.is_empty()
        || stream_id.len() > 256
        || !stream_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        anyhow::bail!("必须为 1–256 个字母、数字、下划线、连字符或句点");
    }
    Ok(Some(stream_id.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamWebSocketAttempt {
    UseHttp,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamWebSocketMaintenanceAction {
    None,
    SendPing,
    Drop,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UpstreamWebSocketLiveness {
    pub(crate) connected_at: Instant,
    pub(crate) last_activity_at: Instant,
    pub(crate) heartbeat_sent_at: Option<Instant>,
}

impl UpstreamWebSocketLiveness {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            connected_at: now,
            last_activity_at: now,
            heartbeat_sent_at: None,
        }
    }

    pub(crate) fn record_activity(&mut self, now: Instant) {
        self.last_activity_at = now;
    }

    pub(crate) fn record_pong(&mut self, now: Instant) {
        self.last_activity_at = now;
        self.heartbeat_sent_at = None;
    }

    pub(crate) fn record_heartbeat_sent(&mut self, now: Instant) {
        self.heartbeat_sent_at = Some(now);
    }

    pub(crate) fn maintenance_deadline(&self) -> Instant {
        let liveness_deadline = self
            .heartbeat_sent_at
            .map(|sent_at| sent_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT)
            .unwrap_or(self.last_activity_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL);
        std::cmp::min(
            self.connected_at + UPSTREAM_WEBSOCKET_MAX_REUSE_AGE,
            liveness_deadline,
        )
    }

    pub(crate) fn maintenance_action(&self, now: Instant) -> UpstreamWebSocketMaintenanceAction {
        if now >= self.connected_at + UPSTREAM_WEBSOCKET_MAX_REUSE_AGE {
            return UpstreamWebSocketMaintenanceAction::Drop;
        }
        if let Some(sent_at) = self.heartbeat_sent_at {
            return if now >= sent_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT {
                UpstreamWebSocketMaintenanceAction::Drop
            } else {
                UpstreamWebSocketMaintenanceAction::None
            };
        }
        if now >= self.last_activity_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL {
            UpstreamWebSocketMaintenanceAction::SendPing
        } else {
            UpstreamWebSocketMaintenanceAction::None
        }
    }
}

pub(crate) struct CachedUpstreamWebSocket {
    pub(crate) route_id: String,
    pub(crate) url: String,
    pub(crate) auth_identity: UpstreamWebSocketAuthIdentity,
    pub(crate) response_ids: VecDeque<[u8; 32]>,
    pub(crate) config_identity: [u8; 32],
    pub(crate) liveness: UpstreamWebSocketLiveness,
    pub(crate) socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct UpstreamWebSocketAuthIdentity {
    pub(crate) authorization: Option<[u8; 32]>,
    pub(crate) account_id: Option<[u8; 32]>,
}

impl UpstreamWebSocketAuthIdentity {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            authorization: websocket_header_fingerprint(headers, AUTHORIZATION.as_str()),
            account_id: websocket_header_fingerprint(headers, CHATGPT_ACCOUNT_ID_HEADER),
        }
    }
}

pub(crate) fn websocket_header_fingerprint(headers: &HeaderMap, name: &str) -> Option<[u8; 32]> {
    headers
        .get(name)
        .map(|value| Sha256::digest(value.as_bytes()).into())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UpstreamWebSocketBackoffKey {
    pub(crate) route_id: String,
    pub(crate) url: String,
    pub(crate) auth_identity: UpstreamWebSocketAuthIdentity,
    pub(crate) config_identity: [u8; 32],
}

impl UpstreamWebSocketBackoffKey {
    pub(crate) fn new(
        route_id: &str,
        url: &str,
        auth_identity: UpstreamWebSocketAuthIdentity,
    ) -> Self {
        Self {
            route_id: route_id.to_string(),
            url: url.to_string(),
            auth_identity,
            config_identity: [0; 32],
        }
    }

    pub(crate) fn for_route(
        route: &RouteTarget,
        url: &str,
        auth: UpstreamWebSocketAuthIdentity,
    ) -> Self {
        let mut key = Self::new(&route.provider_id, url, auth);
        key.config_identity = route.websocket_config;
        key
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UpstreamWebSocketBackoff {
    pub(crate) failure_count: u32,
    pub(crate) until: Instant,
    pub(crate) unsupported: bool,
    pub(crate) generation: u64,
}

#[derive(Debug)]
pub(crate) struct UpstreamWebSocketBackoffs {
    pub(crate) entries: HashMap<UpstreamWebSocketBackoffKey, UpstreamWebSocketBackoff>,
    pub(crate) order: VecDeque<(UpstreamWebSocketBackoffKey, u64)>,
    pub(crate) next_generation: u64,
    routes: Option<HashMap<String, [u8; 32]>>,
    pub(crate) changes: tokio::sync::watch::Sender<u64>,
    probing: HashMap<UpstreamWebSocketBackoffKey, u64>,
    supported: HashSet<UpstreamWebSocketBackoffKey>,
}

impl Default for UpstreamWebSocketBackoffs {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            next_generation: 0,
            routes: None,
            changes: tokio::sync::watch::channel(0).0,
            probing: HashMap::new(),
            supported: HashSet::new(),
        }
    }
}

pub(crate) struct UpstreamWebSocketProbe {
    shared: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    key: UpstreamWebSocketBackoffKey,
    generation: Option<u64>,
}

impl UpstreamWebSocketProbe {
    pub(crate) fn acquire(
        shared: &Arc<Mutex<UpstreamWebSocketBackoffs>>,
        key: &UpstreamWebSocketBackoffKey,
    ) -> Option<Self> {
        let mut state = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.route_is_current(&key.route_id, &key.config_identity)
            || state.is_backing_off(key, Instant::now())
        {
            return None;
        }
        let generation = if state.supported.contains(key) {
            None
        } else {
            if state.probing.contains_key(key) {
                return None;
            }
            state.next_generation = state.next_generation.wrapping_add(1);
            let generation = state.next_generation;
            state.probing.insert(key.clone(), generation);
            Some(generation)
        };
        Some(Self {
            shared: Arc::clone(shared),
            key: key.clone(),
            generation,
        })
    }
}

impl Drop for UpstreamWebSocketProbe {
    fn drop(&mut self) {
        let Some(generation) = self.generation else {
            return;
        };
        let mut state = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.probing.get(&self.key) == Some(&generation) {
            state.probing.remove(&self.key);
        }
    }
}

impl UpstreamWebSocketBackoffs {
    pub(crate) fn route_is_current(&self, route_id: &str, identity: &[u8; 32]) -> bool {
        self.routes
            .as_ref()
            .is_none_or(|routes| routes.get(route_id) == Some(identity))
    }

    pub(crate) fn update_routes(&mut self, snapshot: &RouterSnapshot) {
        let routes = snapshot
            .routes
            .iter()
            .filter(|(_, route)| route.supports_websockets)
            .map(|(id, route)| (id.clone(), route.websocket_config))
            .collect::<HashMap<_, _>>();
        if self.routes.as_ref() == Some(&routes) {
            return;
        }
        let current = |key: &UpstreamWebSocketBackoffKey| {
            routes.get(&key.route_id) == Some(&key.config_identity)
        };
        self.entries.retain(|key, _| current(key));
        self.order.retain(|(key, _)| current(key));
        self.probing.retain(|key, _| current(key));
        self.supported.retain(current);
        self.routes = Some(routes);
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    pub(crate) fn is_backing_off(&self, key: &UpstreamWebSocketBackoffKey, now: Instant) -> bool {
        self.entries
            .get(key)
            .is_some_and(|backoff| backoff.until > now)
    }

    pub(crate) fn record_failure(
        &mut self,
        key: UpstreamWebSocketBackoffKey,
        now: Instant,
    ) -> (u32, Duration) {
        self.supported.remove(&key);
        if !self.route_is_current(&key.route_id, &key.config_identity) {
            return (1, upstream_websocket_backoff_duration(1));
        }
        // Concurrent failures belong to the same outage; only a failed retry
        // after the deadline advances backoff. Preserve unsupported endpoints too.
        if let Some(backoff) = self.entries.get(&key)
            && backoff.until > now
        {
            return (backoff.failure_count, backoff.until.duration_since(now));
        }
        let reset_after = *UPSTREAM_WEBSOCKET_BACKOFF_STEPS
            .last()
            .expect("WebSocket backoff steps must not be empty");
        let failure_count = self
            .entries
            .get(&key)
            .filter(|backoff| !backoff.unsupported && now <= backoff.until + reset_after)
            .map(|backoff| backoff.failure_count.saturating_add(1))
            .unwrap_or(1);
        let duration = upstream_websocket_backoff_duration(failure_count);
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.entries.insert(
            key.clone(),
            UpstreamWebSocketBackoff {
                failure_count,
                until: now + duration,
                unsupported: false,
                generation,
            },
        );
        self.order.push_back((key, generation));
        self.enforce_limit();
        (failure_count, duration)
    }

    pub(crate) fn record_unsupported(&mut self, key: UpstreamWebSocketBackoffKey, now: Instant) {
        self.supported.remove(&key);
        if !self.route_is_current(&key.route_id, &key.config_identity) {
            return;
        }
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.entries.insert(
            key.clone(),
            UpstreamWebSocketBackoff {
                failure_count: 0,
                until: now + UPSTREAM_WEBSOCKET_UNSUPPORTED_TTL,
                unsupported: true,
                generation,
            },
        );
        self.order.push_back((key, generation));
        self.enforce_limit();
    }

    pub(crate) fn record_success(&mut self, key: &UpstreamWebSocketBackoffKey) {
        self.entries.remove(key);
        if self.route_is_current(&key.route_id, &key.config_identity) {
            if self.supported.len() >= MAX_UPSTREAM_WEBSOCKET_BACKOFFS {
                self.supported.clear();
            }
            self.supported.insert(key.clone());
        }
    }

    pub(crate) fn enforce_limit(&mut self) {
        while self.entries.len() > MAX_UPSTREAM_WEBSOCKET_BACKOFFS {
            let Some((key, generation)) = self.order.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.generation == generation)
            {
                self.entries.remove(&key);
            }
        }
        if self.order.len() > MAX_UPSTREAM_WEBSOCKET_BACKOFFS * 4 {
            self.order.retain(|(key, generation)| {
                self.entries
                    .get(key)
                    .is_some_and(|entry| entry.generation == *generation)
            });
        }
    }
}

pub(crate) fn upstream_websocket_backoff_duration(failure_count: u32) -> Duration {
    let index = failure_count.saturating_sub(1) as usize;
    UPSTREAM_WEBSOCKET_BACKOFF_STEPS[index.min(UPSTREAM_WEBSOCKET_BACKOFF_STEPS.len() - 1)]
}

pub(crate) fn record_upstream_websocket_failure(
    backoffs: &Arc<Mutex<UpstreamWebSocketBackoffs>>,
    key: &UpstreamWebSocketBackoffKey,
) {
    backoffs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .record_failure(key.clone(), Instant::now());
}
