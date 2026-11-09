//! Real model-client exchanges against local HTTP fixtures, never a provider.
use super::*;
use crate::{
    model::{AnthropicMessages, PromptCache, Protocol, ToolResult},
    test_support::{Reply, SUCCESS, Server},
};
use serde_json::{Value, json};
use std::time::Duration;

const SUMMARY: &str = r#"{"id":"summary-1","stop_reason":"end_turn","usage":{"input_tokens":900,"output_tokens":12},"content":[{"type":"text","text":"FIRST_SUMMARY: preserve the goal and continue."}]}"#;
const SECOND: &str = r#"{"id":"summary-2","stop_reason":"end_turn","content":[{"type":"text","text":"SECOND_SUMMARY: earlier constraints plus new progress."}]}"#;

fn assistant(blocks: Value) -> Message {
    Message::Assistant(
        AnthropicMessages
            .decode(json!({"content": blocks}))
            .unwrap(),
    )
}

fn history() -> Vec<Message> {
    vec![
        Message::User(
            "PRIVATE_GOAL: inspect the failure without changing the public interface.".into(),
        ),
        assistant(json!([
            {"type":"thinking","thinking":"PRIVATE_REASONING","signature":"PRIVATE_SIGNATURE"},
            {"type":"future_block","payload":"PRIVATE_UNKNOWN"},
            {"type":"tool_use","id":"A","name":"read_file","input":{"path":"file.rs"}}
        ])),
        Message::ToolResults(vec![ToolResult {
            tool_call_id: "A".into(),
            content: "PRIVATE_OLD_RESULT ".repeat(180),
            is_error: false,
        }]),
        assistant(json!([
            {"type":"thinking","thinking":"TAIL_REASONING","signature":"TAIL_SIGNATURE"},
            {"type":"future_block","nested":{"preserve":true}},
            {"type":"tool_use","id":"B","name":"bash","input":{"command":"printf do-not-replay"}}
        ])),
        Message::ToolResults(vec![ToolResult {
            tool_call_id: "B".into(),
            content: "PRIVATE_TAIL_RESULT 🦀".into(),
            is_error: true,
        }]),
    ]
}

fn wire(messages: &[Message]) -> Value {
    AnthropicMessages
        .encode("test", "normal-system", messages, &[], PromptCache::Off)
        .body["messages"]
        .clone()
}

// Fixture size units only. This is not a production tokenizer or window policy.
fn estimate(message: &Message) -> usize {
    wire(std::slice::from_ref(message))[0]
        .to_string()
        .chars()
        .count()
}

fn limits(messages: &[Message], keep_messages: usize) -> Limits {
    Limits {
        keep_recent_tokens: messages[messages.len() - keep_messages..]
            .iter()
            .map(estimate)
            .sum(),
        max_input_chars: 20_000,
        max_output_tokens: 512,
    }
}

fn bodies(server: &Server) -> Vec<Value> {
    server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap())
        .collect()
}

mod lifecycle;
#[cfg(unix)]
mod recording;
mod requests;
