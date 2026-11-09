//! Wire-format details belong here, not in the agent loop or provider.
use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::metadata::{BlockCounts, ResponseMetadata, StopReason, TokenUsage};
use super::{Message, Output, PromptCache, Reply, ToolCall};

pub struct EncodedRequest {
    pub path: &'static str,
    pub headers: HeaderMap,
    pub body: Value,
}

pub trait Protocol {
    fn encode(
        &self,
        model: &str,
        system: &str,
        messages: &[Message],
        tools: &[Value],
        prompt_cache: PromptCache,
    ) -> EncodedRequest;

    // Inspection is infallible and does not establish reply validity.
    fn response_metadata(&self, body: &Value) -> ResponseMetadata;

    fn decode(&self, body: Value) -> Result<Reply>;
}

pub struct AnthropicMessages;

impl AnthropicMessages {
    /// A chars/4 wire-size heuristic, including opaque continuation. This is
    /// neither exact tokenization nor a bound on the complete model request.
    pub(super) fn estimate_message_tokens(&self, message: &Message) -> usize {
        encode_message(message)
            .to_string()
            .chars()
            .count()
            .div_ceil(4)
    }

    pub(super) fn encode_with_summary(
        &self,
        model: &str,
        system: &str,
        summary: &str,
        messages: &[Message],
        tools: &[Value],
        prompt_cache: PromptCache,
    ) -> EncodedRequest {
        let messages = std::iter::once(encode_message(&super::summary_message(summary)))
            .chain(messages.iter().map(encode_message))
            .collect();
        self.encode_context(model, system, messages, tools, prompt_cache)
    }

    fn encode_context(
        &self,
        model: &str,
        system: &str,
        messages: Vec<Value>,
        tools: &[Value],
        prompt_cache: PromptCache,
    ) -> EncodedRequest {
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        let mut body = json!({
            "model": model,
            "max_tokens": 8000,
            "system": system,
            "messages": messages,
            "tools": tools,
        });
        if prompt_cache == PromptCache::Short {
            apply_prompt_cache(&mut body);
        }
        EncodedRequest {
            path: "/messages",
            headers,
            body,
        }
    }

    // Summary policy is request-local: no advertised tools or generated cache
    // markers, and a caller-supplied output cap. Ordinary encoding is unchanged.
    pub(super) fn encode_summary(
        &self,
        model: &str,
        prompt: &str,
        max_output_tokens: u32,
    ) -> EncodedRequest {
        let mut encoded = self.encode(
            model,
            "You summarize historical conversation data. Return only a context checkpoint; do not continue the task or call tools.",
            &[Message::User(prompt.to_owned())],
            &[],
            PromptCache::Off,
        );
        encoded.body["max_tokens"] = json!(max_output_tokens);
        encoded
    }
}

impl Protocol for AnthropicMessages {
    fn encode(
        &self,
        model: &str,
        system: &str,
        messages: &[Message],
        tools: &[Value],
        prompt_cache: PromptCache,
    ) -> EncodedRequest {
        self.encode_context(
            model,
            system,
            messages.iter().map(encode_message).collect(),
            tools,
            prompt_cache,
        )
    }

    fn decode(&self, body: Value) -> Result<Reply> {
        let blocks = body["content"]
            .as_array()
            .context("model response has no content blocks")?;
        let mut output = Vec::new();
        for block in blocks {
            let block_type = block
                .as_object()
                .context("content block is not an object")?
                .get("type")
                .and_then(Value::as_str)
                .context("content block has no string type")?;
            match block_type {
                "text" => output.push(Output::Text(
                    block["text"]
                        .as_str()
                        .context("text block has no text")?
                        .to_owned(),
                )),
                "tool_use" => output.push(Output::ToolCall(ToolCall {
                    id: block["id"]
                        .as_str()
                        .context("tool use has no ID")?
                        .to_owned(),
                    name: block["name"]
                        .as_str()
                        .context("tool use has no name")?
                        .to_owned(),
                    arguments: block["input"]
                        .as_object()
                        .context("tool use input is not an object")?
                        .clone()
                        .into(),
                })),
                // Unknown/thinking blocks are not executable agent actions,
                // but must survive in the next request exactly as received.
                _ => {}
            }
        }
        Ok(Reply {
            output,
            continuation: blocks.clone(),
        })
    }

    fn response_metadata(&self, body: &Value) -> ResponseMetadata {
        let blocks = body["content"].as_array();
        let mut counts = BlockCounts::default();
        let mut text_bytes = Some(0);
        for block in blocks.into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    counts.text += 1;
                    // One malformed text block makes the total unavailable, not zero.
                    text_bytes = text_bytes
                        .zip(block["text"].as_str())
                        .map(|(bytes, text)| bytes + text.len());
                }
                Some("thinking") => counts.thinking += 1,
                Some("redacted_thinking") => counts.redacted_thinking += 1,
                Some("tool_use") => counts.tool_use += 1,
                _ => counts.unknown += 1,
            }
        }
        let stop_reason = body["stop_reason"].as_str().map(|reason| match reason {
            "end_turn" => StopReason::EndTurn,
            "max_tokens" => StopReason::MaxTokens,
            "tool_use" => StopReason::ToolUse,
            "stop_sequence" => StopReason::StopSequence,
            "pause_turn" => StopReason::PauseTurn,
            "refusal" => StopReason::Refusal,
            "model_context_window_exceeded" => StopReason::ModelContextWindowExceeded,
            _ => StopReason::Unknown,
        });
        // Missing or malformed optional metadata must not change whether an otherwise
        // valid reply is executable. Unknown usage is absent, not fabricated as zero.
        let usage = body["usage"].as_object().map(|_| TokenUsage {
            input_tokens: body["usage"]["input_tokens"].as_u64(),
            output_tokens: body["usage"]["output_tokens"].as_u64(),
            cache_read_input_tokens: body["usage"]["cache_read_input_tokens"].as_u64(),
            cache_creation_input_tokens: body["usage"]["cache_creation_input_tokens"].as_u64(),
            cache_creation_5m_input_tokens:
                body["usage"]["cache_creation"]["ephemeral_5m_input_tokens"].as_u64(),
            cache_creation_1h_input_tokens:
                body["usage"]["cache_creation"]["ephemeral_1h_input_tokens"].as_u64(),
        });
        ResponseMetadata {
            response_id: body["id"].as_str().map(str::to_owned),
            response_model: body["model"].as_str().map(str::to_owned),
            stop_reason,
            usage,
            blocks: blocks.map(|_| counts),
            text_bytes: blocks.and(text_bytes),
        }
    }
}

fn encode_message(message: &Message) -> Value {
    match message {
        Message::User(text) => json!({"role": "user", "content": text}),
        Message::Assistant(reply) => {
            json!({"role": "assistant", "content": reply.continuation})
        }
        Message::ToolResults(results) => {
            let results: Vec<Value> = results
                .iter()
                .map(|result| {
                    json!({
                        "type": "tool_result",
                        "tool_use_id": result.tool_call_id,
                        "content": result.content,
                        "is_error": result.is_error,
                    })
                })
                .collect();
            // Anthropic carries tool results inside a user message.
            json!({"role": "user", "content": results})
        }
    }
}

// Pi-style explicit anchors. Only this fresh wire value is changed; borrowed
// schemas and opaque assistant continuation remain untouched. Cache lifetime is
// the provider's default; no ttl field, model-name gate or root breakpoint.
fn apply_prompt_cache(body: &mut Value) {
    // Count only actual wire blocks, not a coincidental cache_control key inside
    // tool arguments/results. Existing controls remain unchanged.
    let tools = body["tools"].as_array().into_iter().flatten();
    let content = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["content"].as_array().into_iter().flatten());
    let existing = tools
        .chain(content)
        .filter(|block| block.get("cache_control").is_some())
        .count();
    let mut slots = 4usize.saturating_sub(existing);

    if let Some(tool) = body["tools"]
        .as_array_mut()
        .and_then(|tools| tools.last_mut())
    {
        mark_cache_block(tool, &mut slots);
    }
    if slots > 0
        && let Some(system) = body["system"].as_str().filter(|text| !text.is_empty())
    {
        body["system"] =
            json!([{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}]);
        slots -= 1;
    }
    // Match Pi's Messages path: inspect only the final user/system message,
    // never scan backward through or add markers to assistant continuation.
    let Some(last) = body["messages"]
        .as_array_mut()
        .and_then(|messages| messages.last_mut())
    else {
        return;
    };
    if !matches!(last["role"].as_str(), Some("user" | "system")) {
        return;
    }
    match &mut last["content"] {
        Value::String(text) if slots > 0 && !text.is_empty() => {
            last["content"] =
                json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]);
        }
        Value::Array(blocks) => {
            if let Some(block) = blocks.last_mut()
                && matches!(
                    block["type"].as_str(),
                    Some("text" | "image" | "tool_result" | "tool_addition" | "tool_removal")
                )
            {
                mark_cache_block(block, &mut slots);
            }
        }
        _ => {}
    }
}

fn mark_cache_block(block: &mut Value, slots: &mut usize) {
    if *slots > 0
        && let Some(fields) = block.as_object_mut()
        && !fields.contains_key("cache_control")
    {
        fields.insert("cache_control".into(), json!({"type": "ephemeral"}));
        *slots -= 1;
    }
}

#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod prompt_cache_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolResult;

    #[test]
    fn response_metadata_preserves_optional_facts_without_content() {
        let body = json!({
            "id": "response-1", "model": "served/model", "stop_reason": "max_tokens",
            "usage": {"input_tokens": 12, "output_tokens": 0, "cache_read_input_tokens": 7,
                "cache_creation_input_tokens": 5,
                "cache_creation": {"ephemeral_5m_input_tokens": 2, "ephemeral_1h_input_tokens": 3},
                "future_private_field": "secret"},
            "content": [
                {"type": "text", "text": "é"}, {"type": "text", "text": ""},
                {"type": "thinking", "thinking": "secret", "signature": "secret"},
                {"type": "redacted_thinking", "data": "secret"},
                {"type": "tool_use", "id": "call", "name": "bash", "input": {"command": "secret"}},
                {"type": "future_secret", "data": "secret"}
            ]
        });
        let metadata = serde_json::to_value(AnthropicMessages.response_metadata(&body)).unwrap();
        let reply = AnthropicMessages.decode(body.clone()).unwrap();
        assert_eq!(
            metadata,
            json!({
                "response_id": "response-1", "response_model": "served/model", "stop_reason": "max_tokens",
                "usage": {"input_tokens": 12, "output_tokens": 0, "cache_read_input_tokens": 7,
                    "cache_creation_input_tokens": 5, "cache_creation_5m_input_tokens": 2, "cache_creation_1h_input_tokens": 3},
                "blocks": {"text": 2, "thinking": 1, "redacted_thinking": 1, "tool_use": 1, "unknown": 1},
                "text_bytes": 2
            })
        );
        assert!(!metadata.to_string().contains("secret"));
        let encoded = AnthropicMessages.encode(
            "test",
            "",
            &[Message::Assistant(reply)],
            &[],
            PromptCache::Short,
        );
        assert_eq!(encoded.body["messages"][0]["content"], body["content"]);
    }

    #[test]
    fn absent_malformed_and_future_metadata_do_not_change_reply_acceptance() {
        for extras in [
            json!({}),
            json!({"id": 4, "model": false, "stop_reason": [], "usage": "bad"}),
        ] {
            let mut body = extras;
            body["content"] = json!([]);
            let metadata = AnthropicMessages.response_metadata(&body);
            let reply = AnthropicMessages.decode(body).unwrap();
            assert!(metadata.response_id.is_none());
            assert!(metadata.response_model.is_none());
            assert!(metadata.stop_reason.is_none());
            assert!(metadata.usage.is_none());
            assert_eq!(reply.text(), "");
        }
        let body = json!({"content": [], "stop_reason": "future_secret",
            "usage": {"input_tokens": -1, "output_tokens": "secret"}});
        let metadata = AnthropicMessages.response_metadata(&body);
        assert!(AnthropicMessages.decode(body).is_ok());
        assert_eq!(metadata.stop_reason, Some(StopReason::Unknown));
        assert_eq!(serde_json::to_value(metadata.usage).unwrap(), json!({}));
    }

    #[test]
    fn encodes_system_tools_and_user_text_without_provider_details() {
        let tools = vec![json!({"name": "read_file", "input_schema": {"type": "object"}})];
        let encoded = AnthropicMessages.encode(
            "test/model",
            "instructions",
            &[Message::User("hello".into())],
            &tools,
            PromptCache::Short,
        );
        assert_eq!(encoded.path, "/messages");
        assert_eq!(encoded.headers["anthropic-version"], "2023-06-01");
        assert!(!encoded.headers.contains_key("authorization"));
        assert!(!encoded.headers.contains_key("x-openrouter-cache"));
        assert_eq!(
            encoded.body,
            json!({
                "model": "test/model", "max_tokens": 8000,
                "system": [{"type": "text", "text": "instructions", "cache_control": {"type": "ephemeral"}}],
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}
                ]}],
                "tools": [{"name": "read_file", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}}],
            })
        );
    }

    #[test]
    fn prompt_cache_is_not_gated_by_model_name() {
        for model in [
            "anthropic/claude-haiku-5.5",
            "anthropic/claude-sonnet-5",
            "deepseek/test",
            "unknown",
        ] {
            let body = AnthropicMessages
                .encode(
                    model,
                    "system",
                    &[Message::User("hello".into())],
                    &[],
                    PromptCache::Short,
                )
                .body;
            assert_eq!(
                body["system"][0]["cache_control"],
                json!({"type": "ephemeral"}),
                "{model}"
            );
            assert_eq!(
                body["messages"][0]["content"][0]["cache_control"],
                json!({"type": "ephemeral"}),
                "{model}"
            );
            assert!(body.get("cache_control").is_none());
        }
    }

    #[test]
    fn normalizes_actions_but_round_trips_all_assistant_blocks_losslessly() {
        let blocks = json!([
            {"type": "thinking", "thinking": "private", "signature": "signed"},
            {"type": "text", "text": "First", "citations": [{"extra": true}]},
            {"type": "tool_use", "id": "call-1", "name": "read_file", "input": {"path": "Cargo.toml"}, "extra": 1},
            {"type": "future_block", "unknown": [1, 2, 3]},
            {"type": "text", "text": "Second"}
        ]);
        let reply = AnthropicMessages
            .decode(json!({"content": blocks}))
            .unwrap();
        assert_eq!(reply.text(), "First\nSecond");
        assert_eq!(reply.output.len(), 3);
        let Output::ToolCall(call) = &reply.output[1] else {
            panic!("expected a tool call")
        };
        assert_eq!(call.id, "call-1");
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments, json!({"path": "Cargo.toml"}));
        let encoded = AnthropicMessages.encode(
            "test",
            "",
            &[Message::Assistant(reply)],
            &[],
            PromptCache::Short,
        );
        assert_eq!(encoded.body["messages"][0]["content"], blocks);
    }

    #[test]
    fn encodes_multiple_tool_results_with_ids_and_errors() {
        let encoded = AnthropicMessages.encode(
            "test",
            "",
            &[Message::ToolResults(vec![
                ToolResult {
                    tool_call_id: "a".into(),
                    content: "done".into(),
                    is_error: false,
                },
                ToolResult {
                    tool_call_id: "b".into(),
                    content: "denied".into(),
                    is_error: true,
                },
            ])],
            &[],
            PromptCache::Short,
        );
        assert_eq!(
            encoded.body["messages"],
            json!([{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "a", "content": "done", "is_error": false},
                {"type": "tool_result", "tool_use_id": "b", "content": "denied", "is_error": true, "cache_control": {"type": "ephemeral"}},
            ]}])
        );
    }

    #[test]
    fn malformed_responses_fail_before_the_agent_can_execute_any_tool() {
        for body in [
            json!({}),
            json!({"content": "wrong"}),
            json!({"content": [null]}),
            json!({"content": [42]}),
            json!({"content": ["text"]}),
            json!({"content": [[]]}),
            json!({"content": [{}]}),
            json!({"content": [{"type": null}]}),
            json!({"content": [{"type": 42}]}),
            json!({"content": [{"type": "text"}]}),
            json!({"content": [{"type": "tool_use", "name": "bash", "input": {}}]}),
            json!({"content": [{"type": "tool_use", "id": "a", "input": {}}]}),
            json!({"content": [{"type": "tool_use", "id": "a", "name": "bash", "input": "wrong"}]}),
        ] {
            assert!(AnthropicMessages.decode(body).is_err());
        }
        assert_eq!(
            AnthropicMessages
                .decode(json!({"content": []}))
                .unwrap()
                .text(),
            ""
        );
    }
}
