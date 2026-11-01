use crate::{App, app::text};
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub native: Vec<Value>,
}
impl Message {
    pub fn new(role: &str, content: String) -> Self {
        Self {
            role: role.into(),
            content,
            tool_calls: vec![],
            tool_call_id: None,
            native: vec![],
        }
    }
}
#[derive(Default, Debug)]
pub struct Generation {
    pub text: String,
    pub tools: Vec<ToolCall>,
    pub usage: Value,
    pub native: Vec<Value>,
}

pub fn endpoint(base: &str, path: &str) -> Result<url::Url> {
    let root = format!("{}/", base.trim_end_matches('/'));
    Ok(url::Url::parse(&root)?.join(path.trim_start_matches('/'))?)
}
async fn request(app: &App, connection: &Value, path: &str) -> Result<reqwest::RequestBuilder> {
    let url = endpoint(text(connection, "baseUrl"), path)?;
    let mut request = app.client.post(url);
    if connection["authKind"] == "openai_oauth" {
        request = request.bearer_auth(crate::auth::access_token(app, connection).await?);
    } else if connection["secretPresent"] == true && connection["authKind"] != "none" {
        let key = crate::auth::get_secret(app, text(connection, "id"))?;
        if connection["protocol"] == "anthropic" {
            request = request.header("x-api-key", key);
        } else if connection["protocol"] == "gemini" {
            request = request.header("x-goog-api-key", key);
        } else {
            request = request.bearer_auth(key);
        }
    }
    if connection["protocol"] == "anthropic" {
        request = request.header("anthropic-version", "2023-06-01");
    }
    Ok(request)
}
pub async fn models(app: &App, connection: &Value) -> Result<Value> {
    let url = endpoint(text(connection, "baseUrl"), "models")?;
    let mut req = app.client.get(url);
    if connection["authKind"] == "openai_oauth" {
        req = req.bearer_auth(crate::auth::access_token(app, connection).await?);
    } else if connection["secretPresent"] == true && connection["authKind"] != "none" {
        let secret = crate::auth::get_secret(app, text(connection, "id"))?;
        req = if connection["protocol"] == "anthropic" {
            req.header("x-api-key", secret)
                .header("anthropic-version", "2023-06-01")
        } else if connection["protocol"] == "gemini" {
            req.header("x-goog-api-key", secret)
        } else {
            req.bearer_auth(secret)
        };
    }
    let response = req
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Model catalog returned {}. You can still enter a model ID manually.",
            response.status()
        )
    }
    let result: Value = response.json().await?;
    let list = result["data"]
        .as_array()
        .or_else(|| result["models"].as_array())
        .context("This catalog format is unsupported; enter the model ID manually")?;
    let mut items: Vec<Value> = list.iter()
        .filter(|m| m["visibility"].is_null() || m["visibility"] == "list")
        .filter_map(|m| {
            let name = m["id"].as_str().or_else(|| m["slug"].as_str()).or_else(|| m["name"].as_str())?;
            Some(json!({"id":name,"name":m["display_name"].as_str().or_else(||m["displayName"].as_str()).unwrap_or(name)}))
        }).collect();
    if connection["preset"] == "lmstudio" {
        enrich_lm_catalog(app, connection, &mut items).await;
    }
    if !text(connection, "id").is_empty() {
        app.store.put("model_catalog", &json!({"id":connection["id"],"baseUrl":connection["baseUrl"],"models":items,"observedAt":crate::app::now()}))?;
    }
    let diagnostics = if connection["preset"] == "lmstudio" {
        vec![
            "Available models may be unloaded. Tool support and effective context depend on the loaded model; verify them before an agent task.",
        ]
    } else {
        vec![]
    };
    Ok(json!({"models":items,"diagnostics":diagnostics}))
}
async fn enrich_lm_catalog(app: &App, connection: &Value, items: &mut Vec<Value>) {
    let result = async {
        let management = endpoint(text(connection, "baseUrl"), "../api/v1/models")?;
        let mut request = app
            .client
            .get(management)
            .timeout(std::time::Duration::from_secs(5));
        if connection["secretPresent"] == true && connection["authKind"] != "none" {
            request = request.bearer_auth(crate::auth::get_secret(app, text(connection, "id"))?);
        }
        let native: Value = request.send().await?.error_for_status()?.json().await?;
        let models = native["models"]
            .as_array()
            .context("No native model catalog")?;
        items.retain_mut(|item| {
            if let Some(model) = models.iter().find(|m| {
                m["key"] == item["id"]
                    || m["loaded_instances"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|i| i["id"] == item["id"]))
            }) {
                if model["type"] != "llm" {
                    return false;
                }
                let instances = model["loaded_instances"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                item["loaded"] = json!(!instances.is_empty());
                item["modelKey"] = model["key"].clone();
                item["capabilitySource"] = json!("server_reported");
                if let Some(context) = instances
                    .iter()
                    .filter_map(|i| i["config"]["context_length"].as_u64())
                    .min()
                {
                    item["contextWindow"] = json!(context);
                }
                item["instanceIds"] = json!(
                    instances
                        .iter()
                        .map(|i| i["id"].clone())
                        .collect::<Vec<_>>()
                );
            }
            true
        });
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let _ = result; // Compatible discovery remains usable when the optional management API is unavailable.
}
pub fn body(
    connection: &Value,
    model: &str,
    system: &str,
    messages: &[Message],
    tools: &[Value],
) -> Result<(String, Value)> {
    let protocol = text(connection, "protocol");
    let mut payload;
    if protocol == "anthropic" {
        let mut list = Vec::new();
        for m in messages {
            if m.role == "tool" {
                list.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":m.tool_call_id,"content":m.content}]}));
            } else {
                let mut blocks = vec![];
                for native in &m.native {
                    if matches!(text(native, "type"), "thinking" | "redacted_thinking") {
                        blocks.push(native.clone());
                    }
                }
                if !m.content.is_empty() {
                    blocks.push(json!({"type":"text","text":m.content}));
                }
                for t in &m.tool_calls {
                    blocks.push(json!({"type":"tool_use","id":t.id,"name":t.name,"input":serde_json::from_str::<Value>(&t.arguments)?}));
                }
                if !blocks.is_empty() {
                    list.push(json!({"role":m.role,"content":blocks}));
                }
            }
        }
        payload =
            json!({"model":model,"system":system,"messages":list,"stream":true,"max_tokens":4096});
        if !tools.is_empty() {
            payload["tools"]=json!(tools.iter().map(|t|json!({"name":t["name"],"description":t["description"],"input_schema":t["parameters"]})).collect::<Vec<_>>());
        }
        return Ok(("messages".into(), payload));
    } else if protocol == "openai_responses" {
        let mut input = Vec::new();
        for m in messages {
            if m.role == "tool" {
                input.push(json!({"type":"function_call_output","call_id":m.tool_call_id,"output":m.content}));
            } else {
                input.extend(
                    m.native
                        .iter()
                        .filter(|v| v["type"] == "reasoning" || v["type"] == "function_call")
                        .cloned(),
                );
                if !m.content.is_empty() {
                    input.push(json!({"role":m.role,"content":m.content}));
                }
                for t in &m.tool_calls {
                    if m.native
                        .iter()
                        .any(|v| v["type"] == "function_call" && v["call_id"] == t.id)
                    {
                        continue;
                    }
                    input.push(json!({"type":"function_call","call_id":t.id,"name":t.name,"arguments":t.arguments}));
                }
            }
        }
        payload = json!({"model":model,"instructions":system,"input":input,"stream":true,"store":false,"include":["reasoning.encrypted_content"]});
        if !tools.is_empty() {
            let definitions:Vec<Value>=tools.iter().map(|t|json!({"type":"function","name":t["name"],"description":t["description"],"parameters":t["parameters"]})).collect();
            payload["tools"] = if connection["authKind"] == "openai_oauth" {
                json!([{"type":"namespace","name":"workbench","description":"Workbench tools","tools":definitions}])
            } else {
                json!(definitions)
            };
        }
        return Ok(("responses".into(), payload));
    } else if protocol == "gemini" {
        let mut contents = Vec::new();
        for m in messages {
            let mut parts = Vec::new();
            if m.role == "tool" {
                parts.push(json!({"functionResponse":{"name":m.tool_call_id.as_deref().unwrap_or("tool"),"response":{"result":m.content}}}));
            } else {
                if !m.content.is_empty() {
                    parts.push(json!({"text":m.content}));
                }
                for t in &m.tool_calls {
                    parts.push(json!({"functionCall":{"name":t.name,"args":serde_json::from_str::<Value>(&t.arguments)?}}));
                }
            }
            contents
                .push(json!({"role":if m.role=="assistant"{"model"}else{"user"},"parts":parts}));
        }
        payload = json!({"systemInstruction":{"parts":[{"text":system}]},"contents":contents});
        if !tools.is_empty() {
            payload["tools"] = json!([{"functionDeclarations":tools}]);
        }
        let safe_id = model.strip_prefix("models/").unwrap_or(model);
        if safe_id.contains(['?', '#']) {
            bail!("Invalid Gemini model identifier")
        };
        return Ok((
            format!("models/{safe_id}:streamGenerateContent?alt=sse"),
            payload,
        ));
    }
    let mut list = vec![json!({"role":"system","content":system})];
    for m in messages {
        let mut item = json!({"role":m.role,"content":m.content});
        if m.role == "tool" {
            item["tool_call_id"] = json!(m.tool_call_id);
        }
        if !m.tool_calls.is_empty() {
            item["tool_calls"]=json!(m.tool_calls.iter().map(|t|json!({"id":t.id,"type":"function","function":{"name":t.name,"arguments":t.arguments}})).collect::<Vec<_>>());
        }
        list.push(item);
    }
    payload = json!({"model":model,"messages":list,"stream":true,"stream_options":{"include_usage":true}});
    if !tools.is_empty() {
        payload["tools"] = json!(
            tools
                .iter()
                .map(|t| json!({"type":"function","function":t}))
                .collect::<Vec<_>>()
        );
    }
    Ok(("chat/completions".into(), payload))
}

#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    data: Vec<String>,
}
impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.buffer.extend(bytes);
        if self.buffer.len() > 4 * 1024 * 1024 {
            bail!("Provider event exceeds the size limit")
        };
        let mut result = vec![];
        while let Some(i) = self.buffer.iter().position(|b| *b == b'\n') {
            let line = self.buffer.drain(..=i).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    result.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.trim_start_matches(' ').to_owned());
            }
        }
        Ok(result)
    }
}
#[allow(clippy::too_many_arguments)] // Explicit provider/context/cancellation boundaries at a single call site.
pub async fn generate(
    app: &App,
    connection: &Value,
    model: &str,
    system: &str,
    messages: &[Message],
    tools: &[Value],
    cancel: &CancellationToken,
    mut on_text: impl FnMut(&str),
) -> Result<Generation> {
    let (path, payload) = body(connection, model, system, messages, tools)?;
    let response = tokio::select! {r=request(app,connection,&path).await?.json(&payload).timeout(std::time::Duration::from_secs(600)).send()=>r?,_=cancel.cancelled()=>bail!("Canceled")};
    if !response.status().is_success() {
        let status = response.status();
        let raw = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                "Check the endpoint, model ID, account permissions and supported API dialect."
                    .into()
            });
        bail!(
            "Provider returned {status}: {}",
            detail.chars().take(1000).collect::<String>()
        );
    }
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::default();
    let mut generation = Generation::default();
    let mut calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
    let mut completed = false;
    let mut native_blocks: BTreeMap<usize, Value> = BTreeMap::new();
    loop {
        let chunk = tokio::select! {v=stream.next()=>v,_=cancel.cancelled()=>bail!("Canceled")};
        let Some(chunk) = chunk else { break };
        for raw in parser.push(&chunk?)? {
            if raw == "[DONE]" {
                completed = true;
                continue;
            }
            let event: Value = serde_json::from_str(&raw)
                .context("Provider sent malformed JSON in its event stream")?;
            if !event["error"].is_null()
                || event["type"] == "response.failed"
                || event["type"] == "error"
            {
                bail!(
                    "Provider stream failed: {}",
                    event["error"]["message"]
                        .as_str()
                        .or_else(|| event["response"]["error"]["message"].as_str())
                        .unwrap_or("Unknown streaming error")
                );
            }
            let mut delta = String::new();
            match text(connection, "protocol") {
                "openai_responses" => match text(&event, "type") {
                    "response.output_text.delta" => delta = text(&event, "delta").into(),
                    "response.output_item.added" => {
                        let item = &event["item"];
                        if item["type"] == "function_call" {
                            let i = event["output_index"].as_u64().unwrap_or(0) as usize;
                            calls.insert(
                                i,
                                ToolCall {
                                    id: text(item, "call_id").into(),
                                    name: text(item, "name").into(),
                                    arguments: String::new(),
                                },
                            );
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let i = event["output_index"].as_u64().unwrap_or(0) as usize;
                        if let Some(call) = calls.get_mut(&i) {
                            call.arguments.push_str(text(&event, "delta"));
                        }
                    }
                    "response.output_item.done" => {
                        let item = &event["item"];
                        if item["type"] == "reasoning" || item["type"] == "function_call" {
                            generation.native.push(item.clone());
                        }
                        if item["type"] == "function_call" {
                            let i = event["output_index"].as_u64().unwrap_or(0) as usize;
                            calls.insert(
                                i,
                                ToolCall {
                                    id: text(item, "call_id").into(),
                                    name: text(item, "name").into(),
                                    arguments: text(item, "arguments").into(),
                                },
                            );
                        }
                    }
                    "response.completed" => {
                        completed = true;
                        generation.usage = event["response"]["usage"].clone();
                    }
                    "response.incomplete" => {
                        bail!("Provider response is incomplete; no further tools were executed")
                    }
                    _ => {}
                },
                "anthropic" => {
                    let i = event["index"].as_u64().unwrap_or(0) as usize;
                    match text(&event, "type") {
                        "content_block_start" => {
                            let block = &event["content_block"];
                            if block["type"] == "tool_use" {
                                calls.insert(
                                    i,
                                    ToolCall {
                                        id: text(block, "id").into(),
                                        name: text(block, "name").into(),
                                        arguments: String::new(),
                                    },
                                );
                            } else if block["type"] == "thinking"
                                || block["type"] == "redacted_thinking"
                            {
                                native_blocks.insert(i, block.clone());
                            }
                        }
                        "content_block_delta" => {
                            let d = &event["delta"];
                            match text(d, "type") {
                                "text_delta" => delta = text(d, "text").into(),
                                "input_json_delta" => {
                                    if let Some(t) = calls.get_mut(&i) {
                                        t.arguments.push_str(text(d, "partial_json"));
                                    }
                                }
                                "thinking_delta" => {
                                    if let Some(b) = native_blocks.get_mut(&i) {
                                        let current = format!(
                                            "{}{}",
                                            text(b, "thinking"),
                                            text(d, "thinking")
                                        );
                                        b["thinking"] = json!(current);
                                    }
                                }
                                "signature_delta" => {
                                    if let Some(b) = native_blocks.get_mut(&i) {
                                        b["signature"] = d["signature"].clone();
                                    }
                                }
                                _ => {}
                            }
                        }
                        "message_delta" => generation.usage = event["usage"].clone(),
                        "message_stop" => completed = true,
                        _ => {}
                    }
                }
                "gemini" => {
                    if let Some(parts) = event["candidates"][0]["content"]["parts"].as_array() {
                        for part in parts {
                            if let Some(t) = part["text"].as_str() {
                                delta.push_str(t);
                            }
                            if part["functionCall"].is_object() {
                                let f = &part["functionCall"];
                                calls.insert(
                                    calls.len(),
                                    ToolCall {
                                        id: text(f, "name").into(),
                                        name: text(f, "name").into(),
                                        arguments: f["args"].to_string(),
                                    },
                                );
                            }
                        }
                    }
                    if !event["candidates"][0]["finishReason"].is_null() {
                        completed = true;
                    }
                    if event["usageMetadata"].is_object() {
                        generation.usage = event["usageMetadata"].clone();
                    }
                }
                _ => {
                    let choice = &event["choices"][0];
                    if let Some(t) = choice["delta"]["content"].as_str() {
                        delta = t.into();
                    }
                    if let Some(items) = choice["delta"]["tool_calls"].as_array() {
                        for item in items {
                            let i = item["index"].as_u64().unwrap_or(0) as usize;
                            let call = calls.entry(i).or_insert(ToolCall {
                                id: String::new(),
                                name: String::new(),
                                arguments: String::new(),
                            });
                            call.id.push_str(text(item, "id"));
                            call.name.push_str(text(&item["function"], "name"));
                            call.arguments
                                .push_str(text(&item["function"], "arguments"));
                        }
                    }
                    if !choice["finish_reason"].is_null() {
                        completed = true;
                    }
                    if event["usage"].is_object() {
                        generation.usage = event["usage"].clone();
                    }
                }
            }
            if !delta.is_empty() {
                generation.text.push_str(&delta);
                on_text(&delta);
                if generation.text.len() > 2 * 1024 * 1024 {
                    bail!("Response exceeded the local output budget")
                }
            }
        }
    }
    if !completed {
        bail!("Provider stream disconnected before a terminal completion event")
    }
    generation.tools = calls.into_values().collect();
    for call in &mut generation.tools {
        if call.arguments.is_empty() {
            call.arguments = "{}".into();
        }
        if call.name.starts_with("workbench.") {
            call.name = call.name.trim_start_matches("workbench.").into();
        }
        if call.id.is_empty() || call.name.is_empty() {
            bail!("Provider returned an incomplete tool call")
        };
        serde_json::from_str::<Value>(&call.arguments)
            .context("Provider returned invalid tool arguments")?;
    }
    generation.native.extend(native_blocks.into_values());
    Ok(generation)
}
pub async fn probe(app: &App, connection: &Value, model: &str) -> Result<Value> {
    let cancel = CancellationToken::new();
    let mut chunks = 0;
    let reply = generate(
        app,
        connection,
        model,
        "Reply briefly.",
        &[Message::new(
            "user",
            "Say Workbench connection verified.".into(),
        )],
        &[],
        &cancel,
        |_| chunks += 1,
    )
    .await?;
    let echo = json!({"name":"connection_echo","description":"Echo supplied test text without external effects.","parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}});
    let mut history = vec![Message::new(
        "user",
        "Use the connection_echo tool with text verified.".into(),
    )];
    let tool_result = generate(app, connection, model, "Test structured tools: call connection_echo exactly once, then acknowledge its returned result.", &history, std::slice::from_ref(&echo), &cancel, |_| {}).await;
    let mut diagnostics = Vec::new();
    let mut tools = false;
    match tool_result {
        Ok(g) if g.tools.len() == 1 && g.tools[0].name == "connection_echo" => {
            let call = &g.tools[0];
            let args: Value = serde_json::from_str(&call.arguments)?;
            if args["text"].is_string() {
                let mut assistant = Message::new("assistant", g.text);
                assistant.tool_calls = g.tools.clone();
                assistant.native = g.native;
                history.push(assistant);
                let mut result = Message::new("tool", json!({"echo":args["text"]}).to_string());
                result.tool_call_id = Some(call.id.clone());
                history.push(result);
                match generate(app, connection, model, "The test tool has returned. Acknowledge its result briefly without making another tool call.", &history, std::slice::from_ref(&echo), &cancel, |_| {}).await {
                    Ok(final_result) => tools = final_result.tools.is_empty() && !final_result.text.is_empty(),
                    Err(error) => diagnostics.push(format!("Tool-result continuation failed: {error}")),
                }
            }
        }
        Ok(_) => diagnostics.push("Model did not return the expected structured test call. Tool execution remains disabled until verified or explicitly configured.".into()),
        Err(error) => diagnostics.push(error.to_string()),
    }
    Ok(
        json!({"text":!reply.text.is_empty(),"streaming":chunks>0,"tools":tools,"reply":reply.text,"diagnostics":diagnostics}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_keeps_split_unicode_and_crlf() {
        let mut p = SseParser::default();
        let bytes = "data: {\"text\":\"ă\"}\r\n\r\n".as_bytes();
        let split = bytes.iter().position(|b| *b == 0xc4).unwrap() + 1;
        assert!(p.push(&bytes[..split]).unwrap().is_empty());
        assert_eq!(p.push(&bytes[split..]).unwrap(), vec!["{\"text\":\"ă\"}"]);
    }
    #[test]
    fn url_preserves_proxy_prefix() {
        assert_eq!(
            endpoint("http://127.0.0.1:1234/prefix/v1", "models")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:1234/prefix/v1/models"
        );
    }
    #[test]
    fn oauth_does_not_send_rejected_fields() {
        let (_, b) = body(
            &json!({"protocol":"openai_responses","authKind":"openai_oauth"}),
            "model",
            "instructions",
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(b["store"], false);
        assert_eq!(b["stream"], true);
        assert!(b.get("temperature").is_none());
        assert!(b.get("previous_response_id").is_none());
    }
}
