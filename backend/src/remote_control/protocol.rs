//! Desktop follower protocol, adapted from codex-mobile-bridge (MIT).
//! See third_party/codex-mobile-bridge.LICENSE.
use serde_json::{Value, json};

pub(super) fn apply_patches(state: &mut Value, patches: &Value) -> Result<(), String> {
    let patches = patches.as_array().ok_or("桌面增量格式无效")?;
    if patches.len() > 10_000 {
        return Err("桌面增量过大".into());
    }
    for patch in patches {
        let path: Vec<String> = match &patch["path"] {
            Value::String(path) if path.is_empty() || path.starts_with('/') => path
                .split('/')
                .skip(1)
                .map(|s| s.replace("~1", "/").replace("~0", "~"))
                .collect(),
            Value::Array(path) => path
                .iter()
                .map(|part| match part {
                    Value::String(s) => Ok(s.clone()),
                    Value::Number(n) if n.is_u64() => Ok(n.to_string()),
                    _ => Err("桌面增量路径无效".to_string()),
                })
                .collect::<Result<_, _>>()?,
            _ => return Err("桌面增量路径无效".into()),
        };
        let op = patch["op"].as_str().ok_or("桌面增量操作无效")?;
        if !matches!(op, "add" | "replace" | "remove") {
            return Err("不支持的桌面增量操作".into());
        }
        if op != "remove" && patch.get("value").is_none() {
            return Err("桌面增量缺少内容".into());
        }
        if path.is_empty() {
            if op != "replace" {
                return Err("不支持的桌面根增量".into());
            }
            *state = patch["value"].clone();
            continue;
        }
        let mut target = &mut *state;
        for key in &path[..path.len() - 1] {
            target = match target {
                Value::Array(items) => {
                    items.get_mut(key.parse::<usize>().map_err(|_| "增量下标无效")?)
                }
                Value::Object(map) => map.get_mut(key),
                _ => None,
            }
            .ok_or("桌面增量路径不存在")?;
        }
        let key = path.last().unwrap();
        match target {
            Value::Array(items) => {
                let index = if key == "-" && op == "add" {
                    items.len()
                } else {
                    key.parse().map_err(|_| "增量下标无效")?
                };
                if index > items.len() || (index == items.len() && op != "add") {
                    return Err("增量下标越界".into());
                }
                match op {
                    "add" => items.insert(index, patch["value"].clone()),
                    "remove" => {
                        items.remove(index);
                    }
                    _ => items[index] = patch["value"].clone(),
                }
            }
            Value::Object(map) => {
                if op != "add" && !map.contains_key(key) {
                    return Err("桌面增量字段不存在".into());
                }
                if op == "remove" {
                    map.remove(key);
                } else {
                    map.insert(key.clone(), patch["value"].clone());
                }
            }
            _ => return Err("桌面增量目标无效".into()),
        }
    }
    Ok(())
}

pub(super) fn ordered_items(value: &Value) -> Vec<&Value> {
    if let Some(items) = value.as_array() {
        return items.iter().collect();
    }
    if let Some(entities) = value["entitiesByKey"].as_object() {
        return value["islands"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|island| island["entries"].as_array().into_iter().flatten())
            .filter_map(|entry| entry["value"].as_str().and_then(|key| entities.get(key)))
            .collect();
    }
    value["items"].as_array().into_iter().flatten().collect()
}

pub(super) fn turns(state: &Value) -> Vec<&Value> {
    if state["turnHistory"]["kind"] == "canonical" {
        ordered_items(&state["turnHistory"]["history"])
    } else {
        ordered_items(&state["turns"])
    }
}

fn text_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn item_view(item: &Value) -> Value {
    let kind = item["type"].as_str().unwrap_or("unknown");
    let (role, text) = match kind {
        "userMessage" => ("user", text_content(&item["content"])),
        "steeringUserMessage" => ("user", text_content(&item["input"])),
        "agentMessage" | "assistantMessage" => ("assistant", text_content(&item["text"])),
        "commandExecution" => (
            "activity",
            format!(
                "{}\n{}",
                text_content(&item["command"]),
                text_content(&item["aggregatedOutput"])
            ),
        ),
        "fileChange" => (
            "activity",
            item["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| format!("{}\n{}", text_content(&c["path"]), text_content(&c["diff"])))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        "reasoning" => (
            "activity",
            item["summary"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        "planImplementation" => ("assistant", text_content(&item["planContent"])),
        "error" => ("error", text_content(&item["message"])),
        _ => (
            "activity",
            serde_json::to_string_pretty(item).unwrap_or_default(),
        ),
    };
    json!({"id": item["id"], "kind": kind, "role": role, "text": text, "status": item["status"]})
}

pub(super) fn request_supported(request: &Value) -> bool {
    let encoded = request["params"].to_string();
    !encoded.contains("openai/userVerification")
        && matches!(
            request["method"].as_str(),
            Some(
                "item/commandExecution/requestApproval"
                    | "item/fileChange/requestApproval"
                    | "item/permissions/requestApproval"
                    | "item/tool/requestUserInput"
                    | "tool/requestUserInput"
                    | "item/plan/requestImplementation"
            )
        )
}

fn question_replies(text: &str) -> Vec<Value> {
    text.trim()
        .strip_prefix("<send_user_message_question_reply>")
        .and_then(|s| s.strip_suffix("</send_user_message_question_reply>"))
        .and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok())
        .unwrap_or_default()
}

pub(super) fn async_requests(state: &Value) -> Vec<Value> {
    let turns = turns(state);
    let Some(turn) = turns
        .last()
        .filter(|t| t["status"] == "inProgress" || t["status"] == "completed")
    else {
        return Vec::new();
    };
    let items = ordered_items(&turn["items"]);
    let answered: std::collections::HashSet<String> = items
        .iter()
        .filter(|item| {
            item["type"] == "userMessage"
                || (item["type"] == "steeringUserMessage" && item["status"] == "accepted")
        })
        .flat_map(|item| {
            question_replies(&text_content(item.get("content").unwrap_or(&item["input"])))
        })
        .filter_map(|reply| reply["questionItemId"].as_str().map(str::to_string))
        .collect();
    items.into_iter().filter(|item|item["type"] == "agentMessage").filter_map(|item| {
        let id = item["id"].as_str()?;
        let questions: Vec<_> = item["questions"].as_array()?.iter().enumerate().filter_map(|(index,question)| {
            let key = json!(["request_user_input_async",id,index]).to_string();
            if answered.contains(&key) { return None; }
            Some(json!({"id":key,"question":question["title"],"options":question["options"].as_array().into_iter().flatten().map(|option|json!({"label":option})).collect::<Vec<_>>()}))
        }).collect();
        (!questions.is_empty()).then(||json!({"id":format!("async:{}:{id}",turn["turnId"].as_str().unwrap_or_default()),"method":"codey/requestUserInputAsync","supported":true,"params":{"questions":questions}}))
    }).collect()
}

pub(super) fn view(state: &Value) -> Value {
    let turns = turns(state).into_iter().map(|turn| {
        let mut messages: Vec<Value> = ordered_items(&turn["items"]).into_iter().map(item_view).collect();
        if !messages.iter().any(|item| item["role"] == "user") {
            let opening = text_content(&turn["params"]["input"]);
            if !opening.is_empty() { messages.insert(0, json!({"role":"user", "kind":"userMessage", "text":opening})); }
        }
        json!({"id":turn["turnId"], "status":turn["status"], "messages":messages, "error":turn["error"]})
    }).collect::<Vec<_>>();
    let mut requests: Vec<Value> = state["requests"].as_array().into_iter().flatten().map(|request| {
        let supported = request_supported(request);
        json!({"id": request["id"], "method": request["method"], "supported":supported,
            "params": if supported { request["params"].clone() } else { json!({"message":"请在电脑端处理此类请求"}) }})
    }).collect();
    requests.extend(async_requests(state));
    json!({"id":state["id"], "title":state["title"], "cwd":state["cwd"], "model":state["latestModel"],
        "effort":state["latestReasoningEffort"].as_str().or(state["latestThreadSettings"]["effort"].as_str()),
        "status":state["threadRuntimeStatus"]["type"], "turns":turns, "requests":requests,
        "historyComplete":state["turnHistory"]["history"]["isComplete"].as_bool().or(state["turnsPagination"]["hasLoadedOldest"].as_bool()).unwrap_or(true)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_support_native_paths_and_json_pointers() {
        let mut state = json!({"items":["a"], "a/b":{"~key":1}});
        apply_patches(
            &mut state,
            &json!([
                {"op":"add","path":["items",1],"value":"b"},
                {"op":"replace","path":"/a~1b/~0key","value":2},
                {"op":"remove","path":["items",0]}
            ]),
        )
        .unwrap();
        assert_eq!(state, json!({"items":["b"], "a/b":{"~key":2}}));
        assert!(apply_patches(&mut state, &json!([{"op":"remove","path":"/items/9"}])).is_err());
        assert!(apply_patches(&mut state, &json!([{"op":"copy","path":[]}])).is_err());
    }

    #[test]
    fn canonical_history_retains_order_and_hides_verification_challenges() {
        let state = json!({"id":"t", "turnHistory":{"kind":"canonical","history":{
            "entitiesByKey":{"b":{"turnId":"two","items":[]},"a":{"turnId":"one","items":[{"type":"agentMessage","text":"完成"}]}},
            "islands":[{"entries":[{"value":"a"},{"value":"b"}]}]}},
            "requests":[{"id":1,"method":"item/tool/requestUserInput","params":{"openai/userVerification":"private-challenge"}}]});
        let view = view(&state);
        assert_eq!(view["turns"][0]["id"], "one");
        assert_eq!(view["turns"][0]["messages"][0]["text"], "完成");
        assert!(!view.to_string().contains("private-challenge"));
        assert_eq!(view["requests"][0]["supported"], false);
    }

    #[test]
    fn async_questions_disappear_only_after_accepted_answers() {
        let key = json!(["request_user_input_async", "question", 0]).to_string();
        let text = format!(
            "<send_user_message_question_reply>{}</send_user_message_question_reply>",
            json!([{"questionItemId":key,"answer":"yes"}])
        );
        let mut state = json!({"turns":[{"turnId":"turn","status":"inProgress","items":[
            {"id":"question","type":"agentMessage","questions":[{"title":"继续？","options":["是"]}]},
            {"type":"steeringUserMessage","status":"pending","input":[{"text":text}]}
        ]}]});
        assert_eq!(async_requests(&state).len(), 1);
        state["turns"][0]["items"][1]["status"] = json!("accepted");
        assert!(async_requests(&state).is_empty());
        state["turns"][0]["status"] = json!("failed");
        assert!(async_requests(&state).is_empty());
    }
}
