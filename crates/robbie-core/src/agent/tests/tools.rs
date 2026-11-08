//! Tool execution and real delegation evidence, without live provider calls.
use super::*;
use crate::test_support::{agent, events, log};
use serde_json::json;

mod executions;

mod delegation;
