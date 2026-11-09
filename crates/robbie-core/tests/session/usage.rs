//! Public-consumer accounting, independent of recording or footer rendering.
use super::*;
use robbie_core::{
    ActivityReceiver, CompactionOptions, ContextUsage, Session, TokenTotal, UsageSnapshot,
};
use std::{path::Path, time::Duration};

const NORMAL: &str = r#"{"content":[{"type":"text","text":"done"}],"usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":10}}"#;
const PARTIAL: &str =
    r#"{"content":[{"type":"text","text":"done"}],"usage":{"input_tokens":5,"output_tokens":2}}"#;
const ZERO: &str = r#"{"content":[{"type":"text","text":"done"}],"usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#;
const SUMMARY: &str = r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"Summary of earlier goals."}],"usage":{"input_tokens":900,"output_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#;
const REJECTED: &str = r#"{"stop_reason":"end_turn","content":[{"type":"text","text":" "}],"usage":{"input_tokens":900,"output_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#;

async fn connect(
    server: &Server,
    root: &Path,
    recording: bool,
) -> (
    Session,
    ActivityReceiver,
    robbie_core::test_support::RecordingControl,
) {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("fixture.txt"), "fixture contents").unwrap();
    server
        .session(SessionConfig {
            workspace,
            skills_directory: "skills".into(),
            model: ModelSettings {
                api_key: "fixture-not-real".into(),
                model: "fixture/model".into(),
                upstream: None,
                prompt_cache: PromptCache::Off,
            },
            recording: RecordingSettings {
                state_directory: recording.then(|| root.join("state")),
                capture: CapturePolicy::MetadataOnly,
            },
        })
        .await
        .unwrap()
}

fn updates(receiver: &mut ActivityReceiver) -> Vec<(&'static str, UsageSnapshot)> {
    let mut snapshots = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let EventKind::UsageUpdated(snapshot) = event.kind {
            snapshots.push((event.source, snapshot));
        }
    }
    snapshots
}

fn options() -> CompactionOptions {
    CompactionOptions {
        keep_recent_tokens: 1,
        max_input_chars: 30_000,
        max_output_tokens: 512,
    }
}

async fn seed(session: &mut Session) {
    session
        .prepare_turn()
        .run(&"old goal and constraints ".repeat(200))
        .await
        .unwrap();
    session.prepare_turn().run("recent goal").await.unwrap();
}

#[path = "usage/accounting.rs"]
mod accounting;
#[path = "usage/context.rs"]
mod context;
