//! Wire-format details belong here, not in the agent loop or provider.
use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::metadata::{BlockCounts, ResponseMetadata, StopReason, TokenUsage};
use super::{Message, Output, Reply, ToolCall};

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
    ) -> EncodedRequest;

    // Inspection is infallible and does not establish reply validity.
    fn response_metadata(&self, body: &Value) -> ResponseMetadata;

    fn decode(&self, body: Value) -> Result<Reply>;
}

pub struct AnthropicMessages;

impl Protocol for AnthropicMessages {
    fn encode(
        &self,
        model: &str,
        system: &str,
        messages: &[Message],
        tools: &[Value],
    ) -> EncodedRequest {
        let messages: Vec<Value> = messages
            .iter()
            .map(|message| match message {
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
            })
            .collect();
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        EncodedRequest {
            path: "/messages",
            headers,
            body: json!({
                "model": model,
                "max_tokens": 8000,
                "system": system,
                "messages": messages,
                // Existing tool declarations use name/description/input_schema.
                // A later protocol can translate them to its own tool format.
                "tools": tools,
            }),
        }
    }

    fn decode(&self, body: Value) -> Result<Reply> {
        let blocks = body["content"]
            .as_array()
            .context("model response has no content blocks")?;
        let mut output = Vec::new();
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => output.push(Output::Text(
                    block["text"]
                        .as_str()
                        .context("text block has no text")?
                        .to_owned(),
                )),
                Some("tool_use") => output.push(Output::ToolCall(ToolCall {
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
        let encoded = AnthropicMessages.encode("test", "", &[Message::Assistant(reply)], &[]);
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
        );
        assert_eq!(encoded.path, "/messages");
        assert_eq!(encoded.headers["anthropic-version"], "2023-06-01");
        assert!(!encoded.headers.contains_key("authorization"));
        assert!(!encoded.headers.contains_key("x-openrouter-cache"));
        assert_eq!(
            encoded.body,
            json!({
                "model": "test/model", "max_tokens": 8000, "system": "instructions",
                "messages": [{"role": "user", "content": "hello"}], "tools": tools,
            })
        );
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
        let encoded = AnthropicMessages.encode("test", "", &[Message::Assistant(reply)], &[]);
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
        );
        assert_eq!(
            encoded.body["messages"],
            json!([{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "a", "content": "done", "is_error": false},
                {"type": "tool_result", "tool_use_id": "b", "content": "denied", "is_error": true},
            ]}])
        );
    }

    #[test]
    fn malformed_responses_fail_before_the_agent_can_execute_any_tool() {
        for body in [
            json!({}),
            json!({"content": "wrong"}),
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
