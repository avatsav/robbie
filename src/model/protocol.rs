//! Wire-format details belong here, not in the agent loop or provider.
use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolResult;

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
