use serde_json::Value;
use tokio::sync::mpsc;

// Activity is separate from conversation history and the model's wire protocol.
#[derive(Debug)]
pub struct AgentEvent {
    pub source: &'static str,
    pub kind: EventKind,
}

#[derive(Debug)]
pub enum EventKind {
    ModelStarted,
    AssistantText(String),
    ToolStarted {
        name: String,
        input: Value,
    },
    ToolFinished {
        name: String,
        output: String,
        is_error: bool,
    },
}

pub type EventSender = mpsc::UnboundedSender<AgentEvent>;
