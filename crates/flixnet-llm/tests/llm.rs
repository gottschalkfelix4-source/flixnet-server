//! The LLM client against a mocked OpenAI-compatible endpoint: tool-call JSON,
//! malformed output, the step limit of the tool loop.

use std::sync::Mutex;
use std::time::Duration;

use flixnet_llm::agent::{self, LIMIT_REPLY, ToolBox};
use flixnet_llm::query::{self, NaturalSearchError};
use flixnet_llm::{LlmClient, LlmConfig, LlmError, Message, ToolSpec};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> LlmClient {
    LlmClient::new(LlmConfig {
        base_url: format!("{}/v1", server.uri()),
        model: "test-model".into(),
        api_key: Some("geheim-key".into()),
        timeout: Duration::from_secs(5),
    })
}

fn tool_call(name: &str, arguments: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "x", "object": "chat.completion",
        "choices": [{ "index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": null,
            "tool_calls": [{ "id": "c1", "type": "function",
                             "function": { "name": name, "arguments": arguments } }]
        }}]
    }))
}

fn text(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{ "index": 0, "finish_reason": "stop",
                      "message": { "role": "assistant", "content": content } }]
    }))
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn genres() -> Vec<String> {
    vec!["Komödie".into(), "Drama".into()]
}

#[tokio::test]
async fn natural_language_becomes_a_filter_via_a_forced_tool_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer geheim-key"))
        .respond_with(tool_call(
            "set_filter",
            r#"{"kinds":["movie"],"genres":["Komödie"],"year_from":1990,"year_to":1999,"max_runtime_minutes":120,"played":false}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let q = query::natural_query(
        &client(&server),
        "lustige Filme aus den 90ern unter 2 Stunden die ich noch nicht gesehen habe",
        &genres(),
        2026,
    )
    .await
    .unwrap();
    assert_eq!(q.kinds, vec!["movie"]);
    assert_eq!(q.genres, vec!["Komödie"]);
    assert_eq!((q.year_from, q.year_to), (Some(1990), Some(1999)));
    assert_eq!(q.max_runtime_minutes, Some(120));
    assert_eq!(q.played, Some(false));

    let body = &bodies(&server).await[0];
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["tool_choice"]["function"]["name"], "set_filter");
    assert_eq!(body["tools"][0]["function"]["name"], "set_filter");
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["properties"]["genres"]["items"]["enum"],
        json!(genres())
    );
    assert!(
        body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("90ern")
    );
}

#[tokio::test]
async fn malformed_filter_output_is_retried_once_then_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(tool_call("set_filter", r#"{"kinds": ["movie"#))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(text("Gern! {\"genres\": [\"drama\"]}"))
        .mount(&server)
        .await;
    let q = query::natural_query(&client(&server), "Dramen", &genres(), 2026)
        .await
        .unwrap();
    assert_eq!(q.genres, vec!["Drama"]);
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    // The retry tells the model what went wrong.
    let retry = sent[1]["messages"].as_array().unwrap();
    assert!(
        retry.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("set_filter")
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(text("Ich weiß es nicht."))
        .expect(2)
        .mount(&server)
        .await;
    let err = query::natural_query(&client(&server), "irgendwas", &genres(), 2026)
        .await
        .unwrap_err();
    assert!(matches!(err, NaturalSearchError::Malformed(_)), "{err}");

    // Garbage instead of a completion.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let err = query::natural_query(&client(&server), "x", &genres(), 2026)
        .await
        .unwrap_err();
    assert!(
        matches!(err, NaturalSearchError::Llm(LlmError::Malformed(_))),
        "{err}"
    );
}

#[tokio::test]
async fn http_errors_carry_the_server_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"message": "model not found"}})),
        )
        .mount(&server)
        .await;
    let err = client(&server)
        .chat(&flixnet_llm::client::ChatRequest::new(vec![Message::user(
            "hi",
        )]))
        .await
        .unwrap_err();
    match err {
        LlmError::Http { status, message } => {
            assert_eq!(status, 404);
            assert_eq!(message, "model not found");
        }
        other => panic!("{other}"),
    }
}

/// Records calls; `lookup` answers with a fixed item.
#[derive(Default)]
struct Tools {
    calls: Mutex<Vec<(String, Value)>>,
}

impl ToolBox for Tools {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "lookup".into(),
            description: "Sucht etwas".into(),
            parameters: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        }]
    }

    async fn call(&self, name: &str, args: Value) -> Value {
        self.calls.lock().unwrap().push((name.to_owned(), args));
        json!({"items": [{"id": 7, "title": "Kinderfilm"}]})
    }
}

#[tokio::test]
async fn tool_results_go_back_to_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(tool_call("lookup", r#"{"q": "Kinder"}"#))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(text("<think>hm</think>Schau „Kinderfilm“."))
        .mount(&server)
        .await;
    let tools = Tools::default();
    let outcome = agent::run(
        &client(&server),
        vec![Message::system("s"), Message::user("Was für Kinder?")],
        &tools,
        5,
    )
    .await
    .unwrap();
    assert_eq!(outcome.reply, "Schau „Kinderfilm“.");
    assert_eq!(outcome.steps, 2);
    assert!(!outcome.hit_limit);
    assert_eq!(
        *tools.calls.lock().unwrap(),
        vec![("lookup".to_owned(), json!({"q": "Kinder"}))]
    );
    let sent = bodies(&server).await;
    let messages = sent[1]["messages"].as_array().unwrap();
    let tool_message = messages.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(tool_message["tool_call_id"], "c1");
    assert!(
        tool_message["content"]
            .as_str()
            .unwrap()
            .contains("Kinderfilm")
    );
    assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "lookup");
}

#[tokio::test]
async fn the_tool_loop_stops_after_max_steps() {
    let server = MockServer::start().await;
    // A model that never stops calling tools — and a malformed call in between.
    Mock::given(method("POST"))
        .respond_with(tool_call("lookup", "{kaputt"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(tool_call("lookup", r#"{"q": "nochmal"}"#))
        .mount(&server)
        .await;
    let tools = Tools::default();
    let outcome = agent::run(&client(&server), vec![Message::user("Endlos")], &tools, 4)
        .await
        .unwrap();
    assert!(outcome.hit_limit);
    assert_eq!(outcome.reply, LIMIT_REPLY);
    assert_eq!(outcome.steps, 4);
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 4, "never more requests than the limit");
    // Steps 1–3 offer tools, the last one does not.
    for body in &sent[..3] {
        assert!(body.get("tools").is_some());
    }
    assert!(sent[3].get("tools").is_none());
    // The malformed call was answered with an error instead of being executed.
    assert_eq!(tools.calls.lock().unwrap().len(), 2);
    let second = sent[1]["messages"].as_array().unwrap();
    let error = second.iter().find(|m| m["role"] == "tool").unwrap();
    assert!(
        error["content"]
            .as_str()
            .unwrap()
            .contains("kein gültiges JSON")
    );
}

#[tokio::test]
async fn tool_calls_written_as_text_are_understood() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(text(r#"{"name": "lookup", "arguments": {"q": "Text"}}"#))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(text("Fertig."))
        .mount(&server)
        .await;
    let tools = Tools::default();
    let outcome = agent::run(&client(&server), vec![Message::user("x")], &tools, 3)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "Fertig.");
    assert_eq!(
        *tools.calls.lock().unwrap(),
        vec![("lookup".to_owned(), json!({"q": "Text"}))]
    );
}
