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

// The console is our first event consumer; Ratatui will replace this renderer.
pub fn render_console(event: AgentEvent) {
    let source = event.source;
    match event.kind {
        EventKind::ModelStarted => println!("[{source}] Waiting for model…"),
        EventKind::AssistantText(text) => println!("[{source}] {text}"),
        EventKind::ToolStarted { name, input } => {
            println!("[{source}] Running {name}: {input}");
        }
        EventKind::ToolFinished {
            name,
            output,
            is_error,
        } => {
            let status = if is_error { "error" } else { "done" };
            println!("[{source}] {name} ({status})\n{output}");
        }
    }
}
