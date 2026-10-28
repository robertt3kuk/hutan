//! The OpenAI-compatible adapter: `POST {base}/chat/completions` (SSE) and `GET {base}/models`.
//! `base` includes the API version (`https://api.openai.com/v1`, `http://127.0.0.1:1234/v1`,
//! `https://openrouter.ai/api/v1`), the convention of OpenAI's own SDKs. One adapter covers
//! OpenAI, OpenRouter, Groq, LM Studio, vLLM, llama.cpp's server and the like.
//!
//! Deltas come from `choices[0].delta.content`; `reasoning_content` deltas some servers add
//! are not shown. Usage comes from the final chunk that `stream_options.include_usage` asks
//! for. The reply ends with `data: [DONE]`; a server that only sends a `finish_reason` and
//! closes the stream is accepted too.

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

#[derive(Debug)]
pub(crate) struct OpenAi {
    kind: ProviderKind,
    endpoint: Endpoint,
}

impl OpenAi {
    pub(crate) fn new(kind: ProviderKind, endpoint: Endpoint) -> Self {
        Self { kind, endpoint }
    }

    fn auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, GatewayError> {
        self.endpoint.with_key(request, "authorization", "Bearer ")
    }

    fn body(&self, request: &ChatRequest) -> Value {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                if m.images.is_empty() {
                    return json!({ "role": role, "content": m.content });
                }
                // Images go as data URLs next to the text (the vision message shape every
                // OpenAI-compatible server that takes images understands).
                let mut parts = vec![json!({ "type": "text", "text": m.content })];
                parts.extend(m.images.iter().map(|image| {
                    json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", image.media_type, image.base64()),
                        },
                    })
                }));
                json!({ "role": role, "content": parts })
            })
            .collect();
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(max) = request.max_tokens {
            // OpenAI's newer models reject `max_tokens`; every compatible server still takes it.
            let field = if self.kind == ProviderKind::OpenAi {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[field] = json!(max);
        }
        body
    }
}

/// What a `/models` entry says about image input, when it says anything: OpenRouter's
/// `architecture.input_modalities`, a plain `input_modalities`, a boolean `capabilities.vision`
/// or `vision`, LM Studio's `type: "vlm"`.
fn declared_vision(entry: &Value) -> Option<bool> {
    let modalities = entry
        .pointer("/architecture/input_modalities")
        .or_else(|| entry.get("input_modalities"))
        .and_then(Value::as_array);
    if let Some(modalities) = modalities {
        return Some(modalities.iter().any(|m| m.as_str() == Some("image")));
    }
    if let Some(vision) = entry
        .pointer("/capabilities/vision")
        .or_else(|| entry.get("vision"))
        .and_then(Value::as_bool)
    {
        return Some(vision);
    }
    entry
        .get("type")
        .and_then(Value::as_str)
        .map(|kind| kind == "vlm")
}

/// OpenAI's own `/models` says nothing about modalities: the model families documented to take
/// images.
fn openai_vision_family(model: &str) -> bool {
    [
        "gpt-4o",
        "chatgpt-4o",
        "gpt-4.1",
        "gpt-4-turbo",
        "gpt-5",
        "o3",
        "o4",
    ]
    .iter()
    .any(|prefix| model.starts_with(prefix))
        || (model.starts_with("o1") && !model.contains("mini") && !model.contains("preview"))
}

impl Provider for OpenAi {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, GatewayError>> {
        async move {
            let value = self.endpoint.get_json("models", |r| self.auth(r)).await?;
            let mut models: Vec<ModelInfo> = value
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| GatewayError::Protocol("`/models` has no `data` array".into()))?
                .iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_owned();
                    Some(ModelInfo {
                        display_name: id.clone(),
                        description: m
                            .get("owned_by")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        id,
                        is_default: false,
                        hidden: false,
                        default_reasoning_effort: None,
                        reasoning_efforts: Vec::new(),
                    })
                })
                .collect();
            models.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(models)
        }
        .boxed()
    }

    fn supports_vision(&self, model: &str) -> BoxFuture<'_, Result<bool, GatewayError>> {
        let model = model.to_owned();
        async move {
            let value = self.endpoint.get_json("models", |r| self.auth(r)).await?;
            let entry = value
                .get("data")
                .and_then(Value::as_array)
                .and_then(|models| {
                    models
                        .iter()
                        .find(|m| m.get("id").and_then(Value::as_str) == Some(model.as_str()))
                });
            Ok(entry.and_then(declared_vision).unwrap_or_else(|| {
                self.kind == ProviderKind::OpenAi && openai_vision_family(&model)
            }))
        }
        .boxed()
    }

    fn stream_chat(&self, request: ChatRequest, cancel: CancellationToken) -> GatewayStream {
        let body = self.body(&request).to_string();
        let endpoint = self.endpoint.clone();
        let key = self.endpoint.key().cloned();
        let kind = self.kind;
        let open = async move {
            if kind.requires_credential() && key.is_none() {
                return Err(GatewayError::MissingCredential);
            }
            endpoint
                .send(|| {
                    let request = endpoint
                        .post("chat/completions")
                        .header("content-type", "application/json")
                        .header("accept", "text/event-stream")
                        .body(body.clone());
                    endpoint.with_key(request, "authorization", "Bearer ")
                })
                .await
        }
        .boxed();
        stream_response(
            open,
            OpenAiDecoder::new(self.endpoint.redactor().clone()),
            self.endpoint.redactor().clone(),
            cancel,
        )
    }
}

struct OpenAiDecoder {
    sse: SseDecoder,
    redactor: Redactor,
    finish_reason: Option<String>,
    usage: Option<TokenCounts>,
    completed: bool,
}

impl OpenAiDecoder {
    fn new(redactor: Redactor) -> Self {
        Self {
            sse: SseDecoder::default(),
            redactor,
            finish_reason: None,
            usage: None,
            completed: false,
        }
    }

    /// Usage first (when the provider sent it), then the end of the reply.
    fn complete(&mut self, out: &mut Vec<GatewayEvent>) {
        if self.completed {
            return;
        }
        self.completed = true;
        if let Some(usage) = self.usage.take() {
            out.push(GatewayEvent::Usage(usage));
        }
        out.push(GatewayEvent::Completed {
            finish_reason: self.finish_reason.take(),
        });
    }

    fn data(&mut self, data: &str, out: &mut Vec<GatewayEvent>) -> Result<(), GatewayError> {
        if data.trim() == "[DONE]" {
            self.complete(out);
            return Ok(());
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|e| GatewayError::Protocol(format!("a stream chunk is not JSON: {e}")))?;
        if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("the provider reported an error");
            return Err(GatewayError::Stream {
                message: self.redactor.apply(message).into_owned(),
            });
        }
        if let Some(choice) = value.pointer("/choices/0") {
            if let Some(text) = choice.pointer("/delta/content").and_then(Value::as_str)
                && !text.is_empty()
            {
                out.push(GatewayEvent::Delta(text.to_owned()));
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(reason.to_owned());
            }
        }
        if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(usage_of(usage));
        }
        Ok(())
    }
}

fn usage_of(usage: &Value) -> TokenCounts {
    let number = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64);
    TokenCounts {
        input_tokens: number("/prompt_tokens"),
        cached_input_tokens: number("/prompt_tokens_details/cached_tokens"),
        cache_write_tokens: None,
        output_tokens: number("/completion_tokens"),
        reasoning_output_tokens: number("/completion_tokens_details/reasoning_tokens"),
        total_tokens: number("/total_tokens"),
    }
}

impl Decoder for OpenAiDecoder {
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
            return Ok(out);
        }
        if self.finish_reason.is_some() {
            self.complete(&mut out);
            return Ok(out);
        }
        Err(GatewayError::Stream {
            message: "the connection closed before the reply was complete".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&str]) -> Result<Vec<GatewayEvent>, GatewayError> {
        let mut decoder = OpenAiDecoder::new(Redactor::default());
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(decoder.feed(chunk.as_bytes())?);
        }
        out.extend(decoder.finish()?);
        Ok(out)
    }

    #[test]
    fn deltas_usage_and_the_end_come_in_order() {
        let events = decode(&[
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2,\"total_tokens\":11,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
            "data: [DONE]\n\n",
        ])
        .unwrap();
        assert_eq!(
            events,
            [
                GatewayEvent::Delta("Hel".into()),
                GatewayEvent::Delta("lo".into()),
                GatewayEvent::Usage(TokenCounts {
                    input_tokens: Some(9),
                    cached_input_tokens: Some(4),
                    cache_write_tokens: None,
                    output_tokens: Some(2),
                    reasoning_output_tokens: None,
                    total_tokens: Some(11),
                }),
                GatewayEvent::Completed {
                    finish_reason: Some("stop".into())
                },
            ]
        );
    }

    #[test]
    fn a_stream_that_just_closes_after_finish_reason_completes() {
        let events = decode(&["data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"length\"}]}\n\n"]).unwrap();
        assert_eq!(
            events.last(),
            Some(&GatewayEvent::Completed {
                finish_reason: Some("length".into())
            })
        );
    }

    #[test]
    fn a_stream_cut_short_is_an_error_and_so_is_an_error_chunk() {
        let cut = decode(&["data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n"]);
        assert!(matches!(cut, Err(GatewayError::Stream { .. })), "{cut:?}");
        let error = decode(&["data: {\"error\":{\"message\":\"boom\"}}\n\n"]);
        assert!(
            matches!(&error, Err(GatewayError::Stream { message }) if message == "boom"),
            "{error:?}"
        );
    }
}
