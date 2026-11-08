//! Controlled HTTP evidence; no authenticated model calls or user session files.
use super::*;
use crate::test_support::{agent, events, log};
use serde_json::json;

const RESPONSE: &str = r#"{"id":"response-1","model":"served/model","stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":3},"content":[{"type":"text","text":"private assistant text"},{"type":"thinking","thinking":"private reasoning","signature":"private signature"},{"type":"future_private_tag","data":"private block"}]}"#;
const EMPTY: &str = r#"{"id":"empty-response","stop_reason":"end_turn","content":[]}"#;

mod responses;

mod validation;

mod errors;

mod retries;

mod lifecycle;
