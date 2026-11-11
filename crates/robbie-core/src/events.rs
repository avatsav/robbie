use crate::diagnostics::activity::TextContext;
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
    /// Session totals plus the emitting conversation's reported context baseline.
    /// Observational only: never assistant text or a signal to start a turn.
    UsageUpdated(crate::UsageSnapshot),
    /// Bounded operational warning, never model-generated text or raw response data.
    Warning(&'static str),
    ModelStarted,
    // Real agent text always carries immutable correlation. None permits
    // unrecorded frontend fixtures without inventing model/turn identities.
    AssistantText(String, Option<TextContext>),
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
