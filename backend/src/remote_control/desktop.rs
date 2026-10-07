//! Length-prefixed Codex desktop IPC; execution always stays with the desktop owner.
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use super::protocol;

const MAX_FRAME: usize = 16 * 1024 * 1024;
pub(super) const UNCERTAIN: &str = "桌面响应中断或超时，操作可能已提交；请先查看会话，不要重复发送";

trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

pub(super) struct Desktop {
    stream: Box<dyn Transport>,
    buffer: Vec<u8>,
    outgoing: Vec<u8>,
    written: usize,
    client: Option<String>,
    owner: Option<String>,
    pub thread: String,
    pub state: Value,
    revision: Option<Value>,
}

impl Desktop {
    async fn connect() -> Result<Self, String> {
        #[cfg(windows)]
        let stream = {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            loop {
                match tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(r"\\.\pipe\codex-ipc")
                {
                    Ok(stream) => break stream,
                    Err(error)
                        if error.raw_os_error() == Some(231)
                            && tokio::time::Instant::now() < deadline =>
                    {
                        tokio::time::sleep(Duration::from_millis(50)).await
                    }
                    Err(_) => {
                        return Err("无法连接 Codex 桌面，请确认 Codey 和 Codex 正在运行".into());
                    }
                }
            }
        };
        #[cfg(unix)]
        let stream =
            tokio::net::UnixStream::connect(crate::codex_config::codex_home().join("ipc/ipc.sock"))
                .await
                .map_err(|_| "无法连接 Codex 桌面，请确认 Codey 和 Codex 正在运行")?;
        Self::initialize(Box::new(stream)).await
    }

    async fn initialize(stream: Box<dyn Transport>) -> Result<Self, String> {
        let mut desktop = Self {
            stream,
            buffer: Vec::new(),
            outgoing: Vec::new(),
            written: 0,
            client: None,
            owner: None,
            thread: String::new(),
            state: Value::Null,
            revision: None,
        };
        let result = desktop
            .request(
                "initialize",
                0,
                json!({"clientType":"codey-remote-control"}),
                false,
            )
            .await?;
        desktop.client = Some(
            result["result"]["clientId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("Codex 桌面初始化响应不兼容")?
                .to_string(),
        );
        Ok(desktop)
    }

    pub async fn follow(thread: &str) -> Result<Self, String> {
        validate_thread(thread)?;
        Self::subscribe(Self::connect().await?, thread).await
    }

    async fn subscribe(mut desktop: Self, thread: &str) -> Result<Self, String> {
        desktop.thread = thread.to_string();
        desktop
            .send(&json!({"type":"broadcast", "sourceClientId":desktop.client,
            "method":"thread-stream-following-changed", "version":1,
            "params":{"hostId":"local", "conversationId":thread, "following":true}}))
            .await?;
        tokio::time::timeout(Duration::from_secs(8), async {
            while !desktop.update().await? {}
            Ok::<_, String>(())
        })
        .await
        .map_err(|_| "尚未取得桌面会话，请点击重新连接以在电脑中打开会话")??;
        Ok(desktop)
    }

    async fn send(&mut self, message: &Value) -> Result<(), String> {
        let body = serde_json::to_vec(message).map_err(|_| "桌面消息编码失败")?;
        if body.len() > MAX_FRAME {
            return Err("桌面消息过大".into());
        }
        self.outgoing
            .extend_from_slice(&(body.len() as u32).to_le_bytes());
        self.outgoing.extend_from_slice(&body);
        tokio::time::timeout(Duration::from_secs(5), self.flush_outgoing())
            .await
            .map_err(|_| UNCERTAIN)?
    }

    // Persist the write cursor too: a heartbeat may cancel discovery replies.
    async fn flush_outgoing(&mut self) -> Result<(), String> {
        while self.written < self.outgoing.len() {
            let count = self
                .stream
                .write(&self.outgoing[self.written..])
                .await
                .map_err(|_| UNCERTAIN)?;
            if count == 0 {
                return Err(UNCERTAIN.into());
            }
            self.written += count;
        }
        self.outgoing.clear();
        self.written = 0;
        self.stream.flush().await.map_err(|_| UNCERTAIN.into())
    }

    // The buffer belongs to the connection so cancellation for stream heartbeats
    // never loses a partially received frame or its length prefix.
    async fn next(&mut self) -> Result<Value, String> {
        self.flush_outgoing().await?;
        loop {
            if self.buffer.len() >= 4 {
                let length = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
                if length == 0 || length > MAX_FRAME {
                    return Err("Codex 桌面 IPC 帧长度不兼容".into());
                }
                if self.buffer.len() >= length + 4 {
                    let value: Value = serde_json::from_slice(&self.buffer[4..length + 4])
                        .map_err(|_| "Codex 桌面 IPC 数据无效")?;
                    self.buffer.drain(..length + 4);
                    if value["type"] == "client-discovery-request" {
                        self.send(&json!({"type":"client-discovery-response", "requestId":value["requestId"], "response":{"canHandle":false}})).await?;
                        continue;
                    }
                    return Ok(value);
                }
            }
            let mut chunk = [0u8; 8192];
            let length = self
                .stream
                .read(&mut chunk)
                .await
                .map_err(|_| "Codex 桌面连接已断开")?;
            if length == 0 {
                return Err("Codex 桌面连接已断开".into());
            }
            self.buffer.extend_from_slice(&chunk[..length]);
        }
    }

    async fn request(
        &mut self,
        method: &str,
        version: u8,
        params: Value,
        mutation: bool,
    ) -> Result<Value, String> {
        let id = Uuid::new_v4().to_string();
        let mut message = json!({"type":"request", "requestId":id, "sourceClientId":self.client,
            "method":method, "version":version, "params":params, "timeoutMs":30_000});
        if let Some(owner) = &self.owner {
            message["targetClientId"] = json!(owner);
        }
        self.send(&message).await?;
        let result = tokio::time::timeout(Duration::from_secs(32), async {
            loop {
                let response = self.next().await?;
                if response["type"] == "response" && response["requestId"] == id {
                    if response["resultType"] != "success" {
                        return Err("Codex 桌面未接受操作，请刷新会话检查状态或在电脑端处理".into());
                    }
                    return Ok(response);
                }
                self.apply_event(response)?;
            }
        })
        .await;
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) if !mutation => Err(error),
            Err(_) if !mutation => Err("Codex 桌面读取超时".into()),
            _ => Err(UNCERTAIN.into()),
        }
    }

    pub async fn update(&mut self) -> Result<bool, String> {
        let event = self.next().await?;
        self.apply_event(event)
    }

    fn apply_event(&mut self, event: Value) -> Result<bool, String> {
        if event["type"] != "broadcast" {
            return Ok(false);
        }
        if event["method"] == "client-status-changed"
            && event["params"]["status"] == "disconnected"
            && self.owner.as_deref() == event["params"]["clientId"].as_str()
        {
            return Err("会话所属的 Codex 窗口已断开，请重新连接".into());
        }
        if event["method"] != "thread-stream-state-changed"
            || event["params"]["conversationId"] != self.thread
            || event["params"]["hostId"] != "local"
        {
            return Ok(false);
        }
        if let Some(targets) = event["targetClientIds"].as_array()
            && !targets
                .iter()
                .any(|id| id.as_str() == self.client.as_deref())
        {
            return Ok(false);
        }
        if event["version"] != 11 {
            return Err("当前 Codex 桌面会话协议不兼容，请更新 Codey".into());
        }
        let change = &event["params"]["change"];
        let source = event["sourceClientId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("桌面会话缺少所属客户端")?;
        if self.owner.as_deref().is_some_and(|owner| owner != source) {
            return Ok(false);
        }
        match change["type"].as_str() {
            Some("snapshot") => {
                if change["conversationState"]["id"] != self.thread {
                    return Err("桌面快照会话不匹配".into());
                }
                self.state = change["conversationState"].clone();
                self.owner = Some(source.to_string());
            }
            Some("patches") if self.owner.is_some() => {
                if self.revision.as_ref() != change.get("baseRevision") || self.revision.is_none() {
                    return Err("会话增量已失步，请重新连接获取完整快照".into());
                }
                protocol::apply_patches(&mut self.state, &change["patches"])?;
                if self.state["id"] != self.thread {
                    return Err("桌面增量会话不匹配".into());
                }
            }
            _ => return Ok(false),
        }
        self.revision = Some(
            change
                .get("revision")
                .filter(|v| !v.is_null())
                .ok_or("桌面快照缺少版本")?
                .clone(),
        );
        Ok(true)
    }

    pub async fn action(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        let (method, version, mut params) = action_params(&self.state, action, args)?;
        params["conversationId"] = json!(self.thread);
        let result = self
            .request(method, version, params, action != "history")
            .await?;
        Ok(result["result"].clone())
    }
}

pub(super) fn validate_thread(thread: &str) -> Result<(), String> {
    Uuid::parse_str(thread)
        .map(|_| ())
        .map_err(|_| "会话标识无效".into())
}

pub(super) fn action_params(
    state: &Value,
    action: &str,
    args: &Value,
) -> Result<(&'static str, u8, Value), String> {
    let active = protocol::turns(state)
        .into_iter()
        .rev()
        .find(|turn| turn["status"] == "inProgress");
    match action {
        "send" | "steer" => {
            let text = args["text"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 100_000)
                .ok_or("请输入有效消息，最多 100000 字节")?;
            let id = args["requestId"].as_str().ok_or("消息缺少请求标识")?;
            validate_thread(id)?;
            if action == "send" && active.is_some() {
                return Err("任务正在执行，请使用补充指令或先停止任务".into());
            }
            if action == "steer" && active.is_none() {
                return Err("当前没有可补充指令的任务，请直接发送".into());
            }
            let request = json!({"threadId":state["id"], "clientUserMessageId":id,
                "input":[{"type":"text", "text":text, "text_elements":[]}]});
            let context =
                json!({"inheritThreadSettings":true, "attachments":[], "commentAttachments":[]});
            if action == "send" {
                Ok((
                    "thread-follower-start-turn",
                    2,
                    json!({"turnStart":{"request":request,"context":context}}),
                ))
            } else {
                Ok((
                    "thread-follower-steer-turn",
                    1,
                    json!({"input":request["input"],"clientUserMessageId":id,"restoreMessage":{"request":request,"context":context},"attachments":[]}),
                ))
            }
        }
        "interrupt" => {
            let turn = active
                .and_then(|t| t["turnId"].as_str())
                .ok_or("当前没有可停止的任务")?;
            if args["turnId"].as_str() != Some(turn) {
                return Err("执行轮次已变化，请刷新后再停止".into());
            }
            Ok((
                "thread-follower-interrupt-turn",
                4,
                json!({"mode":"user-stop", "expectedTurnId":turn}),
            ))
        }
        "settings" => {
            let mut settings = json!({});
            if let Some(model) = args.get("model") {
                let model = model
                    .as_str()
                    .filter(|s| !s.trim().is_empty() && s.len() <= 512)
                    .ok_or("模型名称无效")?;
                settings["model"] = json!(model);
            }
            if let Some(effort) = args.get("effort") {
                let effort = effort
                    .as_str()
                    .filter(|s| {
                        matches!(
                            *s,
                            "none"
                                | "minimal"
                                | "low"
                                | "medium"
                                | "high"
                                | "xhigh"
                                | "max"
                                | "ultra"
                        )
                    })
                    .ok_or("思考强度无效")?;
                settings["effort"] = json!(effort);
            }
            if let Some(mode) = args.get("permissionMode") {
                // Same built-in permission profiles as the desktop composer.
                // Apply only an explicit selection, leaving custom policies alone.
                let (profile, approval, sandbox) = match mode.as_str() {
                    Some("read-only") => (
                        ":read-only",
                        "on-request",
                        json!({"type":"readOnly","networkAccess":false}),
                    ),
                    Some("auto") => {
                        let cwd = state["cwd"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or("尚未取得会话工作目录")?;
                        (
                            ":workspace",
                            "on-request",
                            json!({"type":"workspaceWrite","writableRoots":[cwd],"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false}),
                        )
                    }
                    Some("full-access") => (
                        ":danger-full-access",
                        "never",
                        json!({"type":"dangerFullAccess"}),
                    ),
                    _ => return Err("权限模式无效".into()),
                };
                settings["approvalPolicy"] = json!(approval);
                settings["approvalsReviewer"] = json!("user");
                settings["sandboxPolicy"] = sandbox;
                settings["permissions"] = json!(profile);
                settings["activePermissionProfile"] = json!({"id":profile,"extends":null});
            }
            if settings.as_object().is_none_or(|value| value.is_empty()) {
                return Err("请选择要修改的会话设置".into());
            }
            Ok((
                "thread-follower-update-thread-settings",
                2,
                json!({"threadSettings":settings}),
            ))
        }
        "history" => Ok(("thread-follower-load-complete-history", 1, json!({}))),
        "respond" => {
            if let Some(request) = protocol::async_requests(state)
                .into_iter()
                .find(|r| r["id"] == args["approvalId"])
            {
                let answers = args["answers"].as_object().ok_or("请填写问题答案")?;
                let questions = request["params"]["questions"]
                    .as_array()
                    .ok_or("问题格式无效")?;
                if answers.len() != questions.len() {
                    return Err("请回答所有问题".into());
                }
                let mut replies = Vec::new();
                for question in questions {
                    let id = question["id"].as_str().ok_or("问题标识无效")?;
                    let answer = answers
                        .get(id)
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty() && s.len() <= 20_000)
                        .ok_or("答案为空或过长")?;
                    replies.push(json!({"questionItemId":id,"question":question["question"],"answer":answer}));
                }
                let text = format!(
                    "<send_user_message_question_reply>\n{}\n</send_user_message_question_reply>",
                    json!(replies)
                );
                return action_params(
                    state,
                    if active.is_some() { "steer" } else { "send" },
                    &json!({"text":text,"requestId":args["requestId"]}),
                );
            }
            let request = state["requests"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| r["id"] == args["approvalId"] && !r["id"].is_null())
                .ok_or("该请求已处理或已过期，请刷新会话")?;
            if !protocol::request_supported(request) {
                return Err("此类请求请在电脑端处理".into());
            }
            if request["method"] == "item/plan/requestImplementation" {
                let (text, mode) = match args["decision"].as_str() {
                    Some("implement") => (
                        format!(
                            "Implement the following plan:\n\n{}",
                            request["params"]["planContent"]
                                .as_str()
                                .filter(|s| !s.trim().is_empty())
                                .ok_or("计划内容尚未加载")?
                        ),
                        "default",
                    ),
                    Some("revise") => (
                        args["text"]
                            .as_str()
                            .filter(|s| !s.trim().is_empty())
                            .ok_or("请填写计划修改意见")?
                            .to_string(),
                        "plan",
                    ),
                    _ => return Err("请选择执行计划或修改计划".into()),
                };
                let model = state["latestModel"]
                    .as_str()
                    .or(state["latestCollaborationMode"]["settings"]["model"].as_str())
                    .filter(|s| !s.is_empty())
                    .ok_or("尚未取得桌面模型设置，请重新连接")?;
                let (method, version, mut payload) = action_params(
                    state,
                    "send",
                    &json!({"text":text,"requestId":args["requestId"]}),
                )?;
                payload["turnStart"]["request"]["collaborationMode"] = json!({"mode":mode,"settings":{"model":model,"reasoning_effort":state["latestReasoningEffort"].as_str().or(state["latestThreadSettings"]["effort"].as_str()),"developer_instructions":null}});
                return Ok((method, version, payload));
            }
            let mut payload = json!({"requestId":request["id"]});
            let method = match request["method"].as_str().unwrap_or_default() {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    let decision = args["decision"]
                        .as_str()
                        .filter(|s| matches!(*s, "accept" | "decline" | "cancel"))
                        .ok_or("审批决定无效")?;
                    if let Some(available) = request["params"]["availableDecisions"].as_array()
                        && !available.iter().any(|v| v == decision)
                    {
                        return Err("当前审批不支持该决定".into());
                    }
                    payload["decision"] = json!(decision);
                    if request["method"] == "item/fileChange/requestApproval" {
                        "thread-follower-file-approval-decision"
                    } else {
                        "thread-follower-command-approval-decision"
                    }
                }
                "item/permissions/requestApproval" => {
                    let decision = args["decision"]
                        .as_str()
                        .filter(|s| matches!(*s, "accept" | "decline"))
                        .ok_or("审批决定无效")?;
                    payload["response"] = json!({"permissions":if decision == "accept" { request["params"]["permissions"].clone() } else { json!({}) }, "scope":"turn"});
                    "thread-follower-permissions-request-approval-response"
                }
                _ => {
                    let questions = request["params"]["questions"]
                        .as_array()
                        .ok_or("请求问题无效")?;
                    let answers = args["answers"].as_object().ok_or("请填写问题答案")?;
                    if questions.is_empty() || questions.len() != answers.len() {
                        return Err("请回答所有问题".into());
                    }
                    let mut response = serde_json::Map::new();
                    for question in questions {
                        let id = question["id"].as_str().ok_or("问题标识无效")?;
                        let text = answers
                            .get(id)
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty() && s.len() <= 20_000)
                            .ok_or("问题答案为空或过长")?;
                        response.insert(id.into(), json!({"answers":[text]}));
                    }
                    payload["response"] = json!({"answers":response});
                    "thread-follower-submit-user-input"
                }
            };
            Ok((method, 1, payload))
        }
        _ => Err("不支持的远程会话操作".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn framed_transport_handles_fragments_and_discovery() {
        let (client, mut server) = tokio::io::duplex(1024);
        let fixture = tokio::spawn(async move {
            let length = server.read_u32_le().await.unwrap();
            let mut bytes = vec![0; length as usize];
            server.read_exact(&mut bytes).await.unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["method"], "initialize");
            let reply = json!({"type":"response","requestId":request["requestId"],"resultType":"success","result":{"clientId":"mobile"}}).to_string().into_bytes();
            server
                .write_all(&(reply.len() as u32).to_le_bytes())
                .await
                .unwrap();
            for part in reply.chunks(3) {
                server.write_all(part).await.unwrap();
            }
        });
        let desktop = Desktop::initialize(Box::new(client)).await.unwrap();
        assert_eq!(desktop.client.as_deref(), Some("mobile"));
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_frame_and_stale_revisions_are_rejected() {
        let (client, mut server) = tokio::io::duplex(64);
        let mut desktop = Desktop {
            stream: Box::new(client),
            buffer: Vec::new(),
            outgoing: Vec::new(),
            written: 0,
            client: Some("mobile".into()),
            owner: None,
            thread: "t".into(),
            state: Value::Null,
            revision: None,
        };
        server.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
        assert!(desktop.next().await.unwrap_err().contains("帧长度"));
        let event = json!({"type":"broadcast","method":"thread-stream-state-changed","version":11,"sourceClientId":"owner","params":{"hostId":"local","conversationId":"t","change":{"type":"snapshot","revision":1,"conversationState":{"id":"t"}}}});
        assert!(desktop.apply_event(event).unwrap());
        let patch = json!({"type":"broadcast","method":"thread-stream-state-changed","version":11,"sourceClientId":"owner","params":{"hostId":"local","conversationId":"t","change":{"type":"patches","revision":3,"baseRevision":2,"patches":[]}}});
        assert!(desktop.apply_event(patch).unwrap_err().contains("失步"));
    }

    #[test]
    fn settings_are_partial_and_permissions_use_native_profiles() {
        let state = json!({"id":"t","cwd":"E:/code/codey","latestThreadSettings":{"approvalPolicy":"custom","model":"keep"}});
        let (_, version, model) = action_params(
            &state,
            "settings",
            &json!({"model":"route/model","effort":"high"}),
        )
        .unwrap();
        assert_eq!(version, 2);
        assert_eq!(
            model,
            json!({"threadSettings":{"model":"route/model","effort":"high"}})
        );
        let (_, _, effort) = action_params(&state, "settings", &json!({"effort":"low"})).unwrap();
        assert_eq!(effort, json!({"threadSettings":{"effort":"low"}}));
        for (mode, profile, sandbox, approval) in [
            ("read-only", ":read-only", "readOnly", "on-request"),
            ("auto", ":workspace", "workspaceWrite", "on-request"),
            (
                "full-access",
                ":danger-full-access",
                "dangerFullAccess",
                "never",
            ),
        ] {
            let (method, _, payload) =
                action_params(&state, "settings", &json!({"permissionMode":mode})).unwrap();
            let settings = &payload["threadSettings"];
            assert_eq!(method, "thread-follower-update-thread-settings");
            assert_eq!(settings["permissions"], profile);
            assert_eq!(settings["activePermissionProfile"]["id"], profile);
            assert_eq!(settings["approvalPolicy"], approval);
            assert_eq!(settings["sandboxPolicy"]["type"], sandbox);
            assert!(settings.get("model").is_none());
            assert!(payload.get("activeTurnId").is_none());
            if mode == "auto" {
                assert_eq!(
                    settings["sandboxPolicy"]["writableRoots"],
                    json!(["E:/code/codey"])
                );
            }
        }
        for args in [
            json!({}),
            json!({"model":""}),
            json!({"effort":"invalid"}),
            json!({"permissionMode":"custom"}),
            json!({"model":null}),
            json!({"permissionMode":true}),
        ] {
            assert!(action_params(&state, "settings", &args).is_err());
        }
        assert!(action_params(&json!({}), "settings", &json!({"permissionMode":"auto"})).is_err());
    }

    #[test]
    fn actions_keep_desktop_settings_and_validate_live_turns_and_approvals() {
        let state = json!({"id":"t","turns":[],"requests":[{"id":7,"method":"item/commandExecution/requestApproval","params":{"availableDecisions":["decline"]}}]});
        let (_, version, payload) = action_params(
            &state,
            "send",
            &json!({"text":"执行测试","requestId":Uuid::new_v4().to_string()}),
        )
        .unwrap();
        assert_eq!(version, 2);
        assert_eq!(
            payload["turnStart"]["context"]["inheritThreadSettings"],
            true
        );
        assert!(
            payload["turnStart"]["request"]
                .get("approvalPolicy")
                .is_none()
        );
        assert!(action_params(&state, "interrupt", &json!({"turnId":"stale"})).is_err());
        assert!(
            action_params(
                &state,
                "respond",
                &json!({"approvalId":7,"decision":"accept"})
            )
            .is_err()
        );
        assert!(
            action_params(
                &state,
                "respond",
                &json!({"approvalId":7,"decision":"decline"})
            )
            .is_ok()
        );
        assert!(
            action_params(
                &state,
                "respond",
                &json!({"approvalId":8,"decision":"decline"})
            )
            .is_err()
        );
    }

    async fn read_frame(stream: &mut tokio::io::DuplexStream) -> Value {
        let size = stream.read_u32_le().await.unwrap() as usize;
        let mut bytes = vec![0; size];
        stream.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn write_frame(stream: &mut tokio::io::DuplexStream, value: Value) {
        let bytes = value.to_string().into_bytes();
        stream.write_u32_le(bytes.len() as u32).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
    }

    #[tokio::test]
    async fn follows_desktop_owner_and_sends_to_original_workspace() {
        let (client, mut server) = tokio::io::duplex(8192);
        let thread = Uuid::new_v4().to_string();
        let thread_copy = thread.clone();
        let fixture = tokio::spawn(async move {
            let initialize = read_frame(&mut server).await;
            write_frame(&mut server,json!({"type":"response","requestId":initialize["requestId"],"resultType":"success","result":{"clientId":"mobile"}})).await;
            let following = read_frame(&mut server).await;
            assert_eq!(following["params"]["conversationId"], thread_copy);
            assert_eq!(following["params"]["following"], true);
            write_frame(&mut server,json!({"type":"broadcast","method":"thread-stream-state-changed","version":11,"sourceClientId":"desktop-owner","params":{"hostId":"local","conversationId":thread_copy,"change":{"type":"snapshot","revision":1,"conversationState":{"id":thread_copy,"turns":[]}}}})).await;
            write_frame(
                &mut server,
                json!({"type":"client-discovery-request","requestId":"discovery"}),
            )
            .await;
            let action = read_frame(&mut server).await;
            assert_eq!(action["targetClientId"], "desktop-owner");
            assert_eq!(action["method"], "thread-follower-start-turn");
            assert_eq!(action["params"]["conversationId"], thread_copy);
            assert_eq!(
                action["params"]["turnStart"]["context"]["inheritThreadSettings"],
                true
            );
            let discovery = read_frame(&mut server).await;
            assert_eq!(discovery["response"]["canHandle"], false);
            write_frame(&mut server,json!({"type":"response","requestId":action["requestId"],"resultType":"success","result":{"status":"accepted"}})).await;
        });
        let mut desktop = Desktop::subscribe(
            Desktop::initialize(Box::new(client)).await.unwrap(),
            &thread,
        )
        .await
        .unwrap();
        assert_eq!(
            desktop
                .action(
                    "send",
                    &json!({"text":"test","requestId":Uuid::new_v4().to_string()})
                )
                .await
                .unwrap()["status"],
            "accepted"
        );
        fixture.await.unwrap();
    }

    #[test]
    fn answers_async_questions_and_plan_using_current_desktop_state() {
        let state = json!({"id":"t","latestModel":"test-model","turns":[{"turnId":"turn","status":"inProgress","items":[{"id":"q","type":"agentMessage","questions":[{"title":"如何连接？","options":["外网"]}]}]}]});
        let request = protocol::async_requests(&state).remove(0);
        let question = request["params"]["questions"][0]["id"].as_str().unwrap();
        let (method,_,payload) = action_params(&state,"respond",&json!({"approvalId":request["id"],"requestId":Uuid::new_v4().to_string(),"answers":{question:"外网"}})).unwrap();
        assert_eq!(method, "thread-follower-steer-turn");
        assert!(
            payload["input"][0]["text"]
                .as_str()
                .unwrap()
                .contains("send_user_message_question_reply")
        );
        let state = json!({"id":"t","latestModel":"test-model","requests":[{"id":5,"method":"item/plan/requestImplementation","params":{"planContent":"运行测试"}}]});
        let (_, _, payload) = action_params(
            &state,
            "respond",
            &json!({"approvalId":5,"requestId":Uuid::new_v4().to_string(),"decision":"implement"}),
        )
        .unwrap();
        assert_eq!(
            payload["turnStart"]["request"]["collaborationMode"]["mode"],
            "default"
        );
        assert_eq!(
            payload["turnStart"]["request"]["collaborationMode"]["settings"]["model"],
            "test-model"
        );
    }

    #[tokio::test]
    #[ignore = "requires a running Codex desktop; reads only, sends no task or approval"]
    async fn live_desktop_read_only_smoke() {
        let threads =
            super::super::store::threads(crate::codex_config::codex_home(), "", false).unwrap();
        let thread = threads[0]["id"]
            .as_str()
            .expect("at least one local thread");
        let desktop = Desktop::follow(thread).await.unwrap();
        let view = protocol::view(&desktop.state);
        assert_eq!(view["id"], thread);
        println!(
            "Live desktop handshake + owner snapshot OK; turns={}",
            view["turns"].as_array().unwrap().len()
        );
    }
}
