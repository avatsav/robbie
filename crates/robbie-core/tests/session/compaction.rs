//! Public-consumer compaction contract through the real session and HTTP path.
use super::*;
use robbie_core::{ActivityReceiver, CompactionOptions, CompactionOutcome, Session};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const TAIL: &str = r#"{"content":[{"type":"thinking","thinking":"TAIL_REASONING","signature":"TAIL_SIGNATURE"},{"type":"future_block","nested":{"preserve":true}},{"type":"text","text":"RECENT_REPLY"}]}"#;
const SUMMARY: &str = r#"{"id":"summary-1","stop_reason":"end_turn","usage":{"input_tokens":900,"output_tokens":12},"content":[{"type":"text","text":"FIRST_COMPACTION_SUMMARY: preserve the goal and continue."}]}"#;
const SECOND: &str = r#"{"id":"summary-2","stop_reason":"end_turn","content":[{"type":"text","text":"SECOND_COMPACTION_SUMMARY: previous constraints plus later progress."}]}"#;
const BAD_SUMMARY: &str =
    r#"{"stop_reason":"end_turn","content":[{"type":"text","text":" \n\t "}]}"#;

fn options() -> CompactionOptions {
    CompactionOptions {
        keep_recent_tokens: 1,
        max_input_chars: 30_000,
        max_output_tokens: 512,
    }
}

async fn session(
    server: &Server,
    root: &Path,
    capture: CapturePolicy,
) -> (
    Session,
    ActivityReceiver,
    robbie_core::test_support::RecordingControl,
) {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(workspace.join("skills")).unwrap();
    server
        .session(SessionConfig {
            skills_directory: "skills".into(),
            workspace,
            model: ModelSettings {
                api_key: "fixture-secret".into(),
                model: "fixture/model".into(),
                upstream: Some("fixture-upstream".into()),
                prompt_cache: PromptCache::Off,
            },
            recording: RecordingSettings {
                state_directory: Some(root.join("state")),
                capture,
            },
        })
        .await
        .unwrap()
}

async fn seed(session: &mut Session) {
    let old = "PRIVATE_OLD_PROMPT preserve the public interface. ".repeat(160);
    assert_eq!(session.prepare_turn().run(&old).await.unwrap(), "done");
    assert_eq!(
        session.prepare_turn().run("RECENT_GOAL").await.unwrap(),
        "RECENT_REPLY"
    );
}

fn request_bodies(root: &Path) -> Vec<Value> {
    let records = log(&root.join("state"));
    events(&records, "request_started")
        .iter()
        .map(|event| event["content"]["body"].clone())
        .collect()
}

#[path = "compaction/lifecycle.rs"]
mod lifecycle;
#[path = "compaction/recording.rs"]
mod recording;
#[path = "compaction/requests.rs"]
mod requests;
