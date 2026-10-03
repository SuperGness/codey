// OpenAI Responses SSE event writer (port of proxy/lib/gemini-stream.js
// createResponsesWriter). Emits spec-shaped events with stable item ids.
use serde_json::{json, Value};

pub struct Writer {
    response_id: String,
    model: String,
    created: i64,
    pending: Vec<String>,
    items: Vec<Value>,
    item_count: usize,
    output_index: usize,
    current: Option<CurrentItem>,
    pub saw_item: Option<Box<dyn FnMut(&Value) + Send>>,
}

struct CurrentItem {
    kind: &'static str, // message | reasoning | function_call
    id: String,
    output_index: usize,
    text: String,
    args: String,
    name: String,
    call_id: String,
    thought_signature: Option<String>,
}

fn event(v: Value) -> String {
    format!("data: {}\n\n", v)
}

impl Writer {
    pub fn new(response_id: String, model: String, created: i64) -> Self {
        Writer {
            response_id,
            model,
            created,
            pending: Vec::new(),
            items: Vec::new(),
            item_count: 0,
            output_index: 0,
            current: None,
            saw_item: None,
        }
    }

    fn next_item_id(&mut self) -> String {
        let id = format!("item_proxy_{}_{}", self.response_id, self.item_count);
        self.item_count += 1;
        id
    }

    fn close_current(&mut self) {
        let Some(c) = self.current.take() else { return };
        match c.kind {
            "message" => {
                self.pending.push(event(json!({
                    "type": "response.content_part.done",
                    "response_id": self.response_id,
                    "item_id": c.id,
                    "output_index": c.output_index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": c.text, "annotations": []}
                })));
                let content = if c.text.is_empty() {
                    json!([])
                } else {
                    json!([{"type": "output_text", "text": c.text, "annotations": []}])
                };
                let item = json!({
                    "id": c.id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": content
                });
                self.pending.push(event(json!({
                    "type": "response.output_item.done",
                    "response_id": self.response_id,
                    "output_index": c.output_index,
                    "item": item
                })));
                if let Some(cb) = self.saw_item.as_mut() {
                    cb(&item);
                }
                self.items.push(item);
            }
            "reasoning" => {
                self.pending.push(event(json!({
                    "type": "response.reasoning_summary_text.done",
                    "response_id": self.response_id,
                    "item_id": c.id,
                    "output_index": c.output_index,
                    "content_index": 0,
                    "text": c.text
                })));
                let summary = if c.text.is_empty() {
                    json!([])
                } else {
                    json!([{"type": "summary_text", "text": c.text}])
                };
                let item = json!({
                    "id": c.id,
                    "type": "reasoning",
                    "summary": summary
                });
                self.pending.push(event(json!({
                    "type": "response.output_item.done",
                    "response_id": self.response_id,
                    "output_index": c.output_index,
                    "item": item
                })));
                self.items.push(item);
            }
            "function_call" => {
                self.pending.push(event(json!({
                    "type": "response.function_call_arguments.done",
                    "response_id": self.response_id,
                    "item_id": c.id,
                    "output_index": c.output_index,
                    "arguments": c.args
                })));
                let mut item = json!({
                    "id": c.id,
                    "type": "function_call",
                    "status": "completed",
                    "call_id": c.call_id,
                    "name": c.name,
                    "arguments": c.args
                });
                if let Some(sig) = c.thought_signature {
                    item["thought_signature"] = json!(sig);
                }
                self.pending.push(event(json!({
                    "type": "response.output_item.done",
                    "response_id": self.response_id,
                    "output_index": c.output_index,
                    "item": item
                })));
                if let Some(cb) = self.saw_item.as_mut() {
                    cb(&item);
                }
                self.items.push(item);
            }
            _ => {}
        }
    }

    fn open(&mut self, kind: &'static str) -> &mut CurrentItem {
        self.close_current();
        let id = self.next_item_id();
        let output_index = self.output_index;
        self.output_index += 1;
        self.current = Some(CurrentItem {
            kind,
            id,
            output_index,
            text: String::new(),
            args: String::new(),
            name: String::new(),
            call_id: String::new(),
            thought_signature: None,
        });
        self.current.as_mut().unwrap()
    }

    pub fn start(&mut self) {
        self.pending.push(event(json!({
            "type": "response.created",
            "response": {
                "id": self.response_id,
                "object": "response",
                "created_at": self.created,
                "status": "in_progress",
                "model": self.model,
                "output": [],
                "error": null,
                "incomplete_details": null
            }
        })));
    }

    pub fn text_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self
            .current
            .as_ref()
            .map(|c| c.kind != "message")
            .unwrap_or(true)
        {
            let c = self.open("message");
            let (id, oi) = (c.id.clone(), c.output_index);
            self.pending.push(event(json!({
                "type": "response.output_item.added",
                "response_id": self.response_id,
                "output_index": oi,
                "item": {"id": id, "type": "message", "status": "in_progress", "role": "assistant", "content": []}
            })));
            self.pending.push(event(json!({
                "type": "response.content_part.added",
                "response_id": self.response_id,
                "item_id": id,
                "output_index": oi,
                "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []}
            })));
        }
        let c = self.current.as_mut().unwrap();
        c.text.push_str(delta);
        let (id, oi) = (c.id.clone(), c.output_index);
        let delta = delta.to_string();
        self.pending.push(event(json!({
            "type": "response.output_text.delta",
            "response_id": self.response_id,
            "item_id": id,
            "output_index": oi,
            "content_index": 0,
            "delta": delta
        })));
    }

    pub fn reasoning_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self
            .current
            .as_ref()
            .map(|c| c.kind != "reasoning")
            .unwrap_or(true)
        {
            let c = self.open("reasoning");
            let (id, oi) = (c.id.clone(), c.output_index);
            self.pending.push(event(json!({
                "type": "response.output_item.added",
                "response_id": self.response_id,
                "output_index": oi,
                "item": {"id": id, "type": "reasoning", "status": "in_progress", "summary": []}
            })));
        }
        let c = self.current.as_mut().unwrap();
        c.text.push_str(delta);
        let (id, oi) = (c.id.clone(), c.output_index);
        let delta = delta.to_string();
        self.pending.push(event(json!({
            "type": "response.reasoning_summary_text.delta",
            "response_id": self.response_id,
            "item_id": id,
            "output_index": oi,
            "content_index": 0,
            "delta": delta
        })));
    }

    pub fn tool_call(
        &mut self,
        name: &str,
        args: &Value,
        call_id: &str,
        thought_signature: Option<&str>,
    ) {
        let c = self.open("function_call");
        c.name = name.to_string();
        c.call_id = call_id.to_string();
        c.args = match args {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        c.thought_signature = thought_signature.map(String::from);
        let (id, oi) = (c.id.clone(), c.output_index);
        let args_str = c.args.clone();
        let mut added = json!({
            "type": "response.output_item.added",
            "response_id": self.response_id,
            "output_index": oi,
            "item": {
                "id": id, "type": "function_call", "status": "in_progress",
                "call_id": call_id, "name": name, "arguments": ""
            }
        });
        if let Some(sig) = thought_signature {
            added["item"]["thought_signature"] = json!(sig);
        }
        self.pending.push(event(added));
        self.pending.push(event(json!({
            "type": "response.function_call_arguments.delta",
            "response_id": self.response_id,
            "item_id": id,
            "output_index": oi,
            "delta": args_str
        })));
    }

    /// Native web search surfaced from candidate.groundingMetadata.
    pub fn web_search(&mut self, queries: &[String]) {
        if queries.is_empty() {
            return;
        }
        self.close_current();
        let id = format!("ws_{}_{}", self.response_id, self.item_count);
        self.item_count += 1;
        let index = self.output_index;
        self.output_index += 1;
        let action = json!({"type": "search", "query": queries[0]});
        self.pending.push(event(json!({
            "type": "response.output_item.added",
            "response_id": self.response_id,
            "output_index": index,
            "item": {"id": id, "type": "web_search_call", "status": "in_progress", "action": action}
        })));
        self.pending.push(event(json!({
            "type": "response.web_search_call.searching",
            "response_id": self.response_id,
            "item_id": id,
            "output_index": index
        })));
        self.pending.push(event(json!({
            "type": "response.web_search_call.completed",
            "response_id": self.response_id,
            "item_id": id,
            "output_index": index
        })));
        let item = json!({
            "id": id, "type": "web_search_call", "status": "completed",
            "action": action, "results": null
        });
        self.pending.push(event(json!({
            "type": "response.output_item.done",
            "response_id": self.response_id,
            "output_index": index,
            "item": item
        })));
        self.items.push(item);
    }

    pub fn seal(&mut self) {
        self.close_current();
    }

    pub fn complete(&mut self, usage: Value, raw_finish: Option<String>) {
        self.close_current();
        self.pending.push(event(json!({
            "type": "response.completed",
            "response": {
                "id": self.response_id,
                "object": "response",
                "created_at": self.created,
                "status": "completed",
                "model": self.model,
                "output": self.items,
                "usage": usage,
                "error": null,
                "incomplete_details": null,
                "raw_finish_reason": raw_finish
            }
        })));
    }

    /// Push all pending SSE chunks into the sink.
    pub fn flush(&mut self, sink: &mut dyn FnMut(&str)) {
        if self.pending.is_empty() {
            return;
        }
        let chunk = self.pending.join("");
        self.pending.clear();
        sink(&chunk);
    }
}
