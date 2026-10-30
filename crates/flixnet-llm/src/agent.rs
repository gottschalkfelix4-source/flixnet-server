//! Tool-calling loop for the chat assistant, with a hard step limit.
//!
//! Each step is one request to the model. While the model asks for tools, they are
//! executed and their results appended; the last allowed step is sent without tools,
//! so the model has to answer. A model that still insists on tools gets a fixed
//! answer — the loop never makes more than `max_steps` requests.

use std::future::Future;

use serde_json::{Value, json};

use crate::client::{
    ChatRequest, LlmClient, LlmError, Message, Role, ToolCall, ToolChoice, ToolSpec,
    extract_json_object, parse_arguments, strip_thinking,
};

/// Tools per model reply that are executed (the rest are answered with an error).
pub const MAX_CALLS_PER_STEP: usize = 4;
/// Tool results are cut to this many characters before they go back to the model.
pub const MAX_RESULT_CHARS: usize = 6000;

/// The tools the assistant may use. Implementations decide what the viewer may see.
pub trait ToolBox: Send + Sync {
    fn specs(&self) -> Vec<ToolSpec>;
    /// Runs a tool. Errors are reported to the model as `{"error": "…"}`.
    fn call(&self, name: &str, args: Value) -> impl Future<Output = Value> + Send;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutedCall {
    pub name: String,
    pub args: Value,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutcome {
    pub reply: String,
    /// Requests made to the model.
    pub steps: usize,
    pub calls: Vec<ExecutedCall>,
    /// The model wanted more tools after the last step.
    pub hit_limit: bool,
}

/// The fixed answer when the step limit is reached.
pub const LIMIT_REPLY: &str =
    "Dafür habe ich zu viele Schritte gebraucht. Formuliere die Frage bitte etwas genauer.";

/// A tool call written as JSON text (`{"name": …, "arguments": {…}}`) by models
/// without native tool support.
fn text_tool_call(message: &Message, tools: &[ToolSpec]) -> Option<ToolCall> {
    let value = extract_json_object(message.content.as_deref()?)?;
    let name = value.get("name")?.as_str()?;
    if !tools.iter().any(|t| t.name == name) {
        return None;
    }
    let args = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(ToolCall {
        id: "text-call".to_owned(),
        kind: "function".to_owned(),
        function: crate::client::FunctionCall {
            name: name.to_owned(),
            arguments: match args {
                Value::String(s) => s,
                other => other.to_string(),
            },
        },
    })
}

fn truncate(text: String) -> String {
    if text.chars().count() <= MAX_RESULT_CHARS {
        text
    } else {
        let mut cut: String = text.chars().take(MAX_RESULT_CHARS).collect();
        cut.push_str("…(gekürzt)");
        cut
    }
}

/// Runs the conversation until the model answers in text or `max_steps` requests
/// were made (`max_steps` ≥ 1).
pub async fn run<T: ToolBox>(
    client: &LlmClient,
    mut messages: Vec<Message>,
    tools: &T,
    max_steps: usize,
) -> Result<AgentOutcome, LlmError> {
    let max_steps = max_steps.max(1);
    let specs = tools.specs();
    let mut calls = Vec::new();
    for step in 1..=max_steps {
        let last = step == max_steps;
        let mut request = ChatRequest::new(messages.clone());
        request.temperature = 0.3;
        if !last {
            request.tools = specs.clone();
            request.tool_choice = ToolChoice::Auto;
        }
        let mut reply = client.chat(&request).await?;
        if reply.tool_calls.is_empty()
            && let Some(call) = text_tool_call(&reply, &specs)
        {
            reply.tool_calls = vec![call];
            reply.content = None;
        }
        if reply.tool_calls.is_empty() {
            let text = strip_thinking(reply.content.as_deref().unwrap_or_default());
            return Ok(AgentOutcome {
                reply: if text.is_empty() {
                    "Dazu habe ich leider keine Antwort.".to_owned()
                } else {
                    text
                },
                steps: step,
                calls,
                hit_limit: false,
            });
        }
        if last {
            return Ok(AgentOutcome {
                reply: LIMIT_REPLY.to_owned(),
                steps: step,
                calls,
                hit_limit: true,
            });
        }
        // Ids are needed to pair results with calls; some servers omit them.
        for (i, call) in reply.tool_calls.iter_mut().enumerate() {
            if call.id.is_empty() || call.id == "text-call" {
                call.id = format!("call_{step}_{i}");
            }
        }
        messages.push(Message {
            role: Role::Assistant,
            content: reply.content.clone(),
            tool_calls: reply.tool_calls.clone(),
            tool_call_id: None,
        });
        for (i, call) in reply.tool_calls.iter().enumerate() {
            let result = if i >= MAX_CALLS_PER_STEP {
                json!({ "error": "Zu viele Werkzeugaufrufe auf einmal." })
            } else if !specs.iter().any(|s| s.name == call.function.name) {
                json!({ "error": format!("Unbekanntes Werkzeug {}.", call.function.name) })
            } else {
                match parse_arguments(&call.function.arguments) {
                    Some(args) => {
                        let result = tools.call(&call.function.name, args.clone()).await;
                        calls.push(ExecutedCall {
                            name: call.function.name.clone(),
                            args,
                            result: result.clone(),
                        });
                        result
                    }
                    None => json!({ "error": "Die Argumente sind kein gültiges JSON-Objekt." }),
                }
            };
            messages.push(Message::tool_result(
                call.id.clone(),
                truncate(result.to_string()),
            ));
        }
    }
    unreachable!("the last step always returns")
}
