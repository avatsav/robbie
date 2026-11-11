//! Real session/HTTP/tool seams; no inference or external services.
use super::*;
use robbie_core::{ActivityReceiver, CompactionPolicy, ModelLimits, Session};
use serde_json::{Value, json};
use std::path::Path;

fn policy() -> CompactionPolicy {
    CompactionPolicy {
        keep_recent_tokens: 100,
        ..CompactionPolicy::default()
    }
}
fn limits() -> ModelLimits {
    ModelLimits {
        context_window: 64000,
        max_input_tokens: None,
        max_output_tokens: Some(8000),
    }
}
fn old_prompt() -> String {
    "OLD_GOAL ".repeat(18000)
}
fn new_prompt() -> String {
    "NEW_GOAL ".repeat(1200)
}

fn response(content: Value, input: u64, stop: &str) -> Reply {
    let body = json!({"stop_reason":stop,"content":content,"usage":{"input_tokens":input,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}});
    Reply::Http(200, "", Box::leak(body.to_string().into_boxed_str()))
}
fn done(input: u64) -> Reply {
    response(json!([{"type":"text","text":"done"}]), input, "end_turn")
}
fn summary() -> Reply {
    response(
        json!([{"type":"text","text":"AUTO_SUMMARY preserves earlier goals and constraints."}]),
        45000,
        "end_turn",
    )
}
fn tool(input: u64) -> Reply {
    response(
        json!([{"type":"tool_use","id":"read","name":"read_file","input":{"path":"large.txt"}}]),
        input,
        "tool_use",
    )
}

async fn connect(
    server: &Server,
    root: &Path,
    selected: ModelLimits,
    policy: CompactionPolicy,
) -> (Session, ActivityReceiver) {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("large.txt"), "x".repeat(12000)).unwrap();
    let (mut session, activity, _) = server
        .session(SessionConfig {
            workspace,
            skills_directory: "skills".into(),
            model: ModelSettings {
                api_key: "fixture-secret".into(),
                model: "fixture/model".into(),
                upstream: Some("fixture".into()),
                prompt_cache: PromptCache::Off,
            },
            recording: RecordingSettings {
                state_directory: Some(root.join("state")),
                capture: CapturePolicy::Transcript,
            },
        })
        .await
        .unwrap();
    session.resolve_model_limits(Some(selected)).await.unwrap();
    session.configure_auto_compaction(policy).unwrap();
    (session, activity)
}

fn bodies(records: &[Value]) -> Vec<Value> {
    events(records, "request_started")
        .iter()
        .map(|event| event["content"]["body"].clone())
        .collect()
}

#[path = "automatic/requests.rs"]
mod requests;
#[path = "automatic/safety.rs"]
mod safety;
