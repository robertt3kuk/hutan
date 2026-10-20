//! The three adapters against a local mock HTTP server speaking the byte shapes the providers
//! document: streaming, usage, error mapping, cancellation, and what is (not) retried.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use futures::StreamExt;
use hutan_core::agent::TokenCounts;
use hutan_gateway::testing::{MockServer, Reply, Step};
use hutan_gateway::{
    ChatMessage, ChatRequest, GatewayError, GatewayEvent, Http, HttpConfig, Provider,
    ProviderConfig, ProviderKind, Role, Secret, connect,
};
use tokio_util::sync::CancellationToken;

const KEY: &str = "sk-test-0123456789abcdefSECRET";

fn http_with(tweak: impl FnOnce(&mut HttpConfig)) -> Http {
    let mut config = HttpConfig {
        no_proxy: true,
        retry_backoff: Duration::from_millis(5),
        read_timeout: Duration::from_secs(10),
        ..HttpConfig::default()
    };
    tweak(&mut config);
    Http::new(config).unwrap()
}

fn provider(kind: ProviderKind, server: &MockServer, key: Option<&str>) -> Box<dyn ProviderBox> {
    provider_at(kind, &server.url(), key, &http_with(|_| {}))
}

/// `Arc<dyn Provider>` with a shorter name for the tests.
trait ProviderBox: std::ops::Deref<Target = dyn Provider> {}
impl<T: std::ops::Deref<Target = dyn Provider>> ProviderBox for T {}

fn provider_at(
    kind: ProviderKind,
    url: &str,
    key: Option<&str>,
    http: &Http,
) -> Box<dyn ProviderBox> {
    // OpenAI-style bases carry the API version.
    let base = match kind {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => format!("{url}/v1"),
        _ => url.to_owned(),
    };
    let arc = connect(
        &ProviderConfig {
            kind,
            base_url: Some(base),
            api_key: key.map(Secret::new),
        },
        http,
    )
    .unwrap();
    Box::new(arc)
}

fn request() -> ChatRequest {
    ChatRequest::new(
        "test-model",
        vec![
            ChatMessage::new(Role::System, "be brief"),
            ChatMessage::new(Role::User, "hi"),
        ],
    )
}

async fn collect(
    provider: &dyn Provider,
    request: ChatRequest,
) -> (Vec<GatewayEvent>, Option<GatewayError>) {
    let mut stream = provider.stream_chat(request, CancellationToken::new());
    let mut events = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    (events, error)
}

fn deltas(events: &[GatewayEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            GatewayEvent::Delta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

const OPENAI_STREAM: [&str; 5] = [
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\ndata: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2,\"total_tokens\":11}}\n\n",
    "data: [DONE]\n\n",
];

// OpenAI-compatible -----------------------------------------------------------------------------

#[tokio::test]
async fn openai_streams_deltas_then_usage_then_the_end() {
    let server = MockServer::start().await;
    server.route("POST", "/v1/chat/completions", Reply::sse(OPENAI_STREAM));
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));

    let mut req = request();
    req.temperature = Some(0.5);
    req.max_tokens = Some(64);
    let (events, error) = collect(&**provider, req).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(deltas(&events), "Hello");
    assert_eq!(
        events[2],
        GatewayEvent::Usage(TokenCounts {
            input_tokens: Some(9),
            output_tokens: Some(2),
            total_tokens: Some(11),
            ..TokenCounts::default()
        })
    );
    assert_eq!(
        events[3],
        GatewayEvent::Completed {
            finish_reason: Some("stop".into())
        }
    );

    let sent = &server.requests()[0];
    assert_eq!(
        sent.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    let body = sent.json();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["max_completion_tokens"], 64);
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "hi");
}

#[tokio::test]
async fn an_openai_compatible_server_needs_no_key_and_takes_max_tokens() {
    let server = MockServer::start().await;
    server.route("POST", "/v1/chat/completions", Reply::sse(OPENAI_STREAM));
    let provider = provider(ProviderKind::OpenAiCompatible, &server, None);
    let mut req = request();
    req.max_tokens = Some(7);
    let (events, error) = collect(&**provider, req).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(deltas(&events), "Hello");
    let sent = &server.requests()[0];
    assert_eq!(sent.header("authorization"), None);
    assert_eq!(sent.json()["max_tokens"], 7);
}

#[tokio::test]
async fn openai_lists_models_sorted() {
    let server = MockServer::start().await;
    server.route(
        "GET",
        "/v1/models",
        Reply::json(
            200,
            r#"{"object":"list","data":[{"id":"gpt-b","owned_by":"openai"},{"id":"gpt-a","owned_by":"system"}]}"#,
        ),
    );
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));
    let models = provider.list_models().await.unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["gpt-a", "gpt-b"]
    );
    assert_eq!(models[0].description, "system");
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
}

#[tokio::test]
async fn a_midstream_error_chunk_fails_the_stream_after_the_text_so_far() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        Reply::sse([
            "data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n",
            "data: {\"error\":{\"message\":\"The server had an error\",\"type\":\"server_error\"}}\n\n",
        ]),
    );
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));
    let (events, error) = collect(&**provider, request()).await;
    assert_eq!(deltas(&events), "par");
    assert!(
        matches!(&error, Some(GatewayError::Stream { message }) if message == "The server had an error"),
        "{error:?}"
    );
}

// Anthropic -------------------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_streams_text_and_usage_with_its_headers() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/messages",
        Reply::sse([
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ]),
    );
    let provider = provider(ProviderKind::Anthropic, &server, Some(KEY));
    let (events, error) = collect(&**provider, request()).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(deltas(&events), "Hello");
    assert_eq!(
        events[2],
        GatewayEvent::Usage(TokenCounts {
            input_tokens: Some(25),
            output_tokens: Some(15),
            ..TokenCounts::default()
        })
    );
    assert_eq!(
        events[3],
        GatewayEvent::Completed {
            finish_reason: Some("end_turn".into())
        }
    );
    let sent = &server.requests()[0];
    assert_eq!(sent.header("x-api-key"), Some(KEY));
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(sent.header("authorization"), None);
    let body = sent.json();
    assert_eq!(body["system"], "be brief");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["stream"], true);
    assert!(body["max_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn anthropic_lists_models_and_reports_its_in_stream_error() {
    let server = MockServer::start().await;
    server.route(
        "GET",
        "/v1/models",
        Reply::json(
            200,
            r#"{"data":[{"type":"model","id":"claude-x","display_name":"Claude X","created_at":"2026-01-01T00:00:00Z"}],"has_more":false}"#,
        ),
    );
    server.route(
        "POST",
        "/v1/messages",
        Reply::sse([
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        ]),
    );
    let provider = provider(ProviderKind::Anthropic, &server, Some(KEY));
    let models = provider.list_models().await.unwrap();
    assert_eq!(models[0].id, "claude-x");
    assert_eq!(models[0].display_name, "Claude X");
    let (events, error) = collect(&**provider, request()).await;
    assert!(events.is_empty());
    assert!(
        matches!(&error, Some(GatewayError::Stream { message }) if message == "overloaded_error: Overloaded"),
        "{error:?}"
    );
}

// Ollama ----------------------------------------------------------------------------------------

#[tokio::test]
async fn ollama_streams_ndjson_and_reports_counts() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/api/chat",
        Reply::ndjson([
            "{\"model\":\"llama3\",\"created_at\":\"2026-10-15T10:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n",
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"lo\"},\"done\":false}\n{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"total_duration\":5,\"prompt_eval_count\":26,\"eval_count\":3}\n",
        ]),
    );
    let provider = provider(ProviderKind::Ollama, &server, None);
    let mut req = request();
    req.temperature = Some(0.2);
    req.max_tokens = Some(16);
    let (events, error) = collect(&**provider, req).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(deltas(&events), "Hello");
    assert_eq!(
        events[2],
        GatewayEvent::Usage(TokenCounts {
            input_tokens: Some(26),
            output_tokens: Some(3),
            ..TokenCounts::default()
        })
    );
    let body = server.requests()[0].json();
    assert_eq!(body["options"]["num_predict"], 16);
    assert_eq!(body["options"]["temperature"], 0.2);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(server.requests()[0].header("authorization"), None);
}

#[tokio::test]
async fn ollama_lists_tags_and_maps_an_unknown_model_to_not_found() {
    let server = MockServer::start().await;
    server.route(
        "GET",
        "/api/tags",
        Reply::json(
            200,
            r#"{"models":[{"name":"qwen3:8b","model":"qwen3:8b","size":1,"details":{"family":"qwen3","parameter_size":"8B","quantization_level":"Q4_K_M"}},{"name":"llama3:latest","model":"llama3:latest"}]}"#,
        ),
    );
    server.route(
        "POST",
        "/api/chat",
        Reply::json(404, r#"{"error":"model 'nope' not found"}"#),
    );
    let provider = provider(ProviderKind::Ollama, &server, None);
    let models = provider.list_models().await.unwrap();
    assert_eq!(models[0].id, "llama3:latest");
    assert_eq!(models[1].description, "qwen3 8B Q4_K_M");
    let (_, error) = collect(&**provider, request()).await;
    assert!(
        matches!(&error, Some(GatewayError::NotFound { message }) if message == "model 'nope' not found"),
        "{error:?}"
    );
}

// Errors ----------------------------------------------------------------------------------------

#[tokio::test]
async fn statuses_map_to_clear_errors_and_are_never_retried() {
    let server = MockServer::start().await;
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));
    let route = |status: u16, body: &str, extra: Option<(&str, &str)>| {
        let mut reply = Reply::json(status, body);
        if let Some((n, v)) = extra {
            reply = reply.header(n, v);
        }
        reply
    };
    type Check = Box<dyn Fn(&GatewayError) -> bool>;
    let cases: Vec<(Reply, Check)> = vec![
        (
            route(
                401,
                r#"{"error":{"message":"Incorrect API key provided"}}"#,
                None,
            ),
            Box::new(|e| matches!(e, GatewayError::Unauthorized { status: 401, .. })),
        ),
        (
            route(403, r#"{"error":{"message":"not allowed"}}"#, None),
            Box::new(|e| matches!(e, GatewayError::Unauthorized { status: 403, .. })),
        ),
        (
            route(
                404,
                r#"{"error":{"message":"The model `x` does not exist","code":"model_not_found"}}"#,
                None,
            ),
            Box::new(
                |e| matches!(e, GatewayError::NotFound { message } if message.contains("does not exist")),
            ),
        ),
        (
            route(
                429,
                r#"{"error":{"message":"Rate limit reached"}}"#,
                Some(("retry-after", "7")),
            ),
            Box::new(|e| {
                e.retry_after() == Some(Duration::from_secs(7))
                    && e.to_string().contains("retry after 7s")
            }),
        ),
        (
            route(500, "upstream exploded", None),
            Box::new(
                |e| matches!(e, GatewayError::Server { status: 500, message } if message == "upstream exploded"),
            ),
        ),
        (
            route(503, r#"{"error":{"message":"overloaded"}}"#, None),
            Box::new(|e| matches!(e, GatewayError::Server { status: 503, .. })),
        ),
        (
            route(400, r#"{"error":{"message":"bad field"}}"#, None),
            Box::new(
                |e| matches!(e, GatewayError::Rejected { status: 400, message } if message == "bad field"),
            ),
        ),
        (
            route(302, "", Some(("location", "http://127.0.0.1:1/elsewhere"))),
            Box::new(|e| matches!(e, GatewayError::Rejected { status: 302, .. })),
        ),
    ];
    for (n, (reply, check)) in cases.into_iter().enumerate() {
        server.set_route("POST", "/v1/chat/completions", reply);
        let (events, error) = collect(&**provider, request()).await;
        assert!(events.is_empty());
        let error = error.expect("an error");
        assert!(check(&error), "case {n}: {error:?}");
        // One request per case: an HTTP error is not retried.
        assert_eq!(
            server.requests_to("/v1/chat/completions").len(),
            n + 1,
            "case {n}"
        );
    }
}

#[tokio::test]
async fn a_provider_echoing_the_key_is_scrubbed_from_the_error() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        Reply::json(
            401,
            &format!(
                r#"{{"error":{{"message":"Incorrect API key provided: {KEY}. You can find yours at ... (sk-proj-****abcd)"}}}}"#
            ),
        ),
    );
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));
    let (_, error) = collect(&**provider, request()).await;
    let text = format!("{:?} {}", error, error.as_ref().unwrap());
    assert!(!text.contains("SECRET"), "{text}");
    assert!(!text.contains("sk-test"), "{text}");
    assert!(!text.contains("sk-proj"), "{text}");
}

#[tokio::test]
async fn a_missing_key_fails_before_any_request_is_made() {
    let server = MockServer::start().await;
    for kind in [ProviderKind::OpenAi, ProviderKind::Anthropic] {
        let provider = provider(kind, &server, None);
        let (_, error) = collect(&**provider, request()).await;
        assert!(
            matches!(error, Some(GatewayError::MissingCredential)),
            "{kind}"
        );
    }
    assert!(server.requests().is_empty());
}

// Retry and cancellation ------------------------------------------------------------------------

/// A loopback address nothing listens on.
async fn closed_port() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}

#[tokio::test]
async fn connect_errors_are_retried_a_bounded_number_of_times() {
    let url = closed_port().await;
    let http = http_with(|c| c.connect_retries = 2);
    let provider = provider_at(ProviderKind::Ollama, &url, None, &http);
    let (events, error) = collect(&**provider, request()).await;
    assert!(events.is_empty());
    assert!(
        matches!(&error, Some(GatewayError::Connect { attempts: 3, .. })),
        "{error:?}"
    );
    // Model lists use the same policy.
    let error = provider.list_models().await.unwrap_err();
    assert!(
        matches!(error, GatewayError::Connect { attempts: 3, .. }),
        "{error:?}"
    );

    let http = http_with(|c| c.connect_retries = 0);
    let provider = provider_at(ProviderKind::Ollama, &url, None, &http);
    let (_, error) = collect(&**provider, request()).await;
    assert!(matches!(
        error,
        Some(GatewayError::Connect { attempts: 1, .. })
    ));
}

#[tokio::test]
async fn a_stream_that_breaks_after_bytes_arrived_is_not_retried() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        Reply::sse(["data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n"]).then_reset(),
    );
    let http = http_with(|c| c.connect_retries = 3);
    let provider = provider_at(ProviderKind::OpenAi, &server.url(), Some(KEY), &http);
    let (events, error) = collect(&**provider, request()).await;
    assert_eq!(deltas(&events), "par");
    assert!(
        matches!(error, Some(GatewayError::Stream { .. })),
        "{error:?}"
    );
    assert_eq!(server.connections(), 1, "no second connection was tried");
    assert_eq!(server.requests_to("/v1/chat/completions").len(), 1);
}

#[tokio::test]
async fn a_server_that_hangs_up_before_answering_is_not_retried_either() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        Reply::sse(Vec::<&str>::new()).then_reset(),
    );
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));
    let (_, error) = collect(&**provider, request()).await;
    assert!(
        matches!(error, Some(GatewayError::Stream { .. })),
        "{error:?}"
    );
    assert_eq!(server.requests_to("/v1/chat/completions").len(), 1);
}

#[tokio::test]
async fn cancelling_mid_stream_ends_the_stream_and_closes_the_connection() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        Reply::sse(["data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n"]).then_hold(),
    );
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));
    let cancel = CancellationToken::new();
    let mut stream = provider.stream_chat(request(), cancel.clone());
    assert_eq!(
        stream.next().await.unwrap().unwrap(),
        GatewayEvent::Delta("one".into())
    );
    cancel.cancel();
    assert!(matches!(
        stream.next().await,
        Some(Err(GatewayError::Cancelled))
    ));
    assert!(stream.next().await.is_none());
    drop(stream);
    tokio::time::timeout(Duration::from_secs(10), server.wait_for_disconnects(1))
        .await
        .expect("the server saw the client hang up");
}

#[tokio::test]
async fn cancelling_before_the_provider_answers_works_too() {
    let server = MockServer::start().await;
    // Headers only, then silence: the request is out but no event ever arrives.
    server.route(
        "POST",
        "/api/chat",
        Reply::ndjson(Vec::<&str>::new()).then(Step::Hold),
    );
    let provider = provider(ProviderKind::Ollama, &server, None);
    let cancel = CancellationToken::new();
    let mut stream = provider.stream_chat(request(), cancel.clone());
    let waiting = tokio::spawn(async move { stream.next().await });
    server.wait_for_requests("/api/chat", 1).await;
    cancel.cancel();
    let item = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(item, Some(Err(GatewayError::Cancelled))));
    tokio::time::timeout(Duration::from_secs(10), server.wait_for_disconnects(1))
        .await
        .expect("the server saw the client hang up");
}

#[tokio::test]
async fn silence_longer_than_the_read_timeout_is_a_timeout() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        Reply::sse(["data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n"]).then_hold(),
    );
    let http = http_with(|c| c.read_timeout = Duration::from_millis(300));
    let provider = provider_at(ProviderKind::OpenAi, &server.url(), Some(KEY), &http);
    let (events, error) = collect(&**provider, request()).await;
    assert_eq!(deltas(&events), "one");
    assert!(matches!(error, Some(GatewayError::Timeout)), "{error:?}");
}

// Redaction -------------------------------------------------------------------------------------

#[tokio::test]
async fn nothing_the_gateway_logs_contains_the_key_or_a_body() {
    use std::io::Write;
    use std::sync::Arc;

    use parking_lot::Mutex;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let server = MockServer::start().await;
    server.route("POST", "/v1/chat/completions", Reply::sse(OPENAI_STREAM));
    server.route(
        "GET",
        "/v1/models",
        Reply::json(200, r#"{"data":[{"id":"m"}]}"#),
    );
    let provider = provider(ProviderKind::OpenAi, &server, Some(KEY));
    let mut req = request();
    req.messages
        .push(ChatMessage::new(Role::User, "PRIVATE-PROMPT-TEXT"));
    let (_, error) = collect(&**provider, req).await;
    assert!(error.is_none());
    provider.list_models().await.unwrap();

    let logs = String::from_utf8(capture.0.lock().clone()).unwrap();
    assert!(!logs.is_empty(), "the subscriber saw nothing");
    assert!(!logs.contains("SECRET"), "{logs}");
    assert!(!logs.contains(KEY), "{logs}");
    assert!(!logs.contains("PRIVATE-PROMPT-TEXT"), "{logs}");
}
