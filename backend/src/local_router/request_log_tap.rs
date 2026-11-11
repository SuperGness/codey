use super::*;

pub(crate) struct RequestLogObservedChunk {
    pub(crate) bytes: Bytes,
    pub(crate) written_at: Instant,
    pub(crate) _queue_budget: OwnedSemaphorePermit,
}

pub(crate) struct RequestLogResponseTap {
    pub(crate) sender: Option<mpsc::Sender<RequestLogObservedChunk>>,
    pub(crate) queue_budget: Arc<Semaphore>,
    pub(crate) probe: RouteRequestLogProbe,
}

impl RequestLogResponseTap {
    pub(crate) fn new(probe: RouteRequestLogProbe) -> Self {
        let (sender, mut receiver) =
            mpsc::channel::<RequestLogObservedChunk>(REQUEST_LOG_TAP_QUEUE_CHUNKS);
        let queue_budget = Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS));
        let worker_probe = probe.clone();
        let finish_guard = probe.defer_finish();
        tokio::spawn(async move {
            let _finish_guard = finish_guard;
            let mut projector = RequestLogMetadataProjector::default();
            while let Some(chunk) = receiver.recv().await {
                if let Err(reason) =
                    projector.observe(&chunk.bytes, &worker_probe, chunk.written_at)
                {
                    worker_probe.mark_usage_unavailable(reason);
                    break;
                }
            }
            if projector.usage.is_some() {
                worker_probe.mark_usage_unavailable("usage_projection_failed");
            }
        });
        Self {
            sender: Some(sender),
            queue_budget,
            probe,
        }
    }

    pub(crate) fn observe(&mut self, chunk: &Bytes) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let permits = chunk
            .len()
            .div_ceil(REQUEST_LOG_TAP_CHUNK_BYTES)
            .clamp(1, REQUEST_LOG_TAP_QUEUE_CHUNKS) as u32;
        let Ok(queue_budget) = Arc::clone(&self.queue_budget).try_acquire_many_owned(permits)
        else {
            self.probe.mark_usage_unavailable("observer_queue_full");
            self.sender = None;
            return;
        };
        let observed = RequestLogObservedChunk {
            bytes: chunk.clone(),
            written_at: Instant::now(),
            _queue_budget: queue_budget,
        };
        match sender.try_send(observed) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.probe.mark_usage_unavailable("observer_queue_full");
                self.sender = None;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => self.sender = None,
        }
    }

    pub(crate) fn finish(&mut self) {
        self.sender = None;
    }
}

#[derive(Default)]
pub(crate) struct RequestLogMetadataProjector {
    response_depth: Option<usize>,
    message_depth: Option<usize>,
    awaiting_response: bool,
    awaiting_message: bool,
    pub(crate) depth: usize,
    pub(crate) in_string: bool,
    pub(crate) escaped: bool,
    pub(crate) string: Vec<u8>,
    pub(crate) string_overflow: bool,
    pub(crate) last_key: Option<ProjectedMetadataKey>,
    pub(crate) awaiting_usage: bool,
    pub(crate) awaiting_scalar: Option<ProjectedMetadataScalar>,
    pub(crate) capturing_scalar: Option<ProjectedMetadataScalar>,
    pub(crate) event_type_has_user_content: bool,
    pub(crate) event_delta_has_content: bool,
    event_type_has_output_delta: bool,
    event_delta_quarters: u64,
    delta_counter: ProjectedDeltaCounter,
    pub(crate) usage: Option<UsageCapture>,
}

/// 增量计数字符，跳过 JSON 编码和 UTF-8 延续字节；不缓冲输出正文。
#[derive(Default)]
struct ProjectedDeltaCounter {
    quarters: u64,
    escaped: bool,
    unicode_remaining: u8,
    unicode_value: u16,
}

impl ProjectedDeltaCounter {
    fn observe(&mut self, byte: u8) {
        if self.unicode_remaining > 0 {
            let Some(digit) = (byte as char).to_digit(16) else {
                self.unicode_remaining = 0;
                return;
            };
            self.unicode_value = (self.unicode_value << 4) | digit as u16;
            self.unicode_remaining -= 1;
            if self.unicode_remaining == 0 && !(0xdc00..=0xdfff).contains(&self.unicode_value) {
                self.quarters =
                    self.quarters
                        .saturating_add(if self.unicode_value < 128 { 1 } else { 4 });
            }
        } else if self.escaped {
            self.escaped = false;
            if byte == b'u' {
                self.unicode_remaining = 4;
                self.unicode_value = 0;
            } else {
                self.quarters = self.quarters.saturating_add(1);
            }
        } else if byte == b'\\' {
            self.escaped = true;
        } else if byte != b'"' && byte < 128 {
            self.quarters = self.quarters.saturating_add(1);
        } else if byte >= 0xc2 {
            self.quarters = self.quarters.saturating_add(4);
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ProjectedMetadataKey {
    Response,
    Message,
    ServiceTier,
    Usage,
    Type,
    Delta,
    Model,
    Status,
    Code,
}

#[derive(Clone, Copy)]
pub(crate) enum ProjectedMetadataScalar {
    ServiceTier,
    EventType,
    Delta,
    ResponseModel,
    ResponseStatus,
    ErrorCode,
}

pub(crate) struct UsageCapture {
    pub(crate) containers: Vec<UsageContainer>,
    pub(crate) values: [Option<(u8, u64)>; 6],
    pub(crate) in_string: bool,
    pub(crate) escaped: bool,
    pub(crate) unicode_escape_digits: u8,
    pub(crate) string_is_key: bool,
    pub(crate) string: Vec<u8>,
    pub(crate) string_overflow: bool,
    pub(crate) scalar: Vec<u8>,
    pub(crate) scalar_overflow: bool,
    pub(crate) scalar_field: Option<(ProjectedUsageField, u8)>,
}

#[derive(Clone, Copy)]
pub(crate) enum UsageObjectContext {
    Root,
    CachedDetails(u8),
    ReasoningDetails(u8),
    Other,
}

#[derive(Clone, Copy)]
pub(crate) enum ProjectedUsageKey {
    Field(ProjectedUsageField, u8),
    Object(UsageObjectContext),
    Other,
}

#[derive(Clone, Copy)]
pub(crate) enum ProjectedUsageField {
    Input,
    Output,
    CachedInput,
    CacheCreationInput,
    ReasoningOutput,
    Total,
}

impl ProjectedUsageField {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Input => 0,
            Self::Output => 1,
            Self::CachedInput => 2,
            Self::CacheCreationInput => 3,
            Self::ReasoningOutput => 4,
            Self::Total => 5,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum UsageJsonState {
    ObjectKeyOrEnd,
    ObjectKey,
    ObjectColon,
    ObjectValue,
    ObjectCommaOrEnd,
    ArrayValueOrEnd,
    ArrayValue,
    ArrayCommaOrEnd,
}

pub(crate) struct UsageContainer {
    pub(crate) context: Option<UsageObjectContext>,
    pub(crate) state: UsageJsonState,
    pub(crate) key: ProjectedUsageKey,
}

impl UsageCapture {
    pub(crate) fn new() -> Self {
        Self {
            containers: vec![UsageContainer {
                context: Some(UsageObjectContext::Root),
                state: UsageJsonState::ObjectKeyOrEnd,
                key: ProjectedUsageKey::Other,
            }],
            values: [None; 6],
            in_string: false,
            escaped: false,
            unicode_escape_digits: 0,
            string_is_key: false,
            string: Vec::new(),
            string_overflow: false,
            scalar: Vec::new(),
            scalar_overflow: false,
            scalar_field: None,
        }
    }

    pub(crate) fn observe_byte(&mut self, byte: u8) -> std::result::Result<bool, &'static str> {
        if self.in_string {
            if self.unicode_escape_digits > 0 {
                if !byte.is_ascii_hexdigit() {
                    return Err("usage_projection_failed");
                }
                self.unicode_escape_digits -= 1;
            } else if self.escaped {
                if !matches!(
                    byte,
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u'
                ) {
                    return Err("usage_projection_failed");
                }
                self.escaped = false;
                if byte == b'u' {
                    self.unicode_escape_digits = 4;
                }
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.in_string = false;
                if self.string_is_key {
                    self.finish_key()?;
                }
                return Ok(false);
            } else if byte < b' ' {
                return Err("usage_projection_failed");
            }
            if self.string_is_key && !self.string_overflow {
                if self.string.len() < REQUEST_LOG_USAGE_KEY_BYTES {
                    self.string.push(byte);
                } else {
                    self.string_overflow = true;
                }
            }
            return Ok(false);
        }

        if !self.scalar.is_empty() || self.scalar_overflow {
            if byte.is_ascii_whitespace() || matches!(byte, b',' | b'}' | b']') {
                self.finish_scalar()?;
            } else {
                if self.scalar.len() < REQUEST_LOG_USAGE_SCALAR_BYTES {
                    self.scalar.push(byte);
                } else {
                    self.scalar_overflow = true;
                }
                return Ok(false);
            }
        }

        if byte.is_ascii_whitespace() {
            return Ok(false);
        }

        match byte {
            b'"' => {
                let state = self
                    .containers
                    .last()
                    .map(|container| container.state)
                    .ok_or("usage_projection_failed")?;
                self.string_is_key = matches!(
                    state,
                    UsageJsonState::ObjectKeyOrEnd | UsageJsonState::ObjectKey
                );
                if !self.string_is_key {
                    self.begin_value()?;
                }
                self.string.clear();
                self.string_overflow = false;
                self.in_string = true;
                self.escaped = false;
                self.unicode_escape_digits = 0;
            }
            b'{' | b'[' => {
                let key = self.begin_value()?;
                if self.containers.len() >= REQUEST_LOG_USAGE_NESTING_DEPTH {
                    return Err("usage_projection_limit_exceeded");
                }
                self.containers.push(if byte == b'{' {
                    UsageContainer {
                        context: Some(match key {
                            ProjectedUsageKey::Object(context) => context,
                            _ => UsageObjectContext::Other,
                        }),
                        state: UsageJsonState::ObjectKeyOrEnd,
                        key: ProjectedUsageKey::Other,
                    }
                } else {
                    UsageContainer {
                        context: None,
                        state: UsageJsonState::ArrayValueOrEnd,
                        key: ProjectedUsageKey::Other,
                    }
                });
            }
            b'}' => {
                let Some(container) = self.containers.last() else {
                    return Err("usage_projection_failed");
                };
                if container.context.is_none()
                    || !matches!(
                        container.state,
                        UsageJsonState::ObjectKeyOrEnd | UsageJsonState::ObjectCommaOrEnd
                    )
                {
                    return Err("usage_projection_failed");
                }
                self.containers.pop();
                if self.containers.is_empty() {
                    return Ok(true);
                }
            }
            b']' => {
                let Some(container) = self.containers.last() else {
                    return Err("usage_projection_failed");
                };
                if container.context.is_some()
                    || !matches!(
                        container.state,
                        UsageJsonState::ArrayValueOrEnd | UsageJsonState::ArrayCommaOrEnd
                    )
                {
                    return Err("usage_projection_failed");
                }
                self.containers.pop();
            }
            b':' => {
                let container = self
                    .containers
                    .last_mut()
                    .ok_or("usage_projection_failed")?;
                if !matches!(container.state, UsageJsonState::ObjectColon) {
                    return Err("usage_projection_failed");
                }
                container.state = UsageJsonState::ObjectValue;
            }
            b',' => {
                let container = self
                    .containers
                    .last_mut()
                    .ok_or("usage_projection_failed")?;
                container.state = match container.state {
                    UsageJsonState::ObjectCommaOrEnd => UsageJsonState::ObjectKey,
                    UsageJsonState::ArrayCommaOrEnd => UsageJsonState::ArrayValue,
                    _ => return Err("usage_projection_failed"),
                };
            }
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                let key = self.begin_value()?;
                self.scalar.clear();
                self.scalar.push(byte);
                self.scalar_overflow = false;
                self.scalar_field = match key {
                    ProjectedUsageKey::Field(field, priority) => Some((field, priority)),
                    _ => None,
                };
            }
            _ => return Err("usage_projection_failed"),
        }
        Ok(false)
    }

    pub(crate) fn begin_value(&mut self) -> std::result::Result<ProjectedUsageKey, &'static str> {
        let container = self
            .containers
            .last_mut()
            .ok_or("usage_projection_failed")?;
        match container.state {
            UsageJsonState::ObjectValue => {
                container.state = UsageJsonState::ObjectCommaOrEnd;
                Ok(std::mem::replace(
                    &mut container.key,
                    ProjectedUsageKey::Other,
                ))
            }
            UsageJsonState::ArrayValueOrEnd | UsageJsonState::ArrayValue => {
                container.state = UsageJsonState::ArrayCommaOrEnd;
                Ok(ProjectedUsageKey::Other)
            }
            _ => Err("usage_projection_failed"),
        }
    }

    pub(crate) fn finish_key(&mut self) -> std::result::Result<(), &'static str> {
        let container = self
            .containers
            .last_mut()
            .ok_or("usage_projection_failed")?;
        let context = container.context.ok_or("usage_projection_failed")?;
        if !matches!(
            container.state,
            UsageJsonState::ObjectKeyOrEnd | UsageJsonState::ObjectKey
        ) {
            return Err("usage_projection_failed");
        }
        container.key = if self.string_overflow {
            ProjectedUsageKey::Other
        } else if !self.string.contains(&b'\\') {
            std::str::from_utf8(&self.string).map_err(|_| "usage_projection_failed")?;
            projected_usage_key(context, &self.string)
        } else {
            let mut encoded = Vec::with_capacity(self.string.len() + 2);
            encoded.push(b'"');
            encoded.extend_from_slice(&self.string);
            encoded.push(b'"');
            let key = serde_json::from_slice::<String>(&encoded)
                .map_err(|_| "usage_projection_failed")?;
            projected_usage_key(context, key.as_bytes())
        };
        container.state = UsageJsonState::ObjectColon;
        Ok(())
    }

    pub(crate) fn finish_scalar(&mut self) -> std::result::Result<(), &'static str> {
        if self.scalar_overflow {
            if self.scalar_field.is_some() {
                return Err("usage_projection_failed");
            }
        } else {
            let parsed_u64 = match self.scalar.as_slice() {
                b"null" | b"true" | b"false" => None,
                b"0" => Some(0_u64),
                [b'1'..=b'9', rest @ ..] if rest.iter().all(u8::is_ascii_digit) => {
                    if let Some(value) = std::str::from_utf8(&self.scalar)
                        .ok()
                        .and_then(|digits| digits.parse::<u64>().ok())
                    {
                        Some(value)
                    } else {
                        serde_json::from_slice::<Value>(&self.scalar)
                            .map_err(|_| "usage_projection_failed")?
                            .as_u64()
                    }
                }
                _ => serde_json::from_slice::<Value>(&self.scalar)
                    .map_err(|_| "usage_projection_failed")?
                    .as_u64(),
            };
            if let (Some((field, priority)), Some(value)) = (self.scalar_field, parsed_u64) {
                let slot = &mut self.values[field.index()];
                if slot.is_none_or(|(current_priority, _)| priority <= current_priority) {
                    *slot = Some((priority, value));
                }
            }
        }
        self.scalar.clear();
        self.scalar_overflow = false;
        self.scalar_field = None;
        Ok(())
    }

    pub(crate) fn into_value(self) -> Value {
        let value = |field: ProjectedUsageField| self.values[field.index()].map(|(_, value)| value);
        json!({
            "input_tokens": value(ProjectedUsageField::Input),
            "output_tokens": value(ProjectedUsageField::Output),
            "input_tokens_details": {
                "cached_tokens": value(ProjectedUsageField::CachedInput),
            },
            "cache_creation_input_tokens": value(ProjectedUsageField::CacheCreationInput),
            "output_tokens_details": {
                "reasoning_tokens": value(ProjectedUsageField::ReasoningOutput),
            },
            "total_tokens": value(ProjectedUsageField::Total),
        })
    }
}

pub(crate) fn projected_usage_key(context: UsageObjectContext, value: &[u8]) -> ProjectedUsageKey {
    use ProjectedUsageField as Field;
    use ProjectedUsageKey::{Field as KeyField, Object, Other};
    match (context, value) {
        (UsageObjectContext::Root, b"input_tokens") => KeyField(Field::Input, 0),
        (UsageObjectContext::Root, b"prompt_tokens") => KeyField(Field::Input, 1),
        (UsageObjectContext::Root, b"inputTokens") => KeyField(Field::Input, 2),
        (UsageObjectContext::Root, b"output_tokens") => KeyField(Field::Output, 0),
        (UsageObjectContext::Root, b"completion_tokens") => KeyField(Field::Output, 1),
        (UsageObjectContext::Root, b"outputTokens") => KeyField(Field::Output, 2),
        (UsageObjectContext::Root, b"input_tokens_details") => {
            Object(UsageObjectContext::CachedDetails(0))
        }
        (UsageObjectContext::Root, b"prompt_tokens_details") => {
            Object(UsageObjectContext::CachedDetails(1))
        }
        (UsageObjectContext::Root, b"cache_read_input_tokens") => KeyField(Field::CachedInput, 2),
        (UsageObjectContext::Root, b"cache_read_tokens") => KeyField(Field::CachedInput, 3),
        (UsageObjectContext::Root, b"cached_input_tokens") => KeyField(Field::CachedInput, 4),
        (UsageObjectContext::Root, b"cache_creation_input_tokens") => {
            KeyField(Field::CacheCreationInput, 2)
        }
        (UsageObjectContext::Root, b"cache_creation_tokens") => {
            KeyField(Field::CacheCreationInput, 3)
        }
        (UsageObjectContext::Root, b"cache_write_input_tokens") => {
            KeyField(Field::CacheCreationInput, 4)
        }
        (UsageObjectContext::Root, b"output_tokens_details") => {
            Object(UsageObjectContext::ReasoningDetails(0))
        }
        (UsageObjectContext::Root, b"completion_tokens_details") => {
            Object(UsageObjectContext::ReasoningDetails(1))
        }
        (UsageObjectContext::Root, b"reasoning_tokens") => KeyField(Field::ReasoningOutput, 2),
        (UsageObjectContext::Root, b"total_tokens") => KeyField(Field::Total, 0),
        (UsageObjectContext::Root, b"totalTokens") => KeyField(Field::Total, 1),
        (UsageObjectContext::CachedDetails(priority), b"cached_tokens") => {
            KeyField(Field::CachedInput, priority)
        }
        (UsageObjectContext::CachedDetails(priority), b"cache_write_tokens") => {
            KeyField(Field::CacheCreationInput, priority)
        }
        (UsageObjectContext::ReasoningDetails(priority), b"reasoning_tokens") => {
            KeyField(Field::ReasoningOutput, priority)
        }
        _ => Other,
    }
}

impl RequestLogMetadataProjector {
    pub(crate) fn observe(
        &mut self,
        bytes: &[u8],
        probe: &RouteRequestLogProbe,
        written_at: Instant,
    ) -> std::result::Result<(), &'static str> {
        for &byte in bytes {
            if let Some(capture) = self.usage.as_mut() {
                let complete = capture.observe_byte(byte)?;
                if complete {
                    let capture = self.usage.take().expect("usage capture exists");
                    probe.observe_event_at(&json!({"usage": capture.into_value()}), written_at);
                }
                continue;
            }

            if self.in_string {
                if matches!(self.capturing_scalar, Some(ProjectedMetadataScalar::Delta)) {
                    self.delta_counter.observe(byte);
                }
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.in_string = false;
                    if let Some(field) = self.capturing_scalar.take() {
                        match field {
                            ProjectedMetadataScalar::ServiceTier if !self.string_overflow => {
                                probe.observe_service_tier(&String::from_utf8_lossy(&self.string));
                            }
                            ProjectedMetadataScalar::ResponseModel if !self.string_overflow => {
                                probe.observe_upstream_response_model(&String::from_utf8_lossy(
                                    &self.string,
                                ));
                            }
                            ProjectedMetadataScalar::Delta => {
                                self.event_delta_quarters = self.delta_counter.quarters;
                                self.event_delta_has_content =
                                    self.string_overflow || !self.string.is_empty();
                                if self.event_type_has_user_content && self.event_delta_has_content
                                {
                                    probe.mark_first_downstream_content_at(written_at);
                                }
                            }
                            ProjectedMetadataScalar::EventType if !self.string_overflow => {
                                let value = String::from_utf8_lossy(&self.string);
                                self.event_type_has_user_content =
                                    responses_event_type_has_user_content(&value);
                                self.event_type_has_output_delta =
                                    crate::route_request_log::is_output_delta_event(&value);
                                if self.event_type_has_user_content && self.event_delta_has_content
                                {
                                    probe.mark_first_downstream_content_at(written_at);
                                }
                                probe.observe_terminal_projection(Some(&value), None, None)
                            }
                            ProjectedMetadataScalar::ResponseStatus if !self.string_overflow => {
                                let value = String::from_utf8_lossy(&self.string);
                                probe.observe_terminal_projection(None, Some(&value), None)
                            }
                            ProjectedMetadataScalar::ErrorCode if !self.string_overflow => {
                                let value = String::from_utf8_lossy(&self.string);
                                probe.observe_terminal_projection(None, None, Some(&value))
                            }
                            ProjectedMetadataScalar::EventType
                            | ProjectedMetadataScalar::ServiceTier
                            | ProjectedMetadataScalar::ResponseModel
                            | ProjectedMetadataScalar::ResponseStatus
                            | ProjectedMetadataScalar::ErrorCode => {}
                        }
                        self.last_key = None;
                    } else {
                        self.last_key = (!self.string_overflow)
                            .then(|| projected_metadata_key(&self.string))
                            .flatten();
                    }
                } else if self.string.len() < REQUEST_LOG_METADATA_STRING_BYTES {
                    self.string.push(byte);
                } else {
                    self.string_overflow = true;
                }
                continue;
            }

            match byte {
                b'"' => {
                    self.in_string = true;
                    self.escaped = false;
                    self.string.clear();
                    self.string_overflow = false;
                    self.capturing_scalar = self.awaiting_scalar.take();
                    self.delta_counter = ProjectedDeltaCounter::default();
                    self.awaiting_message = false;
                    self.awaiting_usage = false;
                }
                b':' => {
                    let key = self.last_key.take();
                    self.awaiting_response =
                        self.depth == 1 && matches!(key, Some(ProjectedMetadataKey::Response));
                    self.awaiting_message =
                        self.depth == 1 && matches!(key, Some(ProjectedMetadataKey::Message));
                    self.awaiting_usage =
                        self.depth <= 2 && matches!(key, Some(ProjectedMetadataKey::Usage));
                    self.awaiting_scalar = match (self.depth, key) {
                        (depth, Some(ProjectedMetadataKey::ServiceTier))
                            if depth == 1 || self.response_depth == Some(depth) =>
                        {
                            Some(ProjectedMetadataScalar::ServiceTier)
                        }
                        (depth, Some(ProjectedMetadataKey::Model))
                            if depth == 1
                                || self.response_depth == Some(depth)
                                || self.message_depth == Some(depth) =>
                        {
                            Some(ProjectedMetadataScalar::ResponseModel)
                        }
                        (1, Some(ProjectedMetadataKey::Type)) => {
                            Some(ProjectedMetadataScalar::EventType)
                        }
                        (1, Some(ProjectedMetadataKey::Delta)) => {
                            Some(ProjectedMetadataScalar::Delta)
                        }
                        (1 | 2, Some(ProjectedMetadataKey::Status)) => {
                            Some(ProjectedMetadataScalar::ResponseStatus)
                        }
                        (2 | 3, Some(ProjectedMetadataKey::Code)) => {
                            Some(ProjectedMetadataScalar::ErrorCode)
                        }
                        _ => None,
                    };
                }
                b'{' if self.awaiting_usage => {
                    self.awaiting_usage = false;
                    self.awaiting_message = false;
                    self.awaiting_scalar = None;
                    self.usage = Some(UsageCapture::new());
                }
                b'{' => {
                    if self.depth == 0 {
                        self.event_type_has_user_content = false;
                        self.event_delta_has_content = false;
                        self.event_type_has_output_delta = false;
                        self.event_delta_quarters = 0;
                    }
                    self.depth += 1;
                    if self.awaiting_response {
                        self.response_depth = Some(self.depth);
                    }
                    if self.awaiting_message {
                        self.message_depth = Some(self.depth);
                    }
                    self.awaiting_response = false;
                    self.awaiting_message = false;
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
                b'[' => {
                    self.awaiting_response = false;
                    self.awaiting_message = false;
                    self.depth += 1;
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
                b'}' | b']' => {
                    if self.depth == 1 && byte == b'}' && self.event_type_has_output_delta {
                        probe.observe_output_delta_estimate(self.event_delta_quarters, written_at);
                    }
                    if self.response_depth == Some(self.depth) {
                        self.response_depth = None;
                    }
                    if self.message_depth == Some(self.depth) {
                        self.message_depth = None;
                    }
                    self.depth = self.depth.saturating_sub(1);
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
                byte if byte.is_ascii_whitespace() => {}
                _ => {
                    self.awaiting_response = false;
                    self.awaiting_message = false;
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn projected_metadata_key(value: &[u8]) -> Option<ProjectedMetadataKey> {
    match value {
        b"response" => Some(ProjectedMetadataKey::Response),
        b"message" => Some(ProjectedMetadataKey::Message),
        b"service_tier" => Some(ProjectedMetadataKey::ServiceTier),
        b"usage" => Some(ProjectedMetadataKey::Usage),
        b"type" => Some(ProjectedMetadataKey::Type),
        b"delta" => Some(ProjectedMetadataKey::Delta),
        b"model" => Some(ProjectedMetadataKey::Model),
        b"status" => Some(ProjectedMetadataKey::Status),
        b"code" => Some(ProjectedMetadataKey::Code),
        _ => None,
    }
}

#[cfg(test)]
mod billing_tests {
    use super::*;

    #[test]
    fn live_speed_projects_chunked_unicode_without_counting_json_encoding() {
        let bodies = [
            "{\"type\":\"response.output_text.delta\",\"delta\":\"abcd中😀\\n\\\\\\\"\"}"
                .as_bytes()
                .to_vec(),
            br#"{"delta":"abcd\u4e2d\ud83d\ude00\n\\\"","type":"response.output_text.delta"}"#
                .to_vec(),
        ];
        for body in bodies {
            for chunk_size in 1..=17 {
                let probe = RouteRequestLogProbe::detached_test_probe();
                let mut projector = RequestLogMetadataProjector::default();
                for chunk in body.chunks(chunk_size) {
                    projector.observe(chunk, &probe, Instant::now()).unwrap();
                }
                assert_eq!(projector.event_delta_quarters, 15);
                assert!(projector.event_type_has_output_delta);
                assert_eq!(projector.depth, 0);
            }
        }
    }

    #[test]
    fn live_speed_raw_sse_updates_only_for_delta_and_keeps_bounded_memory() {
        let controller = crate::route_request_log::RouteRequestLogController::with_root(
            std::path::PathBuf::new(),
        );
        let registry = controller.session_stats();
        let now = Instant::now();
        let probe = controller
            .begin(|producer| {
                producer.begin(crate::route_request_log::RouteRequestLogStart {
                    request_id: "raw-live",
                    started_at: now - Duration::from_secs(4),
                    request_protocol: crate::route_request_log::RequestProtocol::Sse,
                    request_kind: "responses",
                    requested_model: "model",
                    reasoning_effort: None,
                    thinking_budget_tokens: None,
                    codex_session_id: Some("raw-session"),
                    codex_session_is_parent: false,
                })
            })
            .unwrap();
        let mut projector = RequestLogMetadataProjector::default();
        projector
            .observe(
                br#"data: {"type":"response.created","response":{"usage":{"output_tokens":0}}}"#,
                &probe,
                now - Duration::from_secs(3),
            )
            .unwrap();
        assert_eq!(
            registry.snapshot("raw-session").unwrap().speed_state,
            Some("waiting")
        );
        let content = "x".repeat(256 * 1024);
        let event = format!(
            "data: {{\"delta\":\"{content}\",\"type\":\"response.output_text.delta\"}}\n\n"
        );
        for chunk in event.as_bytes().chunks(3) {
            projector
                .observe(chunk, &probe, now - Duration::from_secs(2))
                .unwrap();
        }
        let active = registry.snapshot("raw-session").unwrap();
        assert_eq!(active.speed_state, Some("streaming"));
        assert!(active.speed_is_estimate);
        assert!(active.output_tokens_per_second.unwrap() > 20_000.0);
        assert_eq!(active.output_tokens, None);
        assert!(projector.string.len() <= REQUEST_LOG_METADATA_STRING_BYTES);
        let repeated = format!(
            "data: {{\"type\":\"response.completed\",\"response\":{{\"output\":[{{\"text\":\"{content}\",\"delta\":\"ignored\"}}]}}}}\n\n"
        );
        projector
            .observe(repeated.as_bytes(), &probe, now - Duration::from_secs(1))
            .unwrap();
        assert_eq!(projector.event_delta_quarters, 0);
        assert!(!projector.event_type_has_output_delta);
        probe.finish_success();
        assert_eq!(registry.snapshot("raw-session").unwrap().speed_state, None);
    }

    #[test]
    fn upstream_reported_model_projects_from_chunked_sse() {
        const CASES: [(&[u8], &str); 3] = [
            (
                br#"data: {"type":"response.created","response":{"id":"resp_1","model":"gpt-5.1-codex"}}"#,
                "gpt-5.1-codex",
            ),
            (
                br#"data: {"id":"chatcmpl_1","model":"deepseek/deepseek-v4.1-flash","choices":[]}"#,
                "deepseek/deepseek-v4.1-flash",
            ),
            (
                br#"data: {"type":"message_start","message":{"id":"msg_1","model":"claude-sonnet-4-5"}}"#,
                "claude-sonnet-4-5",
            ),
        ];
        for (bytes, expected) in CASES {
            let probe = RouteRequestLogProbe::detached_test_probe();
            let mut projector = RequestLogMetadataProjector::default();
            for chunk in bytes.chunks(5) {
                projector.observe(chunk, &probe, Instant::now()).unwrap();
            }
            assert_eq!(
                probe.upstream_response_model_for_test().as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn upstream_reported_model_ignores_nested_and_repeated_values() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut projector = RequestLogMetadataProjector::default();
        for chunk in
            br#"data: {"type":"response.created","response":{"model":"gpt-5.1-codex","output":[{"model":"nested"}]}}"#
                .chunks(7)
        {
            projector.observe(chunk, &probe, Instant::now()).unwrap();
        }
        projector
            .observe(
                b"data: {\"type\":\"response.completed\",\"response\":{\"model\":\"other\"}}\n\n",
                &probe,
                Instant::now(),
            )
            .unwrap();
        assert_eq!(
            probe.upstream_response_model_for_test().as_deref(),
            Some("gpt-5.1-codex")
        );
    }

    #[test]
    fn service_tier_projects_chunked_long_sse_without_reading_nested_content() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut projector = RequestLogMetadataProjector::default();
        let content = "x".repeat(256 * 1024);
        let event = format!(
            "data: {{\"type\":\"response.completed\",\"response\":{{\"output\":[{{\"text\":\"{content}\",\"service_tier\":\"wrong\"}}],\"service_tier\":\"default\"}}}}\n\n"
        );
        for bytes in event.as_bytes().chunks(13) {
            projector.observe(bytes, &probe, Instant::now()).unwrap();
        }
        assert_eq!(probe.service_tier_for_test().as_deref(), Some("default"));
        projector
            .observe(
                b"data: {\"metadata\":{\"service_tier\":\"wrong\"}}\n\n",
                &probe,
                Instant::now(),
            )
            .unwrap();
        assert_eq!(probe.service_tier_for_test().as_deref(), Some("default"));
        for chunk in b"data: {\"usage\":{\"input_tokens_details\":{\"cache_write_tokens\":1500},\"cache_creation_input_tokens\":999}}\n\n".chunks(3) {
            projector.observe(chunk, &probe, Instant::now()).unwrap();
        }
        assert_eq!(
            probe.token_usage_for_test().cache_creation_input_tokens,
            Some(1500)
        );
        assert!(projector.string.len() <= REQUEST_LOG_METADATA_STRING_BYTES);
    }
}
