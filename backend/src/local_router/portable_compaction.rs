use super::*;

const MAX_PORTABLE_TICKETS: usize = 128;
const PREPARE_TTL: Duration = Duration::from_secs(120);
const RETAIN_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub(crate) struct PortableSourceRetry {
    identity: [u8; 32],
    expires_at: Instant,
}

fn retry_identity(
    source: &RouteTarget,
    source_model: &str,
    target: &RouteTarget,
    target_model: &str,
    generation: u64,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    for text in [
        &source.provider_id,
        source_model,
        &target.provider_id,
        target_model,
    ] {
        digest.update((text.len() as u64).to_be_bytes());
        digest.update(text.as_bytes());
    }
    digest.update(source.context_config);
    digest.update(target.context_config);
    digest.update(generation.to_be_bytes());
    digest.finalize().into()
}

#[derive(Clone, Debug)]
pub(crate) struct PortableTicket {
    token: String,
    source: Arc<RouteTarget>,
    model: String,
    target: Arc<RouteTarget>,
    target_model: String,
    generation: u64,
    created: Instant,
    state: &'static str,
    retry_source: bool,
}

impl PortableTicket {
    fn expire(&mut self) {
        if self.state == "prepared" && self.created.elapsed() >= PREPARE_TTL {
            self.state = "expired";
        }
    }
}

// A concrete thread header always wins. A child must never consume a ticket
// registered for its parent through a shared session header.
pub(crate) fn portable_request_key(request: &HttpRequest) -> Option<String> {
    if let Some(thread) = incoming_header(request, "thread-id") {
        return valid_codex_session_id(thread).map(|id| format!("thread-id:{id}"));
    }
    if request_is_subagent(request)
        || incoming_header(request, "x-codex-parent-thread-id").is_some()
    {
        return None;
    }
    incoming_header(request, "session-id")
        .and_then(valid_codex_session_id)
        .map(|id| format!("thread-id:{id}"))
}

impl LocalRouter {
    pub(crate) fn portable_compaction(&self, operation: &str, payload: &Value) -> Result<Value> {
        self.portable_compaction_from_home(operation, payload, crate::codex_config::codex_home())
    }

    fn portable_compaction_from_home(
        &self,
        operation: &str,
        payload: &Value,
        home: &std::path::Path,
    ) -> Result<Value> {
        let thread = payload
            .get("threadId")
            .and_then(Value::as_str)
            .and_then(valid_codex_session_id)
            .context("缺少有效的 threadId")?;
        let key = format!("thread-id:{thread}");
        let restore_source = if operation == "prepare"
            && !self
                .bindings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .routes
                .contains_key(&key)
        {
            crate::session_model::native_compaction_source(home, thread)?
        } else {
            None
        };
        let snapshot = self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut bindings = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ticket in bindings.portable_tickets.values_mut() {
            ticket.expire();
        }
        if operation == "prepare" {
            bindings
                .portable_source_retries
                .retain(|_, retry| retry.expires_at > Instant::now());
            bindings.portable_tickets.retain(|_, ticket| {
                ticket.state == "running" || ticket.created.elapsed() < RETAIN_TTL
            });
            anyhow::ensure!(
                !bindings
                    .portable_tickets
                    .get(&key)
                    .is_some_and(|ticket| matches!(ticket.state, "prepared" | "running")),
                "此会话已有可移植摘要正在准备或执行"
            );
            anyhow::ensure!(
                !bindings.compacting.contains(&key)
                    && !bindings
                        .compacting
                        .contains(&format!("session-id:{thread}")),
                "此会话正在压缩，请完成后重试"
            );
            let target = payload
                .get("targetProviderId")
                .and_then(Value::as_str)
                .context("缺少 targetProviderId")?;
            let target_model = payload
                .get("targetModel")
                .and_then(Value::as_str)
                .context("缺少 targetModel")?;
            let target_route = snapshot.target_for_request(target_model, Some(target), None)?;
            // 文件读取期间可能已有请求确认了活动线路，不能覆盖该绑定。
            if !bindings.routes.contains_key(&key)
                && let Some(source) = restore_source.as_deref()
            {
                anyhow::ensure!(
                    snapshot.aliases.contains_key(&model_id::key(source)),
                    "压缩历史的源线路或模型已不可用，请恢复原线路后重试"
                );
                let resolved = snapshot.target_for_request(source, None, None)?;
                anyhow::ensure!(
                    resolved.fallback_reason.is_none(),
                    "压缩历史的源线路已不可用"
                );
                anyhow::ensure!(
                    ProtocolBridge::from_upstream_protocol(resolved.protocol)
                        == ProtocolBridge::NativeResponses,
                    "压缩历史的源线路协议已变化"
                );
                bindings.remember_resolved(
                    std::slice::from_ref(&key),
                    &resolved,
                    false,
                    true,
                    Some(false),
                );
            }
            let Some(binding) = bindings.routes.get(&key).cloned() else {
                return Ok(json!({"status":"ok","required":false}));
            };
            if binding.portable
                || (binding.provider_id == target_route.provider_id
                    && binding.context_config == Some(target_route.route.context_config))
            {
                return Ok(json!({"status":"ok","required":false}));
            }
            let source = snapshot
                .routes
                .get(&binding.provider_id)
                .context("源线路已不可用，无法生成交接摘要")?;
            if ProtocolBridge::from_upstream_protocol(source.protocol)
                != ProtocolBridge::NativeResponses
            {
                return Ok(json!({"status":"ok","required":false}));
            }
            anyhow::ensure!(
                !binding.upstream_model.is_empty()
                    && binding.context_config == Some(source.context_config),
                "源线路身份已变化，请恢复原线路后重试"
            );
            anyhow::ensure!(
                bindings.portable_tickets.contains_key(&key)
                    || bindings.portable_tickets.len() < MAX_PORTABLE_TICKETS,
                "可移植摘要任务已满，请稍后重试"
            );
            let token = Uuid::new_v4().to_string();
            let identity = retry_identity(
                source,
                &binding.upstream_model,
                &target_route.route,
                &target_route.upstream_model,
                binding.generation,
            );
            let retry_source = bindings
                .portable_source_retries
                .get(&key)
                .is_some_and(|retry| retry.identity == identity);
            if !retry_source {
                bindings.portable_source_retries.remove(&key);
            }
            bindings.portable_tickets.insert(
                key,
                PortableTicket {
                    token: token.clone(),
                    source: Arc::clone(source),
                    model: binding.upstream_model.clone(),
                    target: target_route.route,
                    target_model: target_route.upstream_model,
                    generation: binding.generation,
                    created: Instant::now(),
                    state: "prepared",
                    retry_source,
                },
            );
            return Ok(json!({"status":"ok","required":true,"token":token,
                "handoffRoute": if retry_source { "source" } else { "automatic" },
                "sourceProviderId":binding.provider_id,"sourceModel":binding.upstream_model}));
        }
        let token = payload
            .get("token")
            .and_then(Value::as_str)
            .context("缺少 token")?;
        let ticket = bindings
            .portable_tickets
            .get(&key)
            .filter(|ticket| ticket.token == token)
            .context("可移植摘要任务不存在或令牌不匹配")?;
        match operation {
            "status" => Ok(json!({"status":"ok","state":ticket.state})),
            "confirm" => {
                anyhow::ensure!(ticket.state == "completed", "可移植摘要尚未完成");
                let generation = ticket.generation;
                let source = Arc::clone(&ticket.source);
                let model = ticket.model.clone();
                anyhow::ensure!(
                    snapshot
                        .routes
                        .get(&source.provider_id)
                        .is_some_and(|route| {
                            route.context_config == source.context_config
                                && route.models.contains(&model)
                        }),
                    "源线路或模型已变化"
                );
                anyhow::ensure!(
                    snapshot
                        .routes
                        .get(&ticket.target.provider_id)
                        .is_some_and(|route| {
                            route.context_config == ticket.target.context_config
                                && route.models.contains(&ticket.target_model)
                        }),
                    "目标线路或模型已变化，请重新生成摘要"
                );
                let binding = bindings.routes.get_mut(&key).context("源会话绑定已失效")?;
                anyhow::ensure!(
                    binding.generation == generation
                        && binding.provider_id == source.provider_id
                        && binding.upstream_model == model
                        && binding.context_config == Some(source.context_config),
                    "源会话已变化，请重新生成摘要"
                );
                binding.portable = true;
                Ok(json!({"status":"ok"}))
            }
            "cancel" => {
                anyhow::ensure!(
                    matches!(ticket.state, "prepared" | "failed" | "expired"),
                    "只能取消尚未开始、失败或过期的可移植摘要"
                );
                // 前端失败处理会清理 failed ticket；仅尚未开始的主动取消撤销回源提示。
                if ticket.state != "failed" {
                    bindings.portable_source_retries.remove(&key);
                }
                bindings.portable_tickets.remove(&key);
                Ok(json!({"status":"ok"}))
            }
            _ => anyhow::bail!("未知的可移植摘要操作"),
        }
    }
}

pub(crate) struct PortableAttempt {
    bindings: Arc<Mutex<RouteBindings>>,
    key: String,
    token: String,
    generation: u64,
    pub(crate) source: Arc<RouteTarget>,
    pub(crate) model: String,
    target: Arc<RouteTarget>,
    target_model: String,
    pub(crate) uses_target: bool,
    retry_source: bool,
    completed: bool,
}

impl PortableAttempt {
    pub(crate) fn acquire(
        server: &RouterServer,
        request: &HttpRequest,
        compacting: bool,
        snapshot: &RouterSnapshot,
    ) -> Result<Option<Self>> {
        let Some(key) = portable_request_key(request) else {
            return Ok(None);
        };
        let mut bindings = server
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let binding = bindings.routes.get(&key).cloned();
        let source_retry = bindings.portable_source_retries.get(&key).cloned();
        let Some(ticket) = bindings.portable_tickets.get_mut(&key) else {
            return Ok(None);
        };
        ticket.expire();
        if !compacting {
            anyhow::ensure!(
                !matches!(ticket.state, "prepared" | "running"),
                "此会话正在准备可移植摘要，请完成后再发送请求"
            );
            return Ok(None);
        }
        if ticket.state == "completed" {
            return Ok(None);
        }
        anyhow::ensure!(
            ticket.state == "prepared",
            "可移植摘要任务已失效或正在运行，请重新准备"
        );
        let source_matches = binding.is_some_and(|binding| {
            binding.generation == ticket.generation
                && binding.provider_id == ticket.source.provider_id
                && binding.upstream_model == ticket.model
                && binding.context_config == Some(ticket.source.context_config)
        });
        if !source_matches
            || !snapshot
                .routes
                .get(&ticket.source.provider_id)
                .is_some_and(|route| {
                    route.context_config == ticket.source.context_config
                        && route.models.contains(&ticket.model)
                })
        {
            ticket.state = "failed";
            anyhow::bail!("源线路或模型已变化，请重新准备可移植摘要");
        }
        if !snapshot
            .routes
            .get(&ticket.target.provider_id)
            .is_some_and(|route| {
                route.context_config == ticket.target.context_config
                    && route.models.contains(&ticket.target_model)
            })
        {
            ticket.state = "failed";
            anyhow::bail!("目标线路或模型已变化，请重新准备可移植摘要");
        }
        ticket.state = "running";
        let retry_source = std::mem::take(&mut ticket.retry_source)
            && source_retry.is_some_and(|retry| {
                retry.expires_at > Instant::now()
                    && retry.identity
                        == retry_identity(
                            &ticket.source,
                            &ticket.model,
                            &ticket.target,
                            &ticket.target_model,
                            ticket.generation,
                        )
            });
        let attempt = Self {
            bindings: Arc::clone(&server.bindings),
            key,
            token: ticket.token.clone(),
            generation: ticket.generation,
            source: Arc::clone(&ticket.source),
            model: ticket.model.clone(),
            target: Arc::clone(&ticket.target),
            target_model: ticket.target_model.clone(),
            uses_target: false,
            retry_source,
            completed: false,
        };
        if retry_source {
            bindings.portable_source_retries.remove(&attempt.key);
        }
        Ok(Some(attempt))
    }

    pub(crate) fn prepare_target(
        &mut self,
        body: &mut Value,
        cache: &PortableHistorySnapshot,
        metadata: Option<&serde_json::Map<String, Value>>,
        permit: &mut Option<OwnedSemaphorePermit>,
        body_budget: &Arc<Semaphore>,
    ) -> bool {
        if self.retry_source {
            return false;
        }
        let Some(metadata) = metadata.filter(|metadata| {
            matches!(
                metadata.get("codey_context_source").and_then(Value::as_str),
                Some("official_catalog" | "user_declared" | "plugin_declared")
            )
        }) else {
            return false;
        };
        let Some(window) = metadata
            .get("context_window")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
        else {
            return false;
        };
        let window = window.saturating_mul(
            metadata
                .get("effective_context_window_percent")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(100),
        ) / 100;
        let window = metadata
            .get("max_context_window")
            .and_then(Value::as_u64)
            .filter(|limit| *limit > 0)
            .map_or(window, |limit| window.min(limit));
        let Ok(output_budget) = gateway_compaction_output_budget(body, Some(metadata)) else {
            return false;
        };
        let Some(restoration) = cache.restoration(&self.key, &self.source, &self.model, body)
        else {
            return false;
        };
        if restoration
            .input_token_bound()
            .and_then(|input| input.checked_add(output_budget))
            .is_none_or(|total| total > window)
        {
            return false;
        }
        let Some(required_bytes) = restoration.request_budget_bytes() else {
            return false;
        };
        // 新增许可先独立持有；恢复或校验失败时自动归还，原请求预算保持不变。
        let Ok(additional_permit) =
            reserve_request_body_budget_growth(permit.as_ref(), body_budget, required_bytes)
        else {
            return false;
        };
        let Some(restored) = restoration.materialize() else {
            return false;
        };
        if validate_cross_route_context(&restored).is_err() {
            return false;
        }
        if let Some(additional_permit) = additional_permit {
            if let Some(held) = permit.as_mut() {
                held.merge(additional_permit);
            } else {
                *permit = Some(additional_permit);
            }
        }
        *body = restored;
        self.uses_target = true;
        true
    }

    pub(crate) fn target_metadata(
        &self,
        snapshot: &RouterSnapshot,
    ) -> Option<serde_json::Map<String, Value>> {
        let metadata =
            crate::model_catalog::runtime_context_metadata(crate::codex_config::codex_home());
        let mut value = gateway_compaction_model_metadata(
            &metadata,
            &self.target.provider_id,
            &self.target_model,
        )
        .cloned()
        .map(Value::Object)
        .unwrap_or_else(|| json!({}));
        let alias = model_alias(&self.target.provider_id, &self.target_model);
        if let Some((_, policy)) = snapshot
            .declared_model_contexts
            .iter()
            .find(|(model, _)| model_id::equal(model, &alias))
        {
            crate::model_catalog::apply_model_context(&mut value, Some(policy)).ok()?;
        }
        value.as_object().cloned()
    }

    // 上游拒绝容量估算时，下一次由用户发起的同身份交接只尝试源线路一次。
    pub(crate) fn target_context_rejected(&mut self) {
        if !self.uses_target {
            return;
        }
        let mut bindings = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !bindings.routes.get(&self.key).is_some_and(|binding| {
            binding.generation == self.generation
                && binding.provider_id == self.source.provider_id
                && binding.upstream_model == self.model
                && binding.context_config == Some(self.source.context_config)
        }) {
            return;
        }
        let identity = retry_identity(
            &self.source,
            &self.model,
            &self.target,
            &self.target_model,
            self.generation,
        );
        if let Some(ticket) = bindings.portable_tickets.get_mut(&self.key)
            && ticket.token == self.token
            && ticket.state == "running"
            && retry_identity(
                &ticket.source,
                &ticket.model,
                &ticket.target,
                &ticket.target_model,
                ticket.generation,
            ) == identity
        {
            // 错误响应尚未写完时，状态查询和失败清理也必须可用。
            // 外层 CompactionGuard 继续阻止并发压缩，直到当前响应处理结束。
            ticket.state = "failed";
            bindings
                .portable_source_retries
                .retain(|_, retry| retry.expires_at > Instant::now());
            if bindings.portable_source_retries.len() >= MAX_PORTABLE_TICKETS
                && !bindings.portable_source_retries.contains_key(&self.key)
                && let Some(oldest) = bindings
                    .portable_source_retries
                    .iter()
                    .min_by_key(|(_, retry)| retry.expires_at)
                    .map(|(key, _)| key.clone())
            {
                bindings.portable_source_retries.remove(&oldest);
            }
            bindings.portable_source_retries.insert(
                self.key.clone(),
                PortableSourceRetry {
                    identity,
                    expires_at: Instant::now() + RETAIN_TTL,
                },
            );
        }
    }

    pub(crate) fn route(&self) -> &Arc<RouteTarget> {
        if self.uses_target {
            &self.target
        } else {
            &self.source
        }
    }

    pub(crate) fn upstream_model(&self) -> &str {
        if self.uses_target {
            &self.target_model
        } else {
            &self.model
        }
    }

    pub(crate) fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for PortableAttempt {
    fn drop(&mut self) {
        let mut bindings = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ticket) = bindings.portable_tickets.get_mut(&self.key)
            && ticket.token == self.token
            && ticket.state == "running"
        {
            ticket.state = if self.completed {
                "completed"
            } else {
                "failed"
            };
        }
    }
}

// None denotes an unresolved delta; it cannot replace the stored history state.
pub(crate) fn resolved_history_portability(body: &Value) -> Option<bool> {
    if validate_cross_route_history_items(body).is_err() {
        Some(false)
    } else if responses_previous_response_id(body).is_none() {
        Some(true)
    } else {
        None
    }
}

/// 原生压缩完整交付前不修改共享绑定，失败或取消仍沿用原历史的线路身份。
/// 提交时只更新准备后未变化的 key，保留并发请求写入的新绑定。
pub(crate) struct PendingNativeCompactionBinding {
    bindings: Arc<Mutex<RouteBindings>>,
    expected: Vec<(String, Option<u64>)>,
    prepared_generation: u64,
    resolved: RouteSelection,
    refresh_session: bool,
}

impl PendingNativeCompactionBinding {
    pub(crate) fn prepare(
        bindings: &Arc<Mutex<RouteBindings>>,
        keys: &[String],
        resolved: &RouteSelection,
        refresh_session: bool,
    ) -> Self {
        let stored = bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expected = keys
            .iter()
            .map(|key| {
                (
                    key.clone(),
                    stored.routes.get(key).map(|binding| binding.generation),
                )
            })
            .collect();
        Self {
            bindings: Arc::clone(bindings),
            expected,
            prepared_generation: stored.next_generation,
            resolved: resolved.clone(),
            refresh_session,
        }
    }

    pub(crate) fn complete(self) {
        let mut bindings = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let keys = self
            .expected
            .into_iter()
            .filter_map(|(key, generation)| {
                let current = bindings.routes.get(&key).map(|binding| binding.generation);
                let unchanged = match generation {
                    Some(generation) => current == Some(generation),
                    // 淘汰后不保留每个 key 的版本；全局版本变化时，无法排除新建后又被淘汰。
                    None => {
                        current.is_none() && bindings.next_generation == self.prepared_generation
                    }
                };
                unchanged.then_some(key)
            })
            .collect::<Vec<_>>();
        bindings.remember_resolved(&keys, &self.resolved, self.refresh_session, true, None);
    }
}

impl RouteBindings {
    pub(crate) fn remember_resolved(
        &mut self,
        keys: &[String],
        resolved: &RouteSelection,
        refresh_session: bool,
        native_compaction: bool,
        history_portable: Option<bool>,
    ) {
        let portable = keys
            .iter()
            .map(|key| {
                (
                    key.clone(),
                    self.routes.get(key).is_some_and(|binding| {
                        binding.portable
                            && binding.provider_id == resolved.provider_id
                            && binding.context_config == Some(resolved.route.context_config)
                            && binding.upstream_model == resolved.upstream_model
                    }),
                )
            })
            .collect::<HashMap<_, _>>();
        self.remember(keys, &resolved.provider_id, refresh_session);
        for key in keys {
            if let Some(binding) = self.routes.get_mut(key)
                && binding.provider_id == resolved.provider_id
                && binding.upstream_model.is_empty()
            {
                binding.upstream_model = resolved.upstream_model.clone();
                binding.context_config = Some(resolved.route.context_config);
                binding.portable = !native_compaction
                    && history_portable
                        .unwrap_or_else(|| portable.get(key).copied().unwrap_or(false));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cold_native_history(source: &str, target: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let records = [
            json!({"type":"session_meta","payload":{"id":"parent","model_provider":"codey_router"}}),
            json!({"type":"compacted","payload":{
                "replacement_history":[{"type":"compaction","encrypted_content":"source-ciphertext"}],
                "resume_metadata":{"previous_turn_settings":{"model":source}}
            }}),
            json!({"type":"turn_context","payload":{"model":target}}),
        ];
        std::fs::write(
            home.path().join("session.jsonl"),
            records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, rollout_path TEXT, model TEXT);")
            .unwrap();
        db.execute(
            "INSERT INTO threads VALUES ('parent','session.jsonl',?1)",
            [target],
        )
        .unwrap();
        home
    }

    #[tokio::test]
    async fn portable_compaction_cold_native_history_restores_source_before_target() {
        let (router, upstream, source, target, model) = fixture().await;
        let source_alias = model_alias(&source, &model);
        let target_alias = model_alias(&target, "target-model");
        let home = cold_native_history(&source_alias, &target_alias);
        let before = std::fs::read(home.path().join("session.jsonl")).unwrap();
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        let prepared = router
            .portable_compaction_from_home("prepare", &payload, home.path())
            .unwrap();
        assert_eq!(prepared["required"], true);
        assert_eq!(prepared["sourceProviderId"], source);
        assert_eq!(prepared["sourceModel"], model);
        let upstream_task = tokio::spawn(async move {
            for stage in 0..2 {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                if stage == 0 {
                    assert_eq!(body["model"], "provider-model");
                    assert_eq!(body["input"][0]["encrypted_content"], "source-ciphertext");
                } else {
                    assert_eq!(body["model"], "target-model");
                    assert_eq!(body["input"][0]["content"][0]["text"], "portable handoff");
                    assert!(!body.to_string().contains("source-ciphertext"));
                }
                write_json_response(&mut socket, 200, &json!({"id":"reply","status":"completed",
                    "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"portable handoff"}]}]})).await.unwrap();
            }
        });
        let client = client();
        let response = post(&client, &router, "parent", &target_alias,
            json!([{"type":"compaction","encrypted_content":"source-ciphertext"},{"type":"compaction_trigger"}]))
            .send().await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let result: Value = response.json().await.unwrap();
        router
            .portable_compaction(
                "confirm",
                &json!({"threadId":"parent","token":prepared["token"]}),
            )
            .unwrap();
        assert_eq!(
            post(
                &client,
                &router,
                "parent",
                &target_alias,
                result["output"].clone()
            )
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
            200
        );
        upstream_task.await.unwrap();
        assert_eq!(
            std::fs::read(home.path().join("session.jsonl")).unwrap(),
            before
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_cold_source_never_falls_back_or_overwrites_active_binding() {
        let (router, _upstream, source, target, model) = fixture().await;
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        for invalid_source in [
            "missing/provider-model".to_string(),
            model_alias(&source, "disabled-model"),
            model.clone(),
        ] {
            let home = cold_native_history(&invalid_source, &model_alias(&target, "target-model"));
            assert!(
                router
                    .portable_compaction_from_home("prepare", &payload, home.path())
                    .is_err()
            );
            assert!(
                !router
                    .bindings
                    .lock()
                    .unwrap()
                    .routes
                    .contains_key("thread-id:parent")
            );
        }
        let home = cold_native_history(
            &model_alias(&source, &model),
            &model_alias(&target, "target-model"),
        );
        bind_source(&router, &target, "target-model");
        assert_eq!(
            router
                .portable_compaction_from_home("prepare", &payload, home.path())
                .unwrap()["required"],
            false
        );
        assert_eq!(
            router.bindings.lock().unwrap().routes["thread-id:parent"].provider_id,
            target
        );
        router.stop().await.unwrap();
    }

    #[test]
    fn cross_route_compaction_trigger_is_an_operation_not_history() {
        let valid = json!({"input":[{"role":"user","content":"full history"},{"type":"compaction_trigger"}]});
        assert!(validate_cross_route_context(&valid).is_ok());
        assert!(validate_portable_context(&valid).is_err());
        for input in [
            json!([{"type":"compaction","encrypted_content":"opaque"},{"type":"compaction_trigger"}]),
            json!([{"type":"compaction_trigger"},{"role":"user","content":"next"}]),
            json!([{"type":"compaction_trigger","encrypted_content":"opaque"}]),
            json!([{"role":"user","content":[{"type":"compaction_trigger"}]}]),
            json!([{"type":"item_reference","id":"old"},{"type":"compaction_trigger"}]),
        ] {
            assert!(validate_cross_route_context(&json!({"input":input})).is_err());
        }
    }

    #[test]
    fn portable_compaction_unrestored_delta_cannot_establish_portability() {
        assert_eq!(
            resolved_history_portability(
                &json!({"input":"next","previous_response_id":"resp-old"})
            ),
            None
        );
        assert_eq!(
            resolved_history_portability(
                &json!({"input":[{"type":"compaction","encrypted_content":"opaque"}],"previous_response_id":"resp-old"})
            ),
            Some(false)
        );
        assert_eq!(
            resolved_history_portability(
                &json!({"input":[{"type":"item_reference","id":"item-old"}]})
            ),
            Some(false)
        );
        assert_eq!(
            resolved_history_portability(&json!({"input":"full history"})),
            Some(true)
        );
    }

    #[tokio::test]
    async fn portable_compaction_cross_route_full_history_can_compact_on_target() {
        let (router, upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let history = json!([{"role":"user","content":"complete visible history"},{"type":"compaction_trigger"}]);
        let expected = history.clone();
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["input"], expected);
            assert_eq!(body["model"], "target-model");
            write_json_response(
                &mut socket,
                200,
                &json!({"id":"native","status":"completed",
                "output":[{"type":"compaction","encrypted_content":"target-ciphertext"}]}),
            )
            .await
            .unwrap();
        });
        let response = post(
            &client(),
            &router,
            "parent",
            &model_alias(&target, "target-model"),
            history,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_full_native_history_switch_preserves_messages_and_tools() {
        let (router, upstream, source, target, model) = fixture().await;
        let history = json!([
            {"role":"user","content":"inspect the file"},
            {"type":"function_call","call_id":"call-1","name":"read_file","arguments":"{}"},
            {"type":"function_call_output","call_id":"call-1","output":"file contents"},
            {"role":"assistant","content":[{"type":"output_text","text":"found it"}]}
        ]);
        let expected = history.clone();
        let upstream_task = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["input"], expected);
                write_json_response(&mut socket, 200, &json!({"id":"reply","output":[]}))
                    .await
                    .unwrap();
            }
        });
        let client = client();
        assert_eq!(
            post(
                &client,
                &router,
                "parent",
                &model_alias(&source, &model),
                history.clone()
            )
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
            200
        );
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        assert_eq!(
            router.portable_compaction("prepare", &payload).unwrap()["required"],
            false
        );
        assert_eq!(
            post(
                &client,
                &router,
                "parent",
                &model_alias(&target, "target-model"),
                history
            )
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
            200
        );
        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn failed_native_compaction_preserves_portable_history_and_source_binding() {
        for (status, output) in [
            (429, json!({"error":{"message":"quota exhausted"}})),
            (
                200,
                json!({"id":"invalid","status":"completed","output":[]}),
            ),
        ] {
            let (router, upstream, source, target, model) = fixture().await;
            let source_route = router
                .snapshot
                .read()
                .unwrap()
                .target_for_request(&model, Some(&source), None)
                .unwrap();
            router.bindings.lock().unwrap().remember_resolved(
                &["thread-id:parent".into()],
                &source_route,
                false,
                false,
                Some(true),
            );
            let before = router.bindings.lock().unwrap().routes["thread-id:parent"].clone();
            let upstream_task = tokio::spawn(async move {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                assert_eq!(
                    incoming_header(&request, "authorization"),
                    Some("Bearer sk-target")
                );
                write_json_response(&mut socket, status, &output)
                    .await
                    .unwrap();
            });
            let response = post(
                &client(), &router, "parent", &model_alias(&target, "target-model"),
                json!([{"role":"user","content":"complete visible history"},{"type":"compaction_trigger"}]),
            ).send().await.unwrap();
            assert_eq!(
                response.status().as_u16(),
                if status == 200 { 502 } else { status }
            );
            let _ = response.bytes().await.unwrap();
            upstream_task.await.unwrap();
            assert_binding_unchanged(&router, &before);
            assert_eq!(router.portable_compaction("prepare", &json!({
                "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
            })).unwrap()["required"], false);
            assert!(router.bindings.lock().unwrap().portable_tickets.is_empty());
            router.stop().await.unwrap();
        }
    }

    #[tokio::test]
    async fn native_compaction_binding_commit_preserves_later_requests_and_session_rules() {
        let (router, _upstream, source, target, model) = fixture().await;
        let source_route = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        let target_route = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request("target-model", Some(&target), None)
            .unwrap();
        let keys = [
            "thread-id:parent".into(),
            "session-id:parent".into(),
            "thread-id:created-later".into(),
            "thread-id:removed-later".into(),
            "thread-id:new".into(),
        ];
        router.bindings.lock().unwrap().remember_resolved(
            &keys[..2],
            &source_route,
            true,
            false,
            Some(true),
        );
        router.bindings.lock().unwrap().remember_resolved(
            &keys[3..4],
            &source_route,
            false,
            false,
            Some(true),
        );
        let session_before = router.bindings.lock().unwrap().routes[&keys[1]].clone();
        let pending =
            PendingNativeCompactionBinding::prepare(&router.bindings, &keys, &target_route, false);
        assert!(router.bindings.lock().unwrap().routes[&keys[0]].portable);
        router.bindings.lock().unwrap().remember_resolved(
            &[keys[0].clone(), keys[2].clone()],
            &source_route,
            false,
            false,
            Some(true),
        );
        let later = router.bindings.lock().unwrap().routes[&keys[0]].clone();
        let created_later = router.bindings.lock().unwrap().routes[&keys[2]].clone();
        router.bindings.lock().unwrap().routes.remove(&keys[3]);
        pending.complete();
        assert_binding_unchanged(&router, &later);
        {
            let bindings = router.bindings.lock().unwrap();
            assert_eq!(
                bindings.routes[&keys[1]].generation,
                session_before.generation
            );
            assert!(bindings.routes[&keys[1]].portable);
            assert_eq!(
                bindings.routes[&keys[2]].generation,
                created_later.generation
            );
            assert_eq!(bindings.routes[&keys[2]].provider_id, source);
            assert!(!bindings.routes.contains_key(&keys[3]));
            assert!(!bindings.routes.contains_key(&keys[4]));
            assert!(
                bindings
                    .order
                    .contains(&(keys[1].clone(), session_before.generation))
            );
        }
        PendingNativeCompactionBinding::prepare(&router.bindings, &keys[4..], &target_route, false)
            .complete();
        {
            let bindings = router.bindings.lock().unwrap();
            assert_eq!(bindings.routes[&keys[4]].provider_id, target);
            assert!(!bindings.routes[&keys[4]].portable);
        }
        PendingNativeCompactionBinding::prepare(&router.bindings, &keys[..2], &target_route, true)
            .complete();
        for key in &keys[..2] {
            let bindings = router.bindings.lock().unwrap();
            assert_eq!(bindings.routes[key].provider_id, target);
            assert!(!bindings.routes[key].portable);
        }
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn native_compaction_binding_commit_rejects_created_then_evicted_key() {
        let (router, _upstream, source, target, model) = fixture().await;
        let snapshot = router.snapshot.read().unwrap().clone();
        let source_route = snapshot
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        let target_route = snapshot
            .target_for_request("target-model", Some(&target), None)
            .unwrap();
        let keys = ["thread-id:created-then-evicted".into()];
        let pending =
            PendingNativeCompactionBinding::prepare(&router.bindings, &keys, &target_route, false);
        {
            let mut bindings = router.bindings.lock().unwrap();
            bindings.remember_resolved(&keys, &source_route, false, false, Some(true));
            let other_keys = (0..MAX_ROUTE_BINDINGS)
                .map(|index| format!("thread-id:other-{index}"))
                .collect::<Vec<_>>();
            bindings.remember_resolved(&other_keys, &source_route, false, false, Some(true));
            assert!(!bindings.routes.contains_key(&keys[0]));
        }
        let before = router.bindings.lock().unwrap().clone();
        pending.complete();
        {
            let bindings = router.bindings.lock().unwrap();
            assert!(!bindings.routes.contains_key(&keys[0]));
            assert_eq!(bindings.routes.len(), MAX_ROUTE_BINDINGS);
            assert_eq!(bindings.next_generation, before.next_generation);
            assert_eq!(bindings.order, before.order);
            for (key, expected) in &before.routes {
                assert_eq!(bindings.routes[key].generation, expected.generation);
            }
        }
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn native_compaction_existing_binding_commits_after_unrelated_write() {
        let (router, _upstream, source, target, model) = fixture().await;
        let snapshot = router.snapshot.read().unwrap().clone();
        let source_route = snapshot
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        let target_route = snapshot
            .target_for_request("target-model", Some(&target), None)
            .unwrap();
        let keys = ["thread-id:existing".into()];
        router.bindings.lock().unwrap().remember_resolved(
            &keys,
            &source_route,
            false,
            false,
            Some(true),
        );
        let pending =
            PendingNativeCompactionBinding::prepare(&router.bindings, &keys, &target_route, false);
        router.bindings.lock().unwrap().remember_resolved(
            &["thread-id:unrelated".into()],
            &source_route,
            false,
            false,
            Some(true),
        );
        pending.complete();
        {
            let bindings = router.bindings.lock().unwrap();
            assert_eq!(bindings.routes[&keys[0]].provider_id, target);
            assert!(!bindings.routes[&keys[0]].portable);
            assert_eq!(bindings.routes["thread-id:unrelated"].provider_id, source);
            assert!(bindings.routes["thread-id:unrelated"].portable);
        }
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn pending_native_compaction_does_not_mutate_full_binding_cache() {
        let (router, _upstream, source, target, model) = fixture().await;
        let snapshot = router.snapshot.read().unwrap().clone();
        let source_route = snapshot
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        let target_route = snapshot
            .target_for_request("target-model", Some(&target), None)
            .unwrap();
        let mut keys = (0..MAX_ROUTE_BINDINGS)
            .map(|index| format!("thread-id:existing-{index}"))
            .collect::<Vec<_>>();
        router.bindings.lock().unwrap().remember_resolved(
            &keys,
            &source_route,
            false,
            false,
            Some(true),
        );
        let before = router.bindings.lock().unwrap().clone();
        keys = vec![
            keys[0].clone(),
            "session-id:missing".into(),
            "thread-id:missing".into(),
        ];
        let mut pending = Some(PendingNativeCompactionBinding::prepare(
            &router.bindings,
            &keys,
            &target_route,
            true,
        ));
        for prepared in [true, false] {
            let bindings = router.bindings.lock().unwrap();
            assert_eq!(
                bindings.next_generation, before.next_generation,
                "prepared={prepared}"
            );
            assert_eq!(bindings.order, before.order, "prepared={prepared}");
            assert_eq!(bindings.routes.len(), MAX_ROUTE_BINDINGS);
            for (key, expected) in &before.routes {
                let actual = &bindings.routes[key];
                assert_eq!(actual.generation, expected.generation);
                assert_eq!(actual.provider_id, expected.provider_id);
                assert_eq!(actual.upstream_model, expected.upstream_model);
                assert_eq!(actual.context_config, expected.context_config);
                assert_eq!(actual.portable, expected.portable);
            }
            drop(bindings);
            if prepared {
                // 失败、取消和预热退出不调用 complete，也不需要回滚。
                drop(pending.take());
            }
        }
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn cross_route_unresolved_delta_is_rejected_before_native_http_send() {
        let (router, upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        router
            .bindings
            .lock()
            .unwrap()
            .routes
            .get_mut("thread-id:parent")
            .unwrap()
            .portable = true;
        let before = router.bindings.lock().unwrap().routes["thread-id:parent"].clone();
        let client = client();
        let response = client
            .post(format!("{}/responses", router.endpoint.base_url))
            .bearer_auth(&router.endpoint.token)
            .header("thread-id", "parent")
            .json(&json!({"model":model_alias(&target,"target-model"),
                "previous_response_id":"source-private-id","input":"new delta"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "context_not_portable"
        );
        assert_binding_unchanged(&router, &before);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), upstream.accept())
                .await
                .is_err()
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_native_history_can_switch_without_deleted_source_and_strip_reasoning_ciphertext()
     {
        let (router, upstream, source, target, model) = fixture().await;
        let source_route = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        router.bindings.lock().unwrap().remember_resolved(
            &["thread-id:parent".into()],
            &source_route,
            false,
            false,
            Some(true),
        );
        let mut snapshot = router.snapshot.read().unwrap().as_ref().clone();
        snapshot.routes.remove(&source);
        *router.snapshot.write().unwrap() = Arc::new(snapshot);
        assert_eq!(
            router
                .portable_compaction(
                    "prepare",
                    &json!({
                        "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
                    })
                )
                .unwrap()["required"],
            false
        );
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(!body.to_string().contains("source-reasoning-secret"));
            assert!(body.to_string().contains("visible reasoning summary"));
            assert!(body.to_string().contains("complete user history"));
            assert!(!body.to_string().contains("compaction_trigger"));
            write_json_response(&mut socket, 200, &json!({"id":"reply","output":[]}))
                .await
                .unwrap();
        });
        let response = post(
            &client(),
            &router,
            "parent",
            &model_alias(&target, "target-model"),
            json!([
                {"role":"user","content":"complete user history"},
                {"type":"reasoning","encrypted_content":"source-reasoning-secret",
                    "summary":[{"type":"summary_text","text":"visible reasoning summary"}]}
            ]),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let _ = response.bytes().await.unwrap();
        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_incremental_history_preserves_opaque_state_and_identity() {
        let (router, _upstream, source, target, model) = fixture().await;
        let mut resolved = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        let keys = ["thread-id:parent".into()];
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        for (native, history, expected) in [
            (false, None, false),
            (false, Some(true), true),
            (false, None, true),
            (true, Some(true), false),
            (false, None, false),
            (false, Some(false), false),
            (false, Some(true), true),
        ] {
            router
                .bindings
                .lock()
                .unwrap()
                .remember_resolved(&keys, &resolved, false, native, history);
            assert_eq!(
                router.bindings.lock().unwrap().routes[&keys[0]].portable,
                expected
            );
        }
        resolved.upstream_model = "changed-model".into();
        router
            .bindings
            .lock()
            .unwrap()
            .remember_resolved(&keys, &resolved, false, false, None);
        assert!(!router.bindings.lock().unwrap().routes[&keys[0]].portable);
        assert_eq!(
            router.portable_compaction("prepare", &payload).unwrap()["required"],
            true
        );
        let token = router.bindings.lock().unwrap().portable_tickets[&keys[0]]
            .token
            .clone();
        router
            .portable_compaction("cancel", &json!({"threadId":"parent","token":token}))
            .unwrap();
        assert_eq!(
            router
                .portable_compaction(
                    "prepare",
                    &json!({"threadId":"parent","targetProviderId":source,"targetModel":model})
                )
                .unwrap()["required"],
            false
        );
        router.bindings.lock().unwrap().remember_resolved(
            &keys,
            &resolved,
            false,
            false,
            Some(true),
        );
        router
            .bindings
            .lock()
            .unwrap()
            .routes
            .get_mut(&keys[0])
            .unwrap()
            .context_config = Some([1; 32]);
        router
            .bindings
            .lock()
            .unwrap()
            .remember_resolved(&keys, &resolved, false, false, None);
        assert!(!router.bindings.lock().unwrap().routes[&keys[0]].portable);
        router.stop().await.unwrap();
    }

    #[test]
    fn portable_compaction_binding_keys_isolate_session_only_children() {
        let mut request = HttpRequest {
            method: "POST".into(),
            path: "/responses".into(),
            headers: vec![("session-id".into(), "parent".into())],
            body: vec![],
            _body_budget_permit: None,
        };
        assert_eq!(
            request_binding_keys(&request),
            ["thread-id:parent", "session-id:parent"]
        );
        assert_eq!(
            portable_request_key(&request).as_deref(),
            Some("thread-id:parent")
        );
        for header in ["x-openai-subagent", "x-codex-parent-thread-id"] {
            request.headers.push((header.into(), "parent".into()));
            assert_eq!(request_binding_keys(&request), ["session-id:parent"]);
            assert_eq!(portable_request_key(&request), None);
            request.headers.pop();
        }
        request.headers.push(("thread-id".into(), "child".into()));
        assert_eq!(
            request_binding_keys(&request),
            ["thread-id:child", "session-id:parent"]
        );
        assert_eq!(
            portable_request_key(&request).as_deref(),
            Some("thread-id:child")
        );
    }

    #[tokio::test]
    async fn portable_compaction_session_only_round_trip_refreshes_source_binding() {
        let (router, upstream, source, target, model) = fixture().await;
        // Seed a stale shared fallback as an earlier request would have done.
        let mut resolved = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        resolved.upstream_model = "old-model".into();
        router.bindings.lock().unwrap().remember_resolved(
            &["session-id:parent".into()],
            &resolved,
            false,
            false,
            None,
        );
        let old_generation = router.bindings.lock().unwrap().routes["session-id:parent"].generation;
        let upstream_task = tokio::spawn(async move {
            for stage in 0..3 {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body["model"],
                    if stage == 2 {
                        "target-model"
                    } else {
                        "provider-model"
                    }
                );
                if stage == 1 {
                    assert_eq!(body["input"][0]["encrypted_content"], "opaque");
                } else if stage == 2 {
                    assert_eq!(body["input"][0]["content"][0]["text"], "portable handoff");
                }
                write_json_response(&mut socket, 200, &json!({"id":"reply","status":"completed",
                    "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"portable handoff"}]}]})).await.unwrap();
            }
        });
        let client = client();
        let send = |model: String, input: Value| {
            client
                .post(format!("{}/responses", router.endpoint.base_url))
                .bearer_auth(&router.endpoint.token)
                .header("session-id", "parent")
                .json(&json!({"model":model,"input":input,"stream":false}))
        };
        // No route metadata: this must still refresh the session model/generation.
        assert_eq!(
            send(
                model.clone(),
                json!([{"type":"compaction","encrypted_content":"opaque"}])
            )
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
            200
        );
        {
            let bindings = router.bindings.lock().unwrap();
            assert_eq!(bindings.routes["session-id:parent"].upstream_model, model);
            assert_ne!(
                bindings.routes["session-id:parent"].generation,
                old_generation
            );
            assert_eq!(bindings.routes["thread-id:parent"].upstream_model, model);
        }
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        let prepared = router.portable_compaction("prepare", &payload).unwrap();
        assert_eq!(prepared["required"], true);
        assert_eq!(prepared["sourceModel"], model);
        let handle = json!({"threadId":"parent","token":prepared["token"]});
        let compact = json!([{"type":"compaction","encrypted_content":"opaque"},{"type":"compaction_trigger"}]);
        assert_eq!(
            send(model.clone(), json!("blocked"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            409
        );
        for header in ["x-openai-subagent", "x-codex-parent-thread-id"] {
            let response = send(model_alias(&target, "target-model"), compact.clone())
                .header(header, "parent")
                .send()
                .await
                .unwrap();
            assert_ne!(response.status().as_u16(), 200);
            assert_eq!(
                router.portable_compaction("status", &handle).unwrap()["state"],
                "prepared"
            );
        }
        let response = send(model_alias(&target, "target-model"), compact)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let result: Value = response.json().await.unwrap();
        router.portable_compaction("confirm", &handle).unwrap();
        let response = send(
            model_alias(&target, "target-model"),
            result["output"].clone(),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        // A subsequent normal turn advances the exact binding, invalidating old confirmation.
        assert!(router.portable_compaction("confirm", &handle).is_err());
        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_terminal_ticket_requires_explicit_cancel_for_native_recovery() {
        for state in ["failed", "expired"] {
            let (router, upstream, source, target, model) = fixture().await;
            bind_source(&router, &source, &model);
            let prepared = router
                .portable_compaction(
                    "prepare",
                    &json!({
                        "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
                    }),
                )
                .unwrap();
            let handle = json!({"threadId":"parent","token":prepared["token"]});
            router
                .bindings
                .lock()
                .unwrap()
                .portable_tickets
                .get_mut("thread-id:parent")
                .unwrap()
                .state = state;
            let compact = json!([{"type":"compaction_trigger"}]);
            let response = post(
                &client(),
                &router,
                "parent",
                &model_alias(&source, &model),
                compact.clone(),
            )
            .send()
            .await
            .unwrap();
            assert_eq!(response.status().as_u16(), 409);
            assert_eq!(
                router.portable_compaction("status", &handle).unwrap()["state"],
                state
            );
            router.portable_compaction("cancel", &handle).unwrap();
            assert!(router.portable_compaction("status", &handle).is_err());
            let upstream_task = tokio::spawn(async move {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                assert_eq!(
                    incoming_header(&request, "authorization"),
                    Some("Bearer sk-upstream")
                );
                write_json_response(
                    &mut socket,
                    200,
                    &json!({"id":"native","status":"completed",
                    "output":[{"type":"compaction","encrypted_content":"native-ciphertext"}]}),
                )
                .await
                .unwrap();
            });
            let response = post(
                &client(),
                &router,
                "parent",
                &model_alias(&source, &model),
                compact,
            )
            .send()
            .await
            .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            assert_eq!(
                response.json::<Value>().await.unwrap()["output"][0]["encrypted_content"],
                "native-ciphertext"
            );
            upstream_task.await.unwrap();
            router.stop().await.unwrap();
        }
    }

    #[tokio::test]
    async fn portable_compaction_running_cannot_cancel_and_new_native_compaction_invalidates_confirmation()
     {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        let prepared = router.portable_compaction("prepare", &payload).unwrap();
        let handle = json!({"threadId":"parent","token":prepared["token"]});
        let route = Arc::clone(&router.snapshot.read().unwrap().routes[&source]);
        router
            .bindings
            .lock()
            .unwrap()
            .portable_tickets
            .get_mut("thread-id:parent")
            .unwrap()
            .state = "running";
        assert!(router.portable_compaction("cancel", &handle).is_err());
        let attempt = PortableAttempt {
            bindings: Arc::clone(&router.bindings),
            key: "thread-id:parent".into(),
            token: prepared["token"].as_str().unwrap().into(),
            generation: router.bindings.lock().unwrap().routes["thread-id:parent"].generation,
            source: route,
            model: model.clone(),
            target: Arc::clone(&router.snapshot.read().unwrap().routes[&target]),
            target_model: "target-model".into(),
            uses_target: false,
            retry_source: false,
            completed: false,
        };
        drop(attempt);
        assert_eq!(
            router.portable_compaction("status", &handle).unwrap()["state"],
            "failed"
        );
        let prepared = router.portable_compaction("prepare", &payload).unwrap();
        let handle = json!({"threadId":"parent","token":prepared["token"]});
        router
            .bindings
            .lock()
            .unwrap()
            .portable_tickets
            .get_mut("thread-id:parent")
            .unwrap()
            .state = "completed";
        router.portable_compaction("confirm", &handle).unwrap();
        let resolved = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        router.bindings.lock().unwrap().remember_resolved(
            &["thread-id:parent".into()],
            &resolved,
            false,
            true,
            Some(true),
        );
        assert!(router.portable_compaction("confirm", &handle).is_err());
        assert_eq!(
            router.portable_compaction("prepare", &payload).unwrap()["required"],
            true
        );
        router.stop().await.unwrap();
    }

    fn acquire_attempt(router: &LocalRouter) -> Result<PortableAttempt> {
        let mut server = super::super::lifecycle_tests::test_server(&CodeyConfig::default());
        server.bindings = Arc::clone(&router.bindings);
        let request = HttpRequest {
            method: "POST".into(),
            path: "/responses".into(),
            headers: vec![("thread-id".into(), "parent".into())],
            body: vec![],
            _body_budget_permit: None,
        };
        PortableAttempt::acquire(&server, &request, true, &router.snapshot.read().unwrap())?
            .context("missing portable attempt")
    }

    fn assert_binding_unchanged(router: &LocalRouter, expected: &RouteBinding) {
        let bindings = router.bindings.lock().unwrap();
        let actual = &bindings.routes["thread-id:parent"];
        assert_eq!(actual.provider_id, expected.provider_id);
        assert_eq!(actual.upstream_model, expected.upstream_model);
        assert_eq!(actual.context_config, expected.context_config);
        assert_eq!(actual.generation, expected.generation);
        assert_eq!(actual.portable, expected.portable);
    }

    #[tokio::test]
    async fn portable_compaction_restored_history_uses_resolved_target_only_until_confirmation() {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let original = router.bindings.lock().unwrap().routes["thread-id:parent"].clone();
        // A qualified model determines the actual route even when a stale hint disagrees.
        assert_eq!(
            router
                .portable_compaction(
                    "prepare",
                    &json!({
                        "threadId":"parent","targetProviderId":target,
                        "targetModel":model_alias(&source, &model)
                    })
                )
                .unwrap()["required"],
            false
        );
        let prepared = router
            .portable_compaction(
                "prepare",
                &json!({
                    "threadId":"parent","targetProviderId":source,
                    "targetModel":model_alias(&target, "target-model")
                }),
            )
            .unwrap();
        assert_eq!(prepared["required"], true);
        let handle = json!({"threadId":"parent","token":prepared["token"]});
        let mut attempt = acquire_attempt(&router).unwrap();
        assert_eq!(attempt.route().provider_id, source);
        assert_eq!(attempt.upstream_model(), model);
        let mut cache = PortableHistoryCache::default();
        let input = json!([
            {"role":"user","content":"complete readable history"},
            {"type":"compaction_trigger"}
        ]);
        let checkpoint = cache
            .capture(
                "thread-id:parent",
                &attempt.source,
                &model,
                &json!({"model":model,"input":input,"stream":false}),
            )
            .unwrap();
        let compaction = json!({"type":"compaction","encrypted_content":"source-ciphertext"});
        cache.remember(
            checkpoint,
            &json!({
                "id":"native","status":"completed","output":[compaction.clone()]
            }),
        );
        let mut body = json!({"model":model_alias(&target, "target-model"),"input":[
            compaction, {"role":"user","content":"continue here"},
            {"type":"compaction_trigger"}
        ],"stream":false});
        assert!(prepare_test_target(&mut attempt, &mut body, &mut cache));
        assert!(attempt.uses_target);
        assert_eq!(attempt.route().provider_id, target);
        assert_eq!(attempt.upstream_model(), "target-model");
        assert_eq!(attempt.source.provider_id, source);
        assert_eq!(attempt.model, model);
        assert_eq!(
            body["input"],
            json!([
                {"role":"user","content":"complete readable history"},
                {"role":"user","content":"continue here"},
                {"type":"compaction_trigger"}
            ])
        );
        assert!(!body.to_string().contains("source-ciphertext"));
        assert_binding_unchanged(&router, &original);
        assert!(router.portable_compaction("confirm", &handle).is_err());
        attempt.complete();
        drop(attempt);
        assert_binding_unchanged(&router, &original);
        router.portable_compaction("confirm", &handle).unwrap();
        let mut confirmed = original;
        confirmed.portable = true;
        assert_binding_unchanged(&router, &confirmed);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_unrestorable_history_keeps_source_and_original_body() {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        router
            .portable_compaction(
                "prepare",
                &json!({
                    "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
                }),
            )
            .unwrap();
        let mut attempt = acquire_attempt(&router).unwrap();
        let mut cache = PortableHistoryCache::default();
        let checkpoint = cache
            .capture(
                "thread-id:parent",
                &attempt.source,
                &model,
                &json!({"input":[{"role":"user","content":"readable history"}]}),
            )
            .unwrap();
        let compact = json!({"type":"compaction","encrypted_content":"known-ciphertext"});
        cache.remember(
            checkpoint,
            &json!({
                "id":"native","status":"completed","output":[compact.clone()]
            }),
        );
        for original in [
            json!({"input":[{"type":"compaction","encrypted_content":"unknown"},{"type":"compaction_trigger"}]}),
            json!({"input":[compact.clone(),{"type":"item_reference","id":"source-item"},{"type":"compaction_trigger"}]}),
            json!({"input":[compact,{"type":"compaction_trigger"}],"previous_response_id":"source-response"}),
        ] {
            let mut body = original.clone();
            assert!(!prepare_test_target(&mut attempt, &mut body, &mut cache));
            assert_eq!(body, original);
            assert!(!attempt.uses_target);
            assert_eq!(attempt.route().provider_id, source);
            assert_eq!(attempt.upstream_model(), model);
        }
        drop(attempt);
        assert!(!router.bindings.lock().unwrap().routes["thread-id:parent"].portable);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_route_changes_reject_acquire_and_confirm_without_rebinding() {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let original = router.bindings.lock().unwrap().routes["thread-id:parent"].clone();
        let snapshot = Arc::clone(&router.snapshot.read().unwrap());
        let payload = json!({
            "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
        });
        for phase in ["acquire", "confirm"] {
            for provider in [&source, &target] {
                for change in ["removed", "identity", "model"] {
                    *router.snapshot.write().unwrap() = Arc::clone(&snapshot);
                    let prepared = router.portable_compaction("prepare", &payload).unwrap();
                    let handle = json!({"threadId":"parent","token":prepared["token"]});
                    if phase == "confirm" {
                        let mut attempt = acquire_attempt(&router).unwrap();
                        attempt.complete();
                        drop(attempt);
                    }
                    let mut changed = (*snapshot).clone();
                    if change == "removed" {
                        changed.routes.remove(provider);
                    } else {
                        let route = Arc::make_mut(changed.routes.get_mut(provider).unwrap());
                        if change == "identity" {
                            route.context_config = [1; 32];
                        } else {
                            route.models.clear();
                        }
                    }
                    *router.snapshot.write().unwrap() = Arc::new(changed);
                    if phase == "acquire" {
                        assert!(acquire_attempt(&router).is_err(), "{provider}: {change}");
                        assert_eq!(
                            router.portable_compaction("status", &handle).unwrap()["state"],
                            "failed"
                        );
                    } else {
                        assert!(
                            router.portable_compaction("confirm", &handle).is_err(),
                            "{provider}: {change}"
                        );
                    }
                    assert_binding_unchanged(&router, &original);
                }
            }
        }
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_changed_source_binding_cannot_be_confirmed() {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let original = router.bindings.lock().unwrap().routes["thread-id:parent"].clone();
        for change in ["provider", "model", "identity", "generation"] {
            router
                .bindings
                .lock()
                .unwrap()
                .routes
                .insert("thread-id:parent".into(), original.clone());
            let prepared = router
                .portable_compaction(
                    "prepare",
                    &json!({
                        "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
                    }),
                )
                .unwrap();
            let mut attempt = acquire_attempt(&router).unwrap();
            attempt.complete();
            drop(attempt);
            let mut changed = original.clone();
            match change {
                "provider" => changed.provider_id = target.clone(),
                "model" => changed.upstream_model = "changed-model".into(),
                "identity" => changed.context_config = Some([1; 32]),
                "generation" => changed.generation += 1,
                _ => unreachable!(),
            }
            router
                .bindings
                .lock()
                .unwrap()
                .routes
                .insert("thread-id:parent".into(), changed.clone());
            assert!(
                router
                    .portable_compaction(
                        "confirm",
                        &json!({
                            "threadId":"parent","token":prepared["token"]
                        })
                    )
                    .is_err(),
                "{change}"
            );
            assert_binding_unchanged(&router, &changed);
        }
        router.stop().await.unwrap();
    }

    async fn fixture() -> (LocalRouter, TcpListener, String, String, String) {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, source, model) = super::super::tests::router_config(format!(
            "http://{}/v1",
            upstream.local_addr().unwrap()
        ));
        config.profiles[0].supports_remote_compaction = true;
        let mut target = config.profiles[0].clone();
        target.id = "route-b".into();
        target.name = "Target".into();
        target.api_key = "sk-target".into();
        let target_id = target.provider_id().to_string();
        config.profiles.push(target);
        config
            .selected_models_by_provider
            .insert(target_id.clone(), vec!["target-model".into()]);
        config.model_context_by_provider.insert(
            target_id.clone(),
            BTreeMap::from([(
                "target-model".into(),
                crate::config::ModelContextConfig {
                    context_window_tokens: 1_000_000,
                    auto_compact_token_limit: None,
                    reserve_output_tokens: None,
                },
            )]),
        );
        let router = LocalRouter::start(&config).await.unwrap();
        (router, upstream, source, target_id, model)
    }

    fn target_metadata(window: u64) -> serde_json::Map<String, Value> {
        json!({"context_window":window,"codey_context_source":"user_declared"})
            .as_object()
            .unwrap()
            .clone()
    }

    fn prepare_test_target(
        attempt: &mut PortableAttempt,
        body: &mut Value,
        cache: &mut PortableHistoryCache,
    ) -> bool {
        attempt.prepare_target(
            body,
            &cache.snapshot(),
            Some(&target_metadata(1_000_000)),
            &mut None,
            &Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
        )
    }

    #[tokio::test]
    async fn portable_target_checks_declared_capacity_and_request_budget_before_copying() {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        router.portable_compaction("prepare", &json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"})).unwrap();
        let mut attempt = acquire_attempt(&router).unwrap();
        let mut cache = PortableHistoryCache::default();
        let checkpoint = cache
            .capture(
                "thread-id:parent",
                &attempt.source,
                &model,
                &json!({"input":[{"role":"user","content":"x".repeat(100_000)}]}),
            )
            .unwrap();
        let opaque = json!({"type":"compaction","encrypted_content":"known"});
        cache.remember(
            checkpoint,
            &json!({"status":"completed","output":[opaque.clone()]}),
        );
        let snapshot = cache.snapshot();
        let original =
            json!({"model":"target-model","input":[opaque,{"type":"compaction_trigger"}]});
        let mut body = original.clone();
        let pool = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
        let mut permit = acquire_request_body_budget(&pool, original.to_string().len()).unwrap();
        let held = permit.as_ref().unwrap().num_permits();
        for metadata in [
            None,
            Some(target_metadata(8192)),
            // 输入本身能放下，但加上摘要输出预算后不足。
            Some(target_metadata(110_000)),
            Some(
                json!({"context_window":1_000_000,"max_context_window":8192,
                "codey_context_source":"user_declared"})
                .as_object()
                .unwrap()
                .clone(),
            ),
            Some(
                json!({"context_window":1_000_000,"codey_context_source":"conservative_fallback"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ] {
            assert!(!attempt.prepare_target(
                &mut body,
                &snapshot,
                metadata.as_ref(),
                &mut permit,
                &pool
            ));
            assert_eq!(body, original);
            assert_eq!(attempt.route().provider_id, source);
            assert_eq!(permit.as_ref().unwrap().num_permits(), held);
        }
        let blocker = Arc::clone(&pool)
            .try_acquire_many_owned(pool.available_permits() as u32)
            .unwrap();
        assert!(!attempt.prepare_target(
            &mut body,
            &snapshot,
            Some(&target_metadata(1_000_000)),
            &mut permit,
            &pool
        ));
        assert_eq!(
            snapshot
                .materializations
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(body, original);
        assert_eq!(permit.as_ref().unwrap().num_permits(), held);
        drop(blocker);
        assert!(attempt.prepare_target(
            &mut body,
            &snapshot,
            Some(&target_metadata(1_000_000)),
            &mut permit,
            &pool
        ));
        assert_eq!(
            snapshot
                .materializations
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(attempt.route().provider_id, target);
        assert!(permit.as_ref().unwrap().num_permits() > held);
        assert_eq!(
            permit.as_ref().unwrap().num_permits() + pool.available_permits(),
            REQUEST_BODY_BUDGET_PERMITS
        );
        drop(permit);
        assert_eq!(pool.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
        drop(attempt);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_target_failed_materialization_releases_only_reserved_growth() {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        router.portable_compaction("prepare", &json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"})).unwrap();
        let mut attempt = acquire_attempt(&router).unwrap();
        let mut cache = PortableHistoryCache::default();
        let checkpoint = cache
            .capture(
                "thread-id:parent",
                &attempt.source,
                &model,
                &json!({"input":[{"role":"user","content":"x".repeat(100_000)}]}),
            )
            .unwrap();
        let opaque = json!({"type":"compaction","encrypted_content":"known"});
        cache.remember(
            checkpoint,
            &json!({"status":"completed","output":[opaque.clone()]}),
        );
        let snapshot = cache.snapshot();
        let pool = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
        let metadata = target_metadata(1_000_000);
        let mut materializations = 0;
        for has_permit in [false, true] {
            for encoded in ["bad!", ""] {
                let original = json!({"model":"target-model","input":[
                    opaque.clone(),
                    {"type":"compaction","encrypted_content":format!("{CODEY_SUMMARY_PREFIX}{encoded}")},
                    {"type":"compaction_trigger"}
                ]});
                let mut body = original.clone();
                let mut permit = if has_permit {
                    acquire_request_body_budget(&pool, original.to_string().len()).unwrap()
                } else {
                    None
                };
                let held = permit.as_ref().map(OwnedSemaphorePermit::num_permits);
                let available = pool.available_permits();
                let required = snapshot
                    .restoration("thread-id:parent", &attempt.source, &model, &body)
                    .unwrap()
                    .request_budget_bytes()
                    .unwrap();
                assert!(request_body_budget_permit_count(required).unwrap() > held.unwrap_or(0));
                assert!(!attempt.prepare_target(
                    &mut body,
                    &snapshot,
                    Some(&metadata),
                    &mut permit,
                    &pool,
                ));
                materializations += 1;
                assert_eq!(
                    snapshot
                        .materializations
                        .load(std::sync::atomic::Ordering::Relaxed),
                    materializations
                );
                assert_eq!(body, original);
                assert_eq!(attempt.route().provider_id, source);
                assert!(!attempt.uses_target);
                assert_eq!(permit.as_ref().map(OwnedSemaphorePermit::num_permits), held);
                assert_eq!(pool.available_permits(), available);
                drop(permit);
                assert_eq!(pool.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
            }
        }
        let mut body = json!({"input":[opaque,{"type":"compaction_trigger"}]});
        let mut permit = None;
        assert!(attempt.prepare_target(&mut body, &snapshot, Some(&metadata), &mut permit, &pool));
        assert_eq!(attempt.route().provider_id, target);
        assert!(permit.as_ref().unwrap().num_permits() > 0);
        assert_eq!(
            permit.as_ref().unwrap().num_permits() + pool.available_permits(),
            REQUEST_BODY_BUDGET_PERMITS
        );
        drop(permit);
        assert_eq!(pool.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
        drop(attempt);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_small_target_window_keeps_source_credentials_and_ciphertext() {
        let (router, upstream, source, target, model) = fixture().await;
        {
            let mut snapshot = router.snapshot.write().unwrap();
            Arc::make_mut(&mut snapshot)
                .declared_model_contexts
                .get_mut(&model_alias(&target, "target-model"))
                .unwrap()
                .context_window_tokens = 8192;
        }
        let mock = tokio::spawn(async move {
            for native in [true, false] {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                assert_eq!(
                    incoming_header(&request, "authorization"),
                    Some("Bearer sk-upstream")
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["model"], "provider-model");
                if native {
                    assert_eq!(body["input"][0]["content"].as_str().unwrap().len(), 12_000);
                    write_json_response(
                        &mut socket,
                        200,
                        &json!({"id":"native","status":"completed","output":[
                            {"type":"compaction","encrypted_content":"small-target-source-window"}
                        ]}),
                    )
                    .await
                    .unwrap();
                } else {
                    assert_eq!(
                        body["input"][0]["encrypted_content"],
                        "small-target-source-window"
                    );
                    write_json_response(&mut socket, 200, &json!({"id":"summary","status":"completed","output":[
                        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"source handoff fits target"}]}
                    ]})).await.unwrap();
                }
            }
        });
        let client = client();
        let compacted = post(
            &client,
            &router,
            "parent",
            &model_alias(&source, &model),
            json!([
                {"role":"user","content":"x".repeat(12_000)},{"type":"compaction_trigger"}
            ]),
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
        let original = router.bindings.lock().unwrap().routes["thread-id:parent"].clone();
        let prepared = router.portable_compaction("prepare", &json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"})).unwrap();
        let mut input = compacted["output"].as_array().unwrap().clone();
        input.push(json!({"type":"compaction_trigger"}));
        let response = post(
            &client,
            &router,
            "parent",
            &model_alias(&target, "target-model"),
            json!(input),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_binding_unchanged(&router, &original);
        router
            .portable_compaction(
                "confirm",
                &json!({"threadId":"parent","token":prepared["token"]}),
            )
            .unwrap();
        mock.await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_target_rejection_fails_before_drop_and_preserves_compaction_exclusion() {
        let (router, _upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        let prepared = router.portable_compaction("prepare", &payload).unwrap();
        let handle = json!({"threadId":"parent","token":prepared["token"]});
        let mut attempt = acquire_attempt(&router).unwrap();
        let keys = vec!["thread-id:parent".into(), "session-id:parent".into()];
        let guard = CompactionGuard::acquire(&router.bindings, keys.clone()).unwrap();
        attempt.uses_target = true;
        attempt.target_context_rejected();
        assert_eq!(
            router.portable_compaction("status", &handle).unwrap()["state"],
            "failed"
        );
        router.portable_compaction("cancel", &handle).unwrap();
        assert!(
            router
                .bindings
                .lock()
                .unwrap()
                .portable_source_retries
                .contains_key("thread-id:parent")
        );
        assert!(CompactionGuard::acquire(&router.bindings, keys).is_err());
        assert!(router.portable_compaction("prepare", &payload).is_err());
        drop(guard);
        let retry = router.portable_compaction("prepare", &payload).unwrap();
        let retry_handle = json!({"threadId":"parent","token":retry["token"]});
        assert_eq!(retry["handoffRoute"], "source");
        assert!(
            router
                .bindings
                .lock()
                .unwrap()
                .portable_source_retries
                .contains_key("thread-id:parent")
        );
        let mut retry_attempt = acquire_attempt(&router).unwrap();
        assert!(retry_attempt.retry_source);
        // 旧请求仍可能在写错误响应，之后的拒绝通知和 Drop 都不能改变新任务。
        attempt.target_context_rejected();
        attempt.complete();
        drop(attempt);
        assert_eq!(
            router.portable_compaction("status", &retry_handle).unwrap()["state"],
            "running"
        );
        assert!(
            router
                .bindings
                .lock()
                .unwrap()
                .portable_source_retries
                .is_empty()
        );
        retry_attempt.complete();
        drop(retry_attempt);
        assert_eq!(
            router.portable_compaction("status", &retry_handle).unwrap()["state"],
            "completed"
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_target_rejection_requires_current_ticket_and_source_identity() {
        for change in [
            "binding_generation",
            "binding_identity",
            "ticket_generation",
            "target_identity",
            "token",
            "source_route",
        ] {
            let (router, _upstream, source, target, model) = fixture().await;
            bind_source(&router, &source, &model);
            router.portable_compaction("prepare", &json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"})).unwrap();
            let mut attempt = acquire_attempt(&router).unwrap();
            attempt.uses_target = change != "source_route";
            {
                let mut bindings = router.bindings.lock().unwrap();
                let binding = bindings.routes.get_mut("thread-id:parent").unwrap();
                match change {
                    "binding_generation" => binding.generation += 1,
                    "binding_identity" => binding.context_config = Some([1; 32]),
                    _ => {}
                }
                let ticket = bindings
                    .portable_tickets
                    .get_mut("thread-id:parent")
                    .unwrap();
                match change {
                    "ticket_generation" => ticket.generation += 1,
                    "target_identity" => Arc::make_mut(&mut ticket.target).context_config[0] ^= 1,
                    "token" => ticket.token.push_str("-new"),
                    _ => {}
                }
            }
            attempt.target_context_rejected();
            {
                let bindings = router.bindings.lock().unwrap();
                assert_eq!(
                    bindings.portable_tickets["thread-id:parent"].state, "running",
                    "{change}"
                );
                assert!(bindings.portable_source_retries.is_empty(), "{change}");
            }
            drop(attempt);
            router.stop().await.unwrap();
        }
    }

    #[tokio::test]
    async fn portable_source_retry_is_once_scoped_expiring_and_cancelable() {
        for change in [
            "consume",
            "cancel",
            "generation",
            "target_config",
            "expired",
            "expires_after_prepare",
        ] {
            let (router, _upstream, source, target, model) = fixture().await;
            bind_source(&router, &source, &model);
            let payload =
                json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
            let prepared = router.portable_compaction("prepare", &payload).unwrap();
            let mut attempt = acquire_attempt(&router).unwrap();
            attempt.uses_target = true;
            attempt.target_context_rejected();
            drop(attempt);
            router
                .portable_compaction(
                    "cancel",
                    &json!({"threadId":"parent","token":prepared["token"]}),
                )
                .unwrap();
            match change {
                "generation" => {
                    router
                        .bindings
                        .lock()
                        .unwrap()
                        .routes
                        .get_mut("thread-id:parent")
                        .unwrap()
                        .generation += 1
                }
                "target_config" => {
                    let mut snapshot = router.snapshot.write().unwrap();
                    Arc::make_mut(
                        Arc::make_mut(&mut snapshot)
                            .routes
                            .get_mut(&target)
                            .unwrap(),
                    )
                    .context_config[0] ^= 1;
                }
                "expired" => {
                    router
                        .bindings
                        .lock()
                        .unwrap()
                        .portable_source_retries
                        .get_mut("thread-id:parent")
                        .unwrap()
                        .expires_at = Instant::now() - Duration::from_secs(1)
                }
                _ => {}
            }
            let retry = router.portable_compaction("prepare", &payload).unwrap();
            assert_eq!(
                retry["handoffRoute"],
                if matches!(change, "consume" | "cancel" | "expires_after_prepare") {
                    "source"
                } else {
                    "automatic"
                }
            );
            if change == "expires_after_prepare" {
                router
                    .bindings
                    .lock()
                    .unwrap()
                    .portable_source_retries
                    .get_mut("thread-id:parent")
                    .unwrap()
                    .expires_at = Instant::now() - Duration::from_secs(1);
                assert!(!acquire_attempt(&router).unwrap().retry_source);
            } else if change == "consume" {
                let attempt = acquire_attempt(&router).unwrap();
                assert!(attempt.retry_source);
                assert!(!attempt.uses_target);
                assert!(
                    router
                        .bindings
                        .lock()
                        .unwrap()
                        .portable_source_retries
                        .is_empty()
                );
                drop(attempt);
                assert_eq!(
                    router.portable_compaction("prepare", &payload).unwrap()["handoffRoute"],
                    "automatic"
                );
            } else if change == "cancel" {
                router
                    .portable_compaction(
                        "cancel",
                        &json!({"threadId":"parent","token":retry["token"]}),
                    )
                    .unwrap();
                assert!(
                    router
                        .bindings
                        .lock()
                        .unwrap()
                        .portable_source_retries
                        .is_empty()
                );
                assert_eq!(
                    router.portable_compaction("prepare", &payload).unwrap()["handoffRoute"],
                    "automatic"
                );
            }
            router.stop().await.unwrap();
        }
    }

    fn bind_source(router: &LocalRouter, provider: &str, model: &str) {
        let resolved = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request(model, Some(provider), None)
            .unwrap();
        router.bindings.lock().unwrap().remember_resolved(
            &["thread-id:parent".into()],
            &resolved,
            false,
            false,
            None,
        );
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn post(
        client: &reqwest::Client,
        router: &LocalRouter,
        thread: &str,
        model: &str,
        input: Value,
    ) -> reqwest::RequestBuilder {
        client
            .post(format!("{}/responses", router.endpoint.base_url))
            .bearer_auth(&router.endpoint.token)
            .header("thread-id", thread)
            .json(&json!({"model":model,"input":input,"stream":false}))
    }

    #[tokio::test]
    async fn portable_compaction_cached_native_window_uses_target_credentials_and_keeps_binding() {
        for target_status in [200, 429, 400] {
            let (router, upstream, source, target, model) = fixture().await;
            let history = json!([
                {"role":"developer","content":"preserve the user's constraints"},
                {"role":"user","content":"inspect the file"},
                {"type":"function_call","call_id":"read-1","name":"read_file","arguments":"{}"},
                {"type":"function_call_output","call_id":"read-1","output":"complete file contents"},
                {"role":"assistant","content":"inspection complete"}
            ]);
            let expected_history = history.as_array().unwrap().clone();
            let opaque = json!({"type":"compaction","encrypted_content":"source-only-window"});
            let expected_opaque = opaque.clone();
            let mock = tokio::spawn(async move {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                assert_eq!(
                    incoming_header(&request, "authorization"),
                    Some("Bearer sk-upstream")
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body["input"].as_array().unwrap().last().unwrap()["type"],
                    "compaction_trigger"
                );
                write_json_response(
                    &mut socket,
                    200,
                    &json!({
                        "id":"native-window","status":"completed","output":[expected_opaque]
                    }),
                )
                .await
                .unwrap();
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                // The old account may have exhausted its quota; it receives no summary request.
                assert_eq!(
                    incoming_header(&request, "authorization"),
                    Some("Bearer sk-target")
                );
                assert_eq!(request.path, "/v1/responses");
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["model"], "target-model");
                let input = body["input"].as_array().unwrap();
                assert_eq!(
                    &input[..expected_history.len()],
                    expected_history.as_slice()
                );
                assert_eq!(
                    input[expected_history.len()]["content"],
                    "continue with the new account"
                );
                assert!(!body.to_string().contains("source-only-window"));
                assert!(!body.to_string().contains("compaction_trigger"));
                assert!(body.get("previous_response_id").is_none());
                if target_status != 200 {
                    write_json_response(&mut socket, target_status,
                        &if target_status == 400 { json!({"error":{"code":"context_length_exceeded","message":"target context too small"}}) }
                        else { json!({"error":{"code":"rate_limit_exceeded","message":"target quota exhausted"}}) })
                        .await.unwrap();
                    if target_status == 400 {
                        let (mut socket, _) = upstream.accept().await.unwrap();
                        let request = read_http_request(&mut socket).await.unwrap();
                        assert_eq!(
                            incoming_header(&request, "authorization"),
                            Some("Bearer sk-upstream")
                        );
                        let body: Value = serde_json::from_slice(&request.body).unwrap();
                        assert_eq!(body["input"][0]["encrypted_content"], "source-only-window");
                        assert_eq!(body["model"], "provider-model");
                        write_json_response(&mut socket, 200, &json!({"id":"source-summary","status":"completed","output":[
                            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"source generated handoff"}]}
                        ]})).await.unwrap();
                    }
                    return;
                }
                write_json_response(&mut socket, 200, &json!({"id":"summary","status":"completed","output":[
                    {"type":"message","role":"assistant","content":[{"type":"output_text","text":"portable summary with file evidence"}]}
                ]})).await.unwrap();
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                assert_eq!(
                    incoming_header(&request, "authorization"),
                    Some("Bearer sk-target")
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body["input"][0]["content"][0]["text"],
                    "portable summary with file evidence"
                );
                write_json_response(&mut socket, 200, &json!({"id":"continued","output":[]}))
                    .await
                    .unwrap();
            });
            let client = client();
            let mut first = history.as_array().unwrap().clone();
            first.push(json!({"type":"compaction_trigger"}));
            let response = post(
                &client,
                &router,
                "parent",
                &model_alias(&source, &model),
                json!(first),
            )
            .send()
            .await
            .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let compacted = response.json::<Value>().await.unwrap();
            assert_eq!(compacted["output"], json!([opaque]));
            let original = router.bindings.lock().unwrap().routes["thread-id:parent"].clone();
            assert!(!original.portable);
            let prepared = router
                .portable_compaction(
                    "prepare",
                    &json!({
                        "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
                    }),
                )
                .unwrap();
            assert_eq!(prepared["required"], true);
            let handle = json!({"threadId":"parent","token":prepared["token"]});
            let response = post(&client, &router, "parent", &model_alias(&target, "target-model"), json!([
                opaque, {"role":"user","content":"continue with the new account"}, {"type":"compaction_trigger"}
            ])).send().await.unwrap();
            assert_eq!(response.status().as_u16(), target_status);
            assert_binding_unchanged(&router, &original);
            if target_status == 200 {
                let summary = response.json::<Value>().await.unwrap();
                assert!(
                    summary["output"][0]["encrypted_content"]
                        .as_str()
                        .unwrap()
                        .starts_with(CODEY_SUMMARY_PREFIX)
                );
                router.portable_compaction("confirm", &handle).unwrap();
                let response = post(
                    &client,
                    &router,
                    "parent",
                    &model_alias(&target, "target-model"),
                    summary["output"].clone(),
                )
                .send()
                .await
                .unwrap();
                assert_eq!(response.status().as_u16(), 200);
            } else {
                let error = response.text().await.unwrap();
                assert!(error.contains("目标账号交接摘要失败"), "{error}");
                assert!(
                    error.contains(if target_status == 400 {
                        "target context too small"
                    } else {
                        "target quota exhausted"
                    }),
                    "{error}"
                );
                assert_eq!(
                    router.portable_compaction("status", &handle).unwrap()["state"],
                    "failed"
                );
                assert!(router.portable_compaction("confirm", &handle).is_err());
                assert_binding_unchanged(&router, &original);
                if target_status == 400 {
                    // Matches the UI failure flow: status -> cancel cleanup -> a new explicit prepare.
                    router.portable_compaction("cancel", &handle).unwrap();
                    let retried = router.portable_compaction("prepare", &json!({
                        "threadId":"parent","targetProviderId":target,"targetModel":"target-model"
                    })).unwrap();
                    assert_eq!(retried["handoffRoute"], "source");
                    let response = post(&client, &router, "parent", &model_alias(&target, "target-model"), json!([
                        {"type":"compaction","encrypted_content":"source-only-window"},
                        {"role":"user","content":"continue with the new account"},{"type":"compaction_trigger"}
                    ])).send().await.unwrap();
                    assert_eq!(response.status().as_u16(), 200);
                    assert_binding_unchanged(&router, &original);
                    router
                        .portable_compaction(
                            "confirm",
                            &json!({"threadId":"parent","token":retried["token"]}),
                        )
                        .unwrap();
                    assert!(
                        router
                            .bindings
                            .lock()
                            .unwrap()
                            .portable_source_retries
                            .is_empty()
                    );
                }
            }
            mock.await.unwrap();
            router.stop().await.unwrap();
        }
    }

    #[tokio::test]
    async fn portable_compaction_source_ciphertext_becomes_confirmed_target_history() {
        let (router, upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let prepare =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        assert_eq!(router.portable_compaction("prepare", &json!({"threadId":"missing","targetProviderId":target,"targetModel":"target-model"})).unwrap()["required"], false);
        assert_eq!(
            router
                .portable_compaction(
                    "prepare",
                    &json!({"threadId":"parent","targetProviderId":source,"targetModel":model})
                )
                .unwrap()["required"],
            false
        );
        let client = client();
        let opaque = json!([{"type":"compaction","encrypted_content":"source-only-ciphertext"}]);
        let rejected = post(
            &client,
            &router,
            "parent",
            &model_alias(&target, "target-model"),
            opaque.clone(),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(rejected.status().as_u16(), 400);
        assert_eq!(
            router.bindings.lock().unwrap().routes["thread-id:parent"].provider_id,
            source
        );
        let prepared = router.portable_compaction("prepare", &prepare).unwrap();
        let handle = json!({"threadId":"parent","token":prepared["token"]});
        assert_eq!(prepared["sourceModel"], model);
        assert!(router.portable_compaction("prepare", &prepare).is_err());
        assert!(
            router
                .portable_compaction(
                    "status",
                    &json!({"threadId":"child","token":prepared["token"]})
                )
                .is_err()
        );
        let blocked = post(
            &client,
            &router,
            "parent",
            &model_alias(&source, &model),
            json!("new turn"),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(blocked.status().as_u16(), 409);
        // A child's concrete ID must override a shared parent session header.
        let child_source = router
            .snapshot
            .read()
            .unwrap()
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        router.bindings.lock().unwrap().remember_resolved(
            &["thread-id:child".into()],
            &child_source,
            false,
            false,
            None,
        );
        let child = post(
            &client,
            &router,
            "child",
            &model_alias(&target, "target-model"),
            opaque.clone(),
        )
        .header("session-id", "parent")
        .header("x-codex-parent-thread-id", "parent")
        .send()
        .await
        .unwrap();
        assert_ne!(child.status().as_u16(), 200);
        assert_eq!(
            router.portable_compaction("status", &handle).unwrap()["state"],
            "prepared"
        );
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            assert_eq!(request.path, "/v1/responses");
            assert_eq!(
                incoming_header(&request, "authorization"),
                Some("Bearer sk-upstream")
            );
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["model"], "provider-model");
            assert_eq!(
                body["input"][0]["encrypted_content"],
                "source-only-ciphertext"
            );
            assert!(
                body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|item| item["type"] != "compaction_trigger")
            );
            write_json_response(&mut socket, 200, &json!({"id":"summary","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"portable handoff"}]}]})).await.unwrap();
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            assert_eq!(
                incoming_header(&request, "authorization"),
                Some("Bearer sk-target")
            );
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["input"][0]["content"][0]["text"], "portable handoff");
            write_json_response(
                &mut socket,
                200,
                &json!({"id":"target","status":"completed","output":[]}),
            )
            .await
            .unwrap();
        });
        let mut compact = opaque.as_array().unwrap().clone();
        compact.push(json!({"type":"compaction_trigger"}));
        let response = post(
            &client,
            &router,
            "parent",
            &model_alias(&target, "target-model"),
            json!(compact),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let result = response.json::<Value>().await.unwrap();
        assert!(
            result["output"][0]["encrypted_content"]
                .as_str()
                .unwrap()
                .starts_with(CODEY_SUMMARY_PREFIX)
        );
        assert_eq!(
            router.portable_compaction("status", &handle).unwrap()["state"],
            "completed"
        );
        assert!(!router.bindings.lock().unwrap().routes["thread-id:parent"].portable);
        router.portable_compaction("confirm", &handle).unwrap();
        assert_eq!(
            router.portable_compaction("prepare", &prepare).unwrap()["required"],
            false
        );
        let response = post(
            &client,
            &router,
            "parent",
            &model_alias(&target, "target-model"),
            result["output"].clone(),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn portable_compaction_failure_expiry_cancel_and_identity_changes_do_not_confirm() {
        let (router, upstream, source, target, model) = fixture().await;
        bind_source(&router, &source, &model);
        let payload =
            json!({"threadId":"parent","targetProviderId":target,"targetModel":"target-model"});
        let prepare = router.portable_compaction("prepare", &payload).unwrap();
        let handle = json!({"threadId":"parent","token":prepare["token"]});
        router.portable_compaction("cancel", &handle).unwrap();
        assert!(router.portable_compaction("status", &handle).is_err());
        let prepare = router.portable_compaction("prepare", &payload).unwrap();
        let handle = json!({"threadId":"parent","token":prepare["token"]});
        router
            .bindings
            .lock()
            .unwrap()
            .portable_tickets
            .get_mut("thread-id:parent")
            .unwrap()
            .created = Instant::now() - PREPARE_TTL;
        assert_eq!(
            router.portable_compaction("status", &handle).unwrap()["state"],
            "expired"
        );
        let prepare = router.portable_compaction("prepare", &payload).unwrap();
        let handle = json!({"threadId":"parent","token":prepare["token"]});
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            read_http_request(&mut socket).await.unwrap();
            write_json_response(&mut socket, 400, &json!({"error":{"message":"failed"}}))
                .await
                .unwrap();
        });
        let response = post(&client(), &router, "parent", &model_alias(&target, "target-model"), json!([{"type":"compaction","encrypted_content":"opaque"},{"type":"compaction_trigger"}])).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 400);
        assert_eq!(
            router.portable_compaction("status", &handle).unwrap()["state"],
            "failed"
        );
        assert!(router.portable_compaction("confirm", &handle).is_err());
        assert_eq!(
            router.bindings.lock().unwrap().routes["thread-id:parent"].provider_id,
            source
        );
        upstream_task.await.unwrap();
        let prepared = router.portable_compaction("prepare", &payload).unwrap();
        let handle = json!({"threadId":"parent","token":prepared["token"]});
        router
            .bindings
            .lock()
            .unwrap()
            .routes
            .get_mut("thread-id:parent")
            .unwrap()
            .context_config = Some([1; 32]);
        let response = post(
            &client(),
            &router,
            "parent",
            &model_alias(&target, "target-model"),
            json!([{"type":"compaction_trigger"}]),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 409);
        assert_eq!(
            router.portable_compaction("status", &handle).unwrap()["state"],
            "failed"
        );
        router.stop().await.unwrap();
    }
}
