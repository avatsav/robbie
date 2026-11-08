//! Native agent sessions, independent of terminal or frontend initialization.
//! Callers own the Tokio runtime, activity consumption, and turn future.
mod agent;
mod diagnostics;
mod events;
mod model;
mod session;
mod skills;
mod tools;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use diagnostics::activity::TextContext;
pub use session::{
    ActivityReceiver, AgentEvent, CapturePolicy, EventKind, FrontendEnd, FrontendTurn,
    ModelSettings, PreparedTurn, PromptCache, Recording, RecordingSettings, Session, SessionConfig,
    TextAppend,
};

#[cfg(test)]
use agent::Agent;
#[cfg(test)]
use tools::ToolRegistry;
