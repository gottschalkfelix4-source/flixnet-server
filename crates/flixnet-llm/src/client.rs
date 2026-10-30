//! Minimal client for OpenAI-compatible `POST {base}/chat/completions` with tools.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

/// Connection settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmConfig {
    /// API root including the version, e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A function call requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON text as the model wrote it (some servers send an object; it is kept as
    /// text so malformed output can be told apart from valid output).
    #[serde(default, deserialize_with = "string_or_json")]
    pub arguments: String,
}

fn string_or_json<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// One chat message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_as_empty"
    )]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

fn null_as_empty<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ToolCall>, D::Error> {
    Ok(Option::<Vec<ToolCall>>::deserialize(d)?.unwrap_or_default())
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }

    fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(text.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// A function the model may call, with a JSON-schema for its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolSpec {
    fn to_json(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    /// The model must answer in text (tools are not sent at all).
    None,
    /// The model must call this function.
    Function(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: ToolChoice,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            temperature: 0.2,
            max_tokens: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM endpoint unreachable: {0}")]
    Transport(String),
    #[error("LLM request timed out")]
    Timeout,
    #[error("LLM endpoint answered {status}: {message}")]
    Http { status: u16, message: String },
    #[error("LLM answer not understood: {0}")]
    Malformed(String),
}

/// rustls with the `ring` provider (as everywhere else in Flixnet).
fn http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // Err only means another provider was installed first, which is fine.
            let _ = rustls::crypto::ring::default_provider().install_default();
            reqwest::Client::builder()
                .user_agent(concat!("Flixnet/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("HTTP client configuration is valid")
        })
        .clone()
}

/// Client for one configured endpoint. Cheap to clone.
#[derive(Debug, Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    config: Arc<LlmConfig>,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            http: http(),
            config: Arc::new(config),
        }
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// The request body as sent to the endpoint.
    pub fn body(&self, request: &ChatRequest) -> Value {
        let mut body = json!({
            "model": self.config.model,
            "messages": request.messages,
            "temperature": request.temperature,
            "stream": false,
        });
        if let Some(max) = request.max_tokens {
            body["max_tokens"] = json!(max);
        }
        if !request.tools.is_empty() && request.tool_choice != ToolChoice::None {
            body["tools"] = Value::Array(request.tools.iter().map(ToolSpec::to_json).collect());
            body["tool_choice"] = match &request.tool_choice {
                ToolChoice::Function(name) => {
                    json!({ "type": "function", "function": { "name": name } })
                }
                _ => json!("auto"),
            };
        }
        body
    }

    /// One completion; returns the assistant message.
    pub async fn chat(&self, request: &ChatRequest) -> Result<Message, LlmError> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut builder = self
            .http
            .post(&url)
            .timeout(self.config.timeout)
            .json(&self.body(request));
        if let Some(key) = self.config.api_key.as_deref().filter(|k| !k.is_empty()) {
            builder = builder.bearer_auth(key);
        }
        let response = builder.send().await.map_err(|err| {
            if err.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Transport(err.to_string())
            }
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|err| {
            if err.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Transport(err.to_string())
            }
        })?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v["error"]["message"]
                        .as_str()
                        .or(v["error"].as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(LlmError::Http {
                status: status.as_u16(),
                message,
            });
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|err| LlmError::Malformed(format!("no JSON: {err}")))?;
        let message = value["choices"][0]["message"].clone();
        if message.is_null() {
            return Err(LlmError::Malformed("no choices".to_owned()));
        }
        serde_json::from_value(message).map_err(|err| LlmError::Malformed(err.to_string()))
    }
}

/// The first JSON object in model text: inside a ```json fence, or the first
/// balanced `{…}` (strings respected). Small models often answer with JSON in the
/// text instead of a proper tool call.
pub fn extract_json_object(text: &str) -> Option<Value> {
    let text = strip_thinking(text);
    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(text.trim()) {
        return Some(v);
    }
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(offset) = text[start..].find('{') {
        let open = start + offset;
        let (mut depth, mut in_string, mut escaped) = (0_i32, false, false);
        for (i, &b) in bytes.iter().enumerate().skip(open) {
            if in_string {
                match b {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v @ Value::Object(_)) = serde_json::from_str(&text[open..=i]) {
                            return Some(v);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        start = open + 1;
    }
    None
}

/// Removes `<think>…</think>` blocks of reasoning models and trims.
pub fn strip_thinking(text: &str) -> String {
    let mut out = text.to_owned();
    while let Some(start) = out.find("<think>") {
        match out[start..].find("</think>") {
            Some(end) => out.replace_range(start..start + end + "</think>".len(), ""),
            None => out.truncate(start),
        }
    }
    out.trim().to_owned()
}

/// The arguments of a tool call as a JSON object, if they are one.
pub fn parse_arguments(arguments: &str) -> Option<Value> {
    if arguments.trim().is_empty() {
        return Some(json!({}));
    }
    match serde_json::from_str::<Value>(arguments) {
        Ok(v @ Value::Object(_)) => Some(v),
        // Double-encoded ("{\"a\":1}").
        Ok(Value::String(inner)) => parse_arguments(&inner).filter(|v| v.is_object()),
        _ => extract_json_object(arguments),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_in_chatty_answers() {
        assert_eq!(
            extract_json_object("Klar! ```json\n{\"a\": \"}\", \"b\": {\"c\": 1}}\n``` Fertig."),
            Some(json!({"a": "}", "b": {"c": 1}}))
        );
        assert_eq!(
            extract_json_object("<think>{\"no\": 1}</think> {\"yes\": true}"),
            Some(json!({"yes": true}))
        );
        assert_eq!(
            extract_json_object("{kaputt} und {\"ok\": 2}"),
            Some(json!({"ok": 2}))
        );
        assert_eq!(extract_json_object("gar nichts"), None);
        assert_eq!(extract_json_object("[1, 2]"), None);
    }

    #[test]
    fn arguments_are_parsed_leniently() {
        assert_eq!(parse_arguments(""), Some(json!({})));
        assert_eq!(parse_arguments("{\"x\":1}"), Some(json!({"x": 1})));
        assert_eq!(parse_arguments("\"{\\\"x\\\":1}\""), Some(json!({"x": 1})));
        assert_eq!(parse_arguments("{\"x\":"), None);
        assert_eq!(parse_arguments("42"), None);
    }

    #[test]
    fn tool_call_arguments_may_be_objects() {
        let m: Message = serde_json::from_value(json!({
            "role": "assistant", "content": null, "tool_calls": [
                {"id": "1", "type": "function", "function": {"name": "f", "arguments": {"a": 1}}},
                {"function": {"name": "g", "arguments": "{\"b\":2}"}}
            ]
        }))
        .unwrap();
        assert_eq!(m.tool_calls[0].function.arguments, "{\"a\":1}");
        assert_eq!(m.tool_calls[1].kind, "function");
        let m: Message = serde_json::from_value(
            json!({"role": "assistant", "content": "Hi", "tool_calls": null}),
        )
        .unwrap();
        assert!(m.tool_calls.is_empty());
    }

    #[test]
    fn thinking_is_removed() {
        assert_eq!(strip_thinking("<think>hm</think>\nAntwort"), "Antwort");
        assert_eq!(strip_thinking("Antwort<think>unfertig"), "Antwort");
    }
}
