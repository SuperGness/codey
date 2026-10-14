use super::*;

tokio::task_local! {
    pub(crate) static COLLABORATION_ALIAS: Option<CollaborationAlias>;
}

const RESERVED_NAMESPACE: &str = "collaboration";
const ALIAS_NAMESPACE: &str = "agents";
const FALLBACK_ALIAS_NAMESPACE: &str = "codey_agents";
const MAX_FALLBACK_ALIAS_INDEX: u32 = 64;

/// 上游为部分模型保留 `collaboration` 命名空间：提交的 schema 必须与模型配置逐字一致，
/// 而其中 `message` 参数带加密标记，任务正文只有官方上游能解开。转发前把它改到普通
/// 命名空间，加密标记随后照常摘除，正文以明文往返；回包再还原成 Codex 认识的
/// `collaboration` 调用并标注参数未加密，Codex 据此按明文投递，任意线路的子代理都能读取。
/// `agents` 已被占用时依次退到 `codey_agents`、`codey_agents_N`，保留命名空间不会原样
/// 进入仍按保留名校验的上游。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CollaborationAlias {
    to: String,
    names: HashSet<String>,
}

pub(crate) fn current_collaboration_alias() -> Option<CollaborationAlias> {
    COLLABORATION_ALIAS.try_with(Clone::clone).ok().flatten()
}

/// 工具名可能直接写在 `name`，也可能嵌在 Chat 形状的 `function.name` 里。
fn tool_display_name(tool: &Value) -> Option<&str> {
    tool.get("name").and_then(Value::as_str).or_else(|| {
        tool.get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
    })
}

fn is_collaboration_namespace_entry(tool: &Value) -> bool {
    tool.get("name").and_then(Value::as_str) == Some(RESERVED_NAMESPACE)
        && tool
            .get("type")
            .and_then(Value::as_str)
            .is_none_or(|tool_type| tool_type == "namespace")
}

fn namespace_children_names(namespace: &Value) -> impl Iterator<Item = &str> {
    ["tools", "children"]
        .iter()
        .filter_map(|field| namespace.get(field).and_then(Value::as_array))
        .flatten()
        .filter_map(tool_display_name)
}

/// 顶层 tools 之外，input 里的附加工具表（如 additional_tools/tool_search_output）
/// 也会携带同一命名空间，改名与占用检查要覆盖所有这些作用域。
fn tool_lists(body: &Value) -> Vec<&Vec<Value>> {
    let mut lists = Vec::new();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        lists.push(tools);
    }
    let items: &[_] = match body.get("input") {
        Some(Value::Array(items)) => items.as_slice(),
        Some(item @ Value::Object(_)) => std::slice::from_ref(item),
        _ => &[],
    };
    for item in items {
        if let Some(tools) = item.get("tools").and_then(Value::as_array) {
            lists.push(tools);
        }
    }
    lists
}

fn tool_lists_mut(body: &mut Value) -> Vec<&mut Vec<Value>> {
    let mut lists = Vec::new();
    let Some(object) = body.as_object_mut() else {
        return lists;
    };
    for (key, value) in object.iter_mut() {
        match key.as_str() {
            "tools" => {
                if let Some(tools) = value.as_array_mut() {
                    lists.push(tools);
                }
            }
            "input" => match value {
                Value::Array(items) => {
                    for item in items {
                        if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                            lists.push(tools);
                        }
                    }
                }
                item @ Value::Object(_) => {
                    if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                        lists.push(tools);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    lists
}

fn free_alias_namespace(taken: &HashSet<String>) -> Option<String> {
    [ALIAS_NAMESPACE, FALLBACK_ALIAS_NAMESPACE]
        .iter()
        .map(|candidate| candidate.to_string())
        .chain(
            (2..=MAX_FALLBACK_ALIAS_INDEX)
                .map(|index| format!("{FALLBACK_ALIAS_NAMESPACE}_{index}")),
        )
        .find(|candidate| !taken.contains(candidate))
}

/// 把请求中的保留命名空间改到空闲的普通命名空间。找不到可改的对象时返回 None，
/// 此时上游收到的仍是原名，schema 必须保持原样。
pub(crate) fn alias_reserved_collaboration_namespace(
    body: &mut Value,
) -> Option<CollaborationAlias> {
    let mut found = false;
    let mut names = HashSet::new();
    let mut taken = HashSet::new();
    for tools in tool_lists(body) {
        for tool in tools {
            if let Some(name) = tool_display_name(tool) {
                taken.insert(name.to_string());
            }
            if is_collaboration_namespace_entry(tool) {
                found = true;
                names.extend(namespace_children_names(tool).map(str::to_string));
            } else if tool.get("namespace").and_then(Value::as_str) == Some(RESERVED_NAMESPACE) {
                found = true;
                if let Some(name) = tool_display_name(tool) {
                    names.insert(name.to_string());
                }
            }
        }
    }
    if !found {
        return None;
    }
    let target = free_alias_namespace(&taken)?;
    for tools in tool_lists_mut(body) {
        for tool in tools.iter_mut() {
            if is_collaboration_namespace_entry(tool) {
                tool["name"] = Value::String(target.clone());
            } else if tool.get("namespace").and_then(Value::as_str) == Some(RESERVED_NAMESPACE) {
                tool["namespace"] = Value::String(target.clone());
            }
        }
    }
    let alias = CollaborationAlias { to: target, names };
    alias.alias_history(body);
    if let Some(choice) = body.get_mut("tool_choice").and_then(Value::as_object_mut) {
        if choice.get("type").and_then(Value::as_str) == Some("namespace")
            && choice.get("name").and_then(Value::as_str) == Some(RESERVED_NAMESPACE)
        {
            choice.insert("name".to_string(), Value::String(alias.to.clone()));
        } else {
            alias.rename(choice, RESERVED_NAMESPACE, &alias.to);
        }
    }
    Some(alias)
}

/// 仍叫 `collaboration` 的命名空间会按保留名进入上游校验；报告请求里是否还存在它，
/// 供调用方决定保留原 schema。
pub(crate) fn body_has_reserved_collaboration_namespace(body: &Value) -> bool {
    tool_lists(body).into_iter().flatten().any(|tool| {
        is_collaboration_namespace_entry(tool)
            || tool.get("namespace").and_then(Value::as_str) == Some(RESERVED_NAMESPACE)
    })
}

impl CollaborationAlias {
    /// 历史调用与历史里内嵌的工具声明都改到别名命名空间。可重复执行，原生历史恢复后
    /// 再调用一次即可。
    pub(crate) fn alias_history(&self, body: &mut Value) -> bool {
        match body.get_mut("input") {
            Some(Value::Array(items)) => items.iter_mut().fold(false, |changed, item| {
                changed | self.alias_history_call(item)
            }),
            Some(item) => self.alias_history_call(item),
            None => false,
        }
    }

    fn alias_history_call(&self, item: &mut Value) -> bool {
        let Some(object) = item.as_object_mut() else {
            return false;
        };
        let mut changed = false;
        if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools.iter_mut() {
                if is_collaboration_namespace_entry(tool) {
                    tool["name"] = Value::String(self.to.clone());
                    changed = true;
                } else if tool.get("namespace").and_then(Value::as_str) == Some(RESERVED_NAMESPACE)
                {
                    tool["namespace"] = Value::String(self.to.clone());
                    changed = true;
                }
            }
        }
        // 参数仍是官方密文的旧调用只有原命名空间能被上游解读，保持不动。
        let sealed = object
            .get("encrypted_function_args")
            .and_then(Value::as_array)
            .is_some_and(|args| !args.is_empty());
        let renamed = !sealed
            && object.get("type").and_then(Value::as_str) == Some("function_call")
            && self.rename(object, RESERVED_NAMESPACE, &self.to);
        changed | renamed
    }

    /// 回包中的别名调用还原为 `collaboration`，并补上空的 `encrypted_function_args`，
    /// 让 Codex 按明文协作消息投递。
    pub(crate) fn apply(&self, value: &mut Value) -> bool {
        match value {
            Value::Array(items) => items
                .iter_mut()
                .fold(false, |changed, item| changed | self.apply(item)),
            Value::Object(object) => {
                let mut changed = false;
                if object.get("type").and_then(Value::as_str) == Some("function_call")
                    && self.rename(object, &self.to, RESERVED_NAMESPACE)
                {
                    object
                        .entry("encrypted_function_args")
                        .or_insert_with(|| json!([]));
                    changed = true;
                }
                for child in object.values_mut() {
                    changed |= self.apply(child);
                }
                changed
            }
            _ => false,
        }
    }

    fn rename(&self, object: &mut serde_json::Map<String, Value>, from: &str, to: &str) -> bool {
        if object.get("namespace").and_then(Value::as_str) != Some(from)
            || !object
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| self.names.contains(name))
        {
            return false;
        }
        object.insert("namespace".to_string(), Value::String(to.to_string()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reserved_body() -> Value {
        json!({
            "tools": [
                {"type":"function","name":"exec_command","parameters":{"type":"object"}},
                {
                    "type": "namespace",
                    "name": "collaboration",
                    "tools": [
                        {
                            "type": "function",
                            "name": "followup_task",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "target": {"type": "string"},
                                    "message": {"type": "string", "encrypted": true}
                                }
                            }
                        },
                        {"type": "function", "name": "wait_agent", "parameters": {"type": "object"}}
                    ]
                }
            ],
            "input": [
                {"type":"function_call","namespace":"collaboration","name":"followup_task",
                 "call_id":"plain","arguments":"{\"message\":\"hi\"}","encrypted_function_args":[]},
                {"type":"function_call","namespace":"collaboration","name":"followup_task",
                 "call_id":"sealed","arguments":"{\"message\":\"gAAA\"}",
                 "encrypted_function_args":["message"]},
                {"type":"function_call","name":"exec_command","call_id":"other","arguments":"{}"}
            ]
        })
    }

    #[test]
    fn reserved_namespace_moves_to_alias_and_encryption_marker_is_stripped() {
        let mut body = reserved_body();
        let alias = alias_reserved_collaboration_namespace(&mut body).unwrap();
        assert!(strip_collaboration_message_encryption(&mut body, true));

        let namespace = &body["tools"][1];
        assert_eq!(namespace["name"], "agents");
        assert!(
            namespace["tools"][0]["parameters"]["properties"]["message"]
                .get("encrypted")
                .is_none()
        );
        assert_eq!(body["input"][0]["namespace"], "agents");
        // 仍是官方密文的旧调用保持原命名空间。
        assert_eq!(body["input"][1]["namespace"], "collaboration");
        assert!(body["input"][2].get("namespace").is_none());
        assert!(!alias.alias_history(&mut body));
    }

    #[test]
    fn taken_alias_namespace_falls_back_instead_of_keeping_reserved_name() {
        let mut body = reserved_body();
        body["tools"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"namespace","name":"agents","tools":[]}));

        let alias = alias_reserved_collaboration_namespace(&mut body).unwrap();
        let namespace = &body["tools"][1];
        assert_eq!(namespace["name"], FALLBACK_ALIAS_NAMESPACE);
        assert_eq!(alias.to, FALLBACK_ALIAS_NAMESPACE);
        assert!(alias.names.contains("followup_task"));
        assert!(alias.names.contains("wait_agent"));
        // 已存在的 agents 命名空间保持不动。
        assert_eq!(body["tools"][2]["name"], "agents");
        assert_eq!(body["input"][0]["namespace"], FALLBACK_ALIAS_NAMESPACE);

        // 改名后按明文摘除标记，回包从备用名还原。
        assert!(strip_collaboration_message_encryption(&mut body, false));
        let mut event = json!({
            "type": "response.output_item.done",
            "item": {"type":"function_call","namespace":FALLBACK_ALIAS_NAMESPACE,
                     "name":"followup_task","call_id":"c1","arguments":"{}"}
        });
        assert!(alias.apply(&mut event));
        assert_eq!(event["item"]["namespace"], "collaboration");
    }

    #[test]
    fn nested_function_names_and_flat_namespace_fields_are_collected() {
        let mut body = json!({
            "tools": [
                {
                    "type": "namespace",
                    "name": "collaboration",
                    "tools": [
                        {"type":"function","function":{"name":"followup_task",
                            "parameters":{"type":"object","properties":{
                                "message":{"type":"string","encrypted":true}}}}}
                    ]
                },
                {"type":"function","name":"send_message","namespace":"collaboration"}
            ]
        });

        let alias = alias_reserved_collaboration_namespace(&mut body).unwrap();
        assert_eq!(body["tools"][0]["name"], "agents");
        assert_eq!(body["tools"][1]["namespace"], "agents");
        assert!(alias.names.contains("followup_task"));
        assert!(alias.names.contains("send_message"));
        assert!(strip_collaboration_message_encryption(&mut body, false));
        assert!(
            body["tools"][0]["tools"][0]["function"]["parameters"]["properties"]["message"]
                .get("encrypted")
                .is_none()
        );
    }

    #[test]
    fn tools_inside_input_items_are_renamed_and_restored() {
        let mut body = json!({
            "tools": [{"type":"function","name":"exec_command","parameters":{"type":"object"}}],
            "input": [{
                "type": "tool_search_output",
                "call_id": "ts-1",
                "tools": [{
                    "type": "namespace",
                    "name": "collaboration",
                    "tools": [{"type":"function","name":"followup_task"}]
                }]
            }]
        });

        let alias = alias_reserved_collaboration_namespace(&mut body).unwrap();
        assert_eq!(body["input"][0]["tools"][0]["name"], "agents");
        assert!(alias.names.contains("followup_task"));

        // 恢复的历史里再次出现保留命名空间时，alias_history 仍能处理。
        body["input"].as_array_mut().unwrap().push(
            json!({"type":"tool_search_output","call_id":"ts-2","tools":[
                {"type":"namespace","name":"collaboration","tools":[
                    {"type":"function","name":"wait_agent"}]}]}),
        );
        assert!(alias.alias_history(&mut body));
        assert_eq!(body["input"][1]["tools"][0]["name"], "agents");
    }

    #[test]
    fn response_calls_restore_reserved_namespace_with_plaintext_marker() {
        let mut body = reserved_body();
        let alias = alias_reserved_collaboration_namespace(&mut body).unwrap();
        let mut event = json!({
            "type": "response.output_item.done",
            "item": {"type":"function_call","namespace":"agents","name":"followup_task",
                     "call_id":"c1","arguments":"{\"target\":\"a\",\"message\":\"do it\"}"}
        });
        assert!(alias.apply(&mut event));
        assert_eq!(event["item"]["namespace"], "collaboration");
        assert_eq!(event["item"]["encrypted_function_args"], json!([]));
        assert!(!alias.apply(&mut event));

        let mut completed = json!({"type":"response.completed","response":{"output":[
            {"type":"function_call","namespace":"agents","name":"wait_agent","call_id":"c2","arguments":"{}"},
            {"type":"function_call","namespace":"agents","name":"unknown","call_id":"c3","arguments":"{}"},
            {"type":"function_call","name":"exec_command","call_id":"c4","arguments":"{}"}
        ]}});
        assert!(alias.apply(&mut completed));
        let output = &completed["response"]["output"];
        assert_eq!(output[0]["namespace"], "collaboration");
        assert_eq!(output[1]["namespace"], "agents");
        assert!(output[2].get("namespace").is_none());
    }

    #[test]
    fn tool_choice_follows_alias() {
        let mut body = reserved_body();
        body["tool_choice"] =
            json!({"type":"function","namespace":"collaboration","name":"wait_agent"});
        alias_reserved_collaboration_namespace(&mut body).unwrap();
        assert_eq!(body["tool_choice"]["namespace"], "agents");
    }

    #[test]
    fn tool_choice_namespace_entry_follows_fallback_alias() {
        let mut body = reserved_body();
        body["tools"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"namespace","name":"agents","tools":[]}));
        body["tool_choice"] = json!({"type":"namespace","name":"collaboration"});
        alias_reserved_collaboration_namespace(&mut body).unwrap();
        assert_eq!(body["tool_choice"]["name"], FALLBACK_ALIAS_NAMESPACE);
    }

    #[test]
    fn reports_remaining_reserved_namespace() {
        let mut body = reserved_body();
        assert!(body_has_reserved_collaboration_namespace(&body));
        alias_reserved_collaboration_namespace(&mut body).unwrap();
        assert!(!body_has_reserved_collaboration_namespace(&body));
    }
}
