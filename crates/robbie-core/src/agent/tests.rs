//! Agent-turn and tool-span behavior through real loopback model exchanges.
use crate::{
    diagnostics::Recorder,
    model::ToolCall,
    test_support::{Reply, SUCCESS, Server},
};
use serde_json::Value;
use std::time::Duration;

mod tools;
mod turns;
