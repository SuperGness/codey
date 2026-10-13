use super::*;

tokio::task_local! {
    pub(crate) static COLLABORATION_ALIAS: Option<CollaborationAlias>;
}

const RESERVED_NAMESPACE: &str = "collaboration";
const ALIAS_NAMESPACE: &str = "agents";

/// 上游为部分模型保留 `collaboration` 命名空间：提交的 schema 必须与模型配置逐字一致，
/// 而其中 `message` 参数带加密标记，任务正文只有官方上游能解开。转发前把它改到普通
/// 命名空间，加密标记随后照常摘除，正文以明文往返；回包再还原成 Codex 认识的
/// `collaboration` 调用并标注参数未加密，Codex 据此按明文投递，任意线路的子代理都能读取。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CollaborationAlias {
    names: HashSet<String>,
}

pub(crate) fn current_collaboration_alias() -> Option<CollaborationAlias> {
    COLLABORATION_ALIAS.try_with(Clone::clone).ok().flatten()
}

/// 已有同名 `agents` 工具时不改名，保持保留 schema 原样以免上游拒绝。
pub(crate) fn alias_reserved_collaboration_namespace(
    body: &mut Value,
) -> Option<CollaborationAlias> {
    let tools = body.get_mut("tools")?.as_array_mut()?;
    if tools
        .iter()
        .any(|tool| tool_name(tool) == Some(ALIAS_NAMESPACE))
    {
        return None;
    }
    let namespace = tools.iter_mut().find(|tool| {
        tool.get("type").and_then(Value::as_str) == Some("namespace")
            && tool_name(tool) == Some(RESERVED_NAMESPACE)
    })?;
    let names: HashSet<String> = namespace
        .get("tools")
        .or_else(|| namespace.get("children"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool_name(tool).map(str::to_string))
        .collect();
    if names.is_empty() {
        return None;
    }
    namespace["name"] = Value::String(ALIAS_NAMESPACE.to_string());
    let alias = CollaborationAlias { names };
    alias.alias_history(body);
    if let Some(choice) = body.get_mut("tool_choice").and_then(Value::as_object_mut) {
        if choice.get("type").and_then(Value::as_str) == Some("namespace")
            && choice.get("name").and_then(Value::as_str) == Some(RESERVED_NAMESPACE)
        {
            choice.insert("name".to_string(), Value::String(ALIAS_NAMESPACE.into()));
        } else {
            alias.rename(choice, RESERVED_NAMESPACE, ALIAS_NAMESPACE);
        }
    }
    Some(alias)
}

fn tool_name(tool: &Value) -> Option<&str> {
    tool.get("name").and_then(Value::as_str)
}

impl CollaborationAlias {
    /// 历史调用改到别名命名空间。可重复执行，原生历史恢复后再调用一次即可。
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
        // 参数仍是官方密文的旧调用只有原命名空间能被上游解读，保持不动。
        let sealed = object
            .get("encrypted_function_args")
            .and_then(Value::as_array)
            .is_some_and(|args| !args.is_empty());
        !sealed
            && object.get("type").and_then(Value::as_str) == Some("function_call")
            && self.rename(object, RESERVED_NAMESPACE, ALIAS_NAMESPACE)
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
                    && self.rename(object, ALIAS_NAMESPACE, RESERVED_NAMESPACE)
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
    fn existing_alias_namespace_keeps_reserved_schema_untouched() {
        let mut body = reserved_body();
        body["tools"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"namespace","name":"agents","tools":[]}));
        assert!(alias_reserved_collaboration_namespace(&mut body).is_none());
        strip_collaboration_message_encryption(&mut body, true);
        assert_eq!(body["tools"][1]["name"], "collaboration");
        assert_eq!(
            body["tools"][1]["tools"][0]["parameters"]["properties"]["message"]["encrypted"],
            true
        );
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
}
