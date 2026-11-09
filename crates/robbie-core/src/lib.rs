//! Native agent sessions, independent of terminal or frontend initialization.
//! Callers own the Tokio runtime, activity consumption, and turn future.
mod agent;
mod diagnostics;
mod events;
mod model;
mod session;
mod skills;
mod tools;
mod usage;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use diagnostics::activity::TextContext;
pub use model::{LimitResolution, LimitSource, ModelLimits};
pub use session::{
    ActivityReceiver, AgentEvent, CapturePolicy, CompactionOptions, CompactionOutcome, EventKind,
    FrontendEnd, FrontendTurn, ModelSettings, PreparedTurn, PromptCache, Recording,
    RecordingSettings, Session, SessionConfig, TextAppend,
};
pub use usage::{ContextUsage, TokenTotal, UsageSnapshot, UsageTotals};

#[cfg(test)]
use agent::Agent;
#[cfg(test)]
use tools::ToolRegistry;
