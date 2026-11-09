//! The agent's conversation, independent of HTTP roles and tool-block spelling.
use serde_json::Value;

#[derive(Clone, Debug)]
pub enum Message {
    User(String),
    Assistant(Reply),
    ToolResults(Vec<ToolResult>),
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone, Debug)]
pub enum Output {
    Text(String),
    ToolCall(ToolCall),
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub output: Vec<Output>,
    // Protocol-owned continuation data, not a second source for agent actions.
    // Retain entire blocks (including signatures and unknown fields). A
    // conversation stays on its original protocol; history is not translated.
    pub(super) continuation: Vec<Value>,
}

/// The latest compaction summary is ordinary user-role historical context, not
/// an assistant response or resumable session snapshot.
pub(crate) fn summary_message(summary: &str) -> Message {
    Message::User(format!(
        "The earlier conversation was compacted into the following summary. Use it as historical context and continue from the retained messages.\n\n<summary>\n{summary}\n</summary>"
    ))
}

impl Reply {
    pub fn text(&self) -> String {
        self.output
            .iter()
            .filter_map(|output| match output {
                Output::Text(text) => Some(text.as_str()),
                Output::ToolCall(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
