#![cfg(unix)]

#[path = "session/compaction.rs"]
mod compaction;

use robbie_core::{
    CapturePolicy, EventKind, FrontendEnd, ModelSettings, PromptCache, RecordingSettings,
    SessionConfig, TextAppend, TextContext,
    test_support::{Reply, SUCCESS, Server, events, log},
};

#[tokio::test(flavor = "current_thread")]
async fn public_session_runs_real_tools_and_correlates_frontend_observations() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("example.txt"), "public consumer fixture").unwrap();
    let state = root.path().join("state");
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"read","name":"read_file","input":{"path":"example.txt"}}]}"#,
        ),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, mut activities, _) = server
        .session(SessionConfig {
            skills_directory: workspace.join("skills"),
            workspace,
            model: ModelSettings {
                api_key: "fixture-secret".into(),
                model: "fixture/model".into(),
                upstream: None,
                prompt_cache: PromptCache::Off,
            },
            recording: RecordingSettings {
                state_directory: Some(state.clone()),
                capture: CapturePolicy::MetadataOnly,
            },
        })
        .await
        .unwrap();
    assert_eq!(session.model_name(), "fixture/model");
    assert!(session.recording().take_warning().is_none());
    let prompt = "  read the fixture  ";
    let turn = session.prepare_turn();
    let mut frontend = turn.record_frontend(prompt);
    let returned = turn.run(prompt.trim()).await.unwrap();
    let (mut reads, mut texts) = (0, 0);
    // As in a host, drain queued activity before acknowledging completion.
    while let Ok(activity) = activities.try_recv() {
        match activity.kind {
            EventKind::ToolFinished {
                name,
                output,
                is_error,
            } => {
                assert_eq!(name, "read_file");
                assert_eq!(output, "public consumer fixture");
                assert!(!is_error);
                reads += 1;
            }
            EventKind::AssistantText(text, context) => {
                let context: TextContext = context.expect("real text carries correlation");
                frontend.received(context, &text);
                // This plain source needs no sanitization; append is a host observation.
                frontend.appended(TextAppend {
                    context,
                    sanitized_bytes: text.len(),
                    non_whitespace_chars: text.chars().filter(|ch| !ch.is_whitespace()).count(),
                });
                assert_eq!(text, "done");
                texts += 1;
            }
            _ => {}
        }
    }
    assert_eq!(returned, "done");
    assert_eq!((reads, texts, server.count()), (1, 1, 2));
    frontend.finish(FrontendEnd::Completed {
        returned_text_bytes: returned.len(),
    });
    let records = log(&state);
    let started = events(&records, "ui_turn_started")[0];
    let finished = events(&records, "ui_turn_finished")[0];
    assert_eq!(started["submitted_bytes"], prompt.len());
    assert_eq!(started["turn_id"], finished["turn_id"]);
    assert_eq!(finished["outcome"], "completed");
    assert_eq!(finished["totals"]["root"]["received_blocks"], 1);
    assert_eq!(finished["totals"]["root"]["appended_blocks"], 1);
    let emitted = events(&records, "agent_text_emitted")[0];
    let received = events(&records, "ui_text_received")[0];
    let appended = events(&records, "ui_text_appended")[0];
    for field in ["turn_id", "request_id", "text_sequence", "source"] {
        assert_eq!(emitted[field], received[field]);
        assert_eq!(received[field], appended[field]);
    }
    assert!(records.iter().all(|record| record.get("content").is_none()));
}
