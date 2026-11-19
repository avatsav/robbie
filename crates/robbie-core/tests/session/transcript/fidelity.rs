use super::*;
use robbie_core::{CompactionOptions, CompactionOutcome};

const TOOL: &str = r#"{"stop_reason":"tool_use","content":[{"type":"text","text":"BEFORE_TOOL"},{"type":"thinking","thinking":"PRIVATE_THINKING","signature":"OPAQUE_SIGNATURE"},{"type":"future_block","nested":{"preserve":true}},{"type":"tool_use","id":"read","name":"read_file","input":{"path":"example.txt"}},{"type":"text","text":"AFTER_TOOL"}]}"#;
const TAIL: &str = r#"{"stop_reason":"end_turn","content":[{"type":"redacted_thinking","data":"OPAQUE_REDACTED"},{"type":"text","text":"first block","extra":"EXTRA_FIELD"},{"type":"future_block","nested":[1,2]},{"type":"text","text":"second block"}]}"#;

#[tokio::test]
async fn export_preserves_wire_blocks_order_and_complete_tool_results_without_auth() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![Reply::Http(200, "", TOOL), Reply::Http(200, "", TAIL)]);
    let mut session = session(&server, root.path(), false).await;
    let contents = "FULL_TOOL_CONTENT\n".repeat(1_000);
    fs::write(root.path().join("workspace/example.txt"), &contents).unwrap();
    session
        .prepare_turn()
        .run("  read the fixture  ")
        .await
        .unwrap();
    let before = session.statistics();
    let dump = snapshot(&session);
    assert_eq!(
        session.statistics(),
        before,
        "export is not inference or accounting"
    );
    assert_eq!(server.count(), 2);
    let request = &dump["request"];
    assert_eq!(request["model"], "fixture/model");
    assert!(request["system"].as_str().unwrap().contains("You are"));
    assert!(!request["tools"].as_array().unwrap().is_empty());
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages[0],
        json!({"role":"user","content":"  read the fixture  "})
    );
    assert_eq!(
        messages[1]["content"],
        serde_json::from_str::<Value>(TOOL).unwrap()["content"]
    );
    assert_eq!(messages[2]["content"][0]["content"], contents);
    assert_eq!(
        messages[3]["content"],
        serde_json::from_str::<Value>(TAIL).unwrap()["content"]
    );
    assert!(request.get("headers").is_none());
    assert!(request.get("url").is_none());
    assert!(
        !dump
            .to_string()
            .contains("PRIVATE_EXPORT_CONNECTION_SECRET")
    );
    assert_eq!(
        dump["workspace"],
        root.path().join("workspace").display().to_string()
    );
    assert!(dump["dumped_at_ms"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn export_uses_the_sessions_cache_policy_and_output_cap_without_inference() {
    for cache in [PromptCache::Off, PromptCache::Short] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![]);
        let mut settings = config(root.path(), false);
        settings.model.prompt_cache = cache;
        let (mut session, _, _) = server.session(settings).await.unwrap();
        session
            .resolve_model_limits(Some(robbie_core::ModelLimits {
                context_window: 32_768,
                max_input_tokens: None,
                max_output_tokens: Some(512),
            }))
            .await
            .unwrap();
        let before = session.statistics();
        let dump = snapshot(&session);
        assert_eq!(dump["request"]["max_tokens"], 512);
        assert_eq!(dump["request"]["messages"], json!([]));
        assert_eq!(
            dump.to_string().contains("cache_control"),
            cache == PromptCache::Short
        );
        assert_eq!(session.statistics(), before);
        assert_eq!(server.count(), 0);
    }
}

#[tokio::test]
async fn export_matches_the_next_ordinary_request_history() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", TAIL),
        Reply::Http(200, "", SUCCESS),
    ]);
    let mut settings = config(root.path(), true);
    settings.recording.capture = CapturePolicy::Transcript;
    let (mut session, _, _) = server.session(settings).await.unwrap();
    session.prepare_turn().run("first").await.unwrap();
    let dump = snapshot(&session);
    let records_before = log(&root.path().join("state"));
    session.prepare_turn().run("second").await.unwrap();
    let records = log(&root.path().join("state"));
    let requests = events(&records, "request_started");
    let actual = &requests[1]["content"]["body"];
    let mut expected = dump["request"].clone();
    expected["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"second"}));
    assert_eq!(&expected, actual);
    assert_eq!(events(&records_before, "request_started").len(), 1);
}

#[tokio::test]
async fn compaction_export_contains_the_summary_once_and_the_exact_retained_tail() {
    const SUMMARY: &str =
        r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"SAVED_CHECKPOINT"}]}"#;
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", TAIL),
        Reply::Http(200, "", SUMMARY),
    ]);
    let mut session = session(&server, root.path(), false).await;
    session
        .prepare_turn()
        .run(&"RETIRED_PRIVATE_GOAL ".repeat(400))
        .await
        .unwrap();
    session.prepare_turn().run("recent").await.unwrap();
    assert_eq!(
        session
            .compact(CompactionOptions {
                keep_recent_tokens: 1,
                max_input_chars: 30_000,
                max_output_tokens: 512,
            })
            .await
            .unwrap(),
        CompactionOutcome::Compacted {
            retired_messages: 3,
            retained_messages: 1
        }
    );
    let dump = snapshot(&session);
    let messages = dump["request"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(
        messages[1]["content"],
        serde_json::from_str::<Value>(TAIL).unwrap()["content"]
    );
    assert_eq!(dump.to_string().matches("SAVED_CHECKPOINT").count(), 1);
    assert!(!dump.to_string().contains("RETIRED_PRIVATE_GOAL"));
    assert_eq!(server.count(), 3);
}
