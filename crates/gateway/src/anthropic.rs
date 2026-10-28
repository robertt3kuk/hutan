//! The Anthropic adapter: `POST {base}/v1/messages` (SSE) and `GET {base}/v1/models`, with the
//! `x-api-key` and `anthropic-version` headers. Only `text_delta`s are shown; thinking and
//! tool-use blocks are not requested and ignored if they arrive.
//!
//! `system` messages go into the request's top-level `system` field; `max_tokens` is required
//! by the API, so 8192 is used when the caller sets none. Usage is the `input_tokens` of
//! `message_start` plus the `output_tokens` of the last `message_delta`; the cache-read count
//! maps to `cached_input_tokens`.

use futures::FutureExt;
use futures::future::BoxFuture;
use hutan_core::ModelInfo;
use hutan_core::agent::TokenCounts;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::GatewayError;
use crate::framing::SseDecoder;
use crate::http::{Decoder, Endpoint, stream_response};
use crate::provider::Provider;
use crate::redact::Redactor;
use crate::types::{ChatRequest, GatewayEvent, GatewayStream, ProviderKind, Role};

/// The API version header; the Messages API is stable under it.
const API_VERSION: &str = "2023-06-01";
/// `max_tokens` when the request names none.
const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Debug)]
pub(crate) struct Anthropic {
    endpoint: Endpoint,
}

impl Anthropic {
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }
}

fn auth(
    endpoint: &Endpoint,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::RequestBuilder, GatewayError> {
    let request = request.header("anthropic-version", API_VERSION);
    endpoint.with_key(request, "x-api-key", "")
}

fn body(request: &ChatRequest) -> Value {
    let system: Vec<&str> = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();
    let messages: Vec<Value> = request
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| {
            let role = if m.role == Role::User {
                "user"
            } else {
                "assistant"
            };
            if m.images.is_empty() {
                return json!({ "role": role, "content": m.content });
            }
            // Images first, then the text, as Anthropic recommends.
            let mut blocks: Vec<Value> = m
                .images
                .iter()
                .map(|image| {
                    json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": image.media_type,
                            "data": image.base64(),
                        },
                    })
                })
                .collect();
            blocks.push(json!({ "type": "text", "text": m.content }));
            json!({ "role": role, "content": blocks })
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    body
}

impl Provider for Anthropic {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, GatewayError>> {
        async move {
            if self.endpoint.key().is_none() {
                return Err(GatewayError::MissingCredential);
            }
            let value = self
                .endpoint
                .get_json("v1/models?limit=1000", |r| auth(&self.endpoint, r))
                .await?;
            let models = value
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| GatewayError::Protocol("`/v1/models` has no `data` array".into()))?
                .iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_owned();
                    Some(ModelInfo {
                        display_name: m
                            .get("display_name")
                            .and_then(Value::as_str)
                            .unwrap_or(&id)
                            .to_owned(),
                        id,
                        description: String::new(),
                        is_default: false,
                        hidden: false,
                        default_reasoning_effort: None,
                        reasoning_efforts: Vec::new(),
                    })
                })
                .collect();
            Ok(models)
        }
        .boxed()
    }

    fn supports_vision(&self, model: &str) -> BoxFuture<'_, Result<bool, GatewayError>> {
        let model = model.to_owned();
        async move {
            if self.endpoint.key().is_none() {
                return Err(GatewayError::MissingCredential);
            }
            let value = self
                .endpoint
                .get_json("v1/models?limit=1000", |r| auth(&self.endpoint, r))
                .await?;
            let entry = value
                .get("data")
                .and_then(Value::as_array)
                .and_then(|models| {
                    models
                        .iter()
                        .find(|m| m.get("id").and_then(Value::as_str) == Some(model.as_str()))
                });
            // The API says per model; a model it does not describe is taken for a Claude
            // model, and every current Claude model reads images.
            Ok(entry
                .and_then(|m| m.pointer("/capabilities/image_input/supported"))
                .and_then(Value::as_bool)
                .unwrap_or(true))
        }
        .boxed()
    }

    fn stream_chat(&self, request: ChatRequest, cancel: CancellationToken) -> GatewayStream {
        let body = body(&request).to_string();
        let endpoint = self.endpoint.clone();
        let open = async move {
            if endpoint.key().is_none() {
                return Err(GatewayError::MissingCredential);
            }
            endpoint
                .send(|| {
                    let request = endpoint
                        .post("v1/messages")
                        .header("content-type", "application/json")
                        .header("accept", "text/event-stream")
                        .body(body.clone());
                    auth(&endpoint, request)
                })
                .await
        }
        .boxed();
        stream_response(
            open,
            AnthropicDecoder::new(self.endpoint.redactor().clone()),
            self.endpoint.redactor().clone(),
            cancel,
        )
    }
}

struct AnthropicDecoder {
    sse: SseDecoder,
    redactor: Redactor,
    usage: TokenCounts,
    stop_reason: Option<String>,
    completed: bool,
}

impl AnthropicDecoder {
    fn new(redactor: Redactor) -> Self {
        Self {
            sse: SseDecoder::default(),
            redactor,
            usage: TokenCounts::default(),
            stop_reason: None,
            completed: false,
        }
    }

    fn data(&mut self, data: &str, out: &mut Vec<GatewayEvent>) -> Result<(), GatewayError> {
        let value: Value = serde_json::from_str(data)
            .map_err(|e| GatewayError::Protocol(format!("a stream event is not JSON: {e}")))?;
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let number = |p: &str| value.pointer(p).and_then(Value::as_u64);
                self.usage.input_tokens = number("/message/usage/input_tokens");
                self.usage.cached_input_tokens = number("/message/usage/cache_read_input_tokens");
                self.usage.cache_write_tokens =
                    number("/message/usage/cache_creation_input_tokens");
                self.usage.output_tokens = number("/message/usage/output_tokens");
            }
            Some("content_block_delta") => {
                if value.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta")
                    && let Some(text) = value.pointer("/delta/text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    out.push(GatewayEvent::Delta(text.to_owned()));
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_owned());
                }
                if let Some(tokens) = value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                {
                    self.usage.output_tokens = Some(tokens);
                }
            }
            Some("message_stop") => {
                self.completed = true;
                if self.usage != TokenCounts::default() {
                    out.push(GatewayEvent::Usage(self.usage));
                }
                out.push(GatewayEvent::Completed {
                    finish_reason: self.stop_reason.take(),
                });
            }
            Some("error") => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the provider reported an error");
                let kind = value
                    .pointer("/error/type")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                return Err(GatewayError::Stream {
                    message: self
                        .redactor
                        .apply(&format!("{kind}: {message}"))
                        .into_owned(),
                });
            }
            // `ping`, `content_block_start`, `content_block_stop`, and kinds added later.
            _ => {}
        }
        Ok(())
    }
}

impl Decoder for AnthropicDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<GatewayEvent>, GatewayError> {
        let mut out = Vec::new();
        for event in self.sse.push(bytes)? {
            self.data(&event.data, &mut out)?;
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<GatewayEvent>, GatewayError> {
        let mut out = Vec::new();
        for event in self.sse.finish() {
            self.data(&event.data, &mut out)?;
        }
        if self.completed {
            Ok(out)
        } else {
            Err(GatewayError::Stream {
                message: "the connection closed before the reply was complete".into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;

    #[test]
    fn system_messages_move_to_the_top_level_field() {
        let request = ChatRequest::new(
            "claude-x",
            vec![
                ChatMessage::new(Role::System, "be brief"),
                ChatMessage::new(Role::User, "hi"),
                ChatMessage::new(Role::Assistant, "hello"),
                ChatMessage::new(Role::User, "again"),
            ],
        );
        let body = body(&request);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(3));
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn usage_combines_start_and_delta_and_follows_the_text() {
        let mut decoder = AnthropicDecoder::new(Redactor::default());
        let mut out = Vec::new();
        for data in [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"cache_read_input_tokens":10,"output_tokens":1}}}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            decoder.data(data, &mut out).unwrap();
        }
        assert_eq!(
            out,
            [
                GatewayEvent::Delta("Hi".into()),
                GatewayEvent::Usage(TokenCounts {
                    input_tokens: Some(25),
                    cached_input_tokens: Some(10),
                    cache_write_tokens: None,
                    output_tokens: Some(15),
                    reasoning_output_tokens: None,
                    total_tokens: None,
                }),
                GatewayEvent::Completed {
                    finish_reason: Some("end_turn".into())
                },
            ]
        );
    }
}
