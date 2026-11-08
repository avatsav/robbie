//! A headless agent with an intentionally closed frontend channel.
use std::sync::Arc;

use crate::{Agent, ToolRegistry, model::ModelClient};

pub(crate) fn agent(model: Arc<ModelClient>) -> Agent {
    // Handled text must not be mistaken for UI receipt.
    let (events, _) = tokio::sync::mpsc::unbounded_channel();
    Agent::new(model, ToolRegistry::new(), events, "main")
}
