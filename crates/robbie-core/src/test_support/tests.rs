use super::*;
use crate::session::{
    CapturePolicy, EventKind, ModelSettings, PromptCache, RecordingSettings, SessionConfig,
};
use std::{fs, path::Path};

fn config(root: &Path) -> SessionConfig {
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    SessionConfig {
        skills_directory: workspace.join("skills"),
        workspace,
        model: ModelSettings {
            api_key: "fixture-secret".into(),
            model: "fixture/model".into(),
            upstream: None,
            prompt_cache: PromptCache::Off,
        },
        recording: RecordingSettings {
            state_directory: Some(root.join("state")),
            capture: CapturePolicy::MetadataOnly,
        },
    }
}

#[tokio::test]
async fn loopback_fixture_runs_real_tools_and_activities_through_the_session_interface() {
    let root = tempfile::tempdir().unwrap();
    let settings = config(root.path());
    fs::write(settings.workspace.join("example.txt"), "workspace fixture").unwrap();
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"read","name":"read_file","input":{"path":"example.txt"}}]}"#,
        ),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, mut activities, _) = server.session(settings).await.unwrap();
    assert_eq!(session.model_name(), "fixture/model");
    assert_eq!(
        session
            .prepare_turn()
            .run("read the fixture")
            .await
            .unwrap(),
        "done"
    );
    let mut reads = 0;
    let mut texts = 0;
    while let Ok(activity) = activities.try_recv() {
        match activity.kind {
            EventKind::ToolFinished {
                name,
                output,
                is_error,
            } => {
                assert_eq!(name, "read_file");
                assert_eq!(output, "workspace fixture");
                assert!(!is_error);
                reads += 1;
            }
            EventKind::AssistantText(text, context) => {
                assert_eq!(text, "done");
                assert!(context.is_some());
                texts += 1;
            }
            _ => {}
        }
    }
    assert_eq!((reads, texts, server.count()), (1, 1, 2));
    let records = log(&root.path().join("state"));
    assert_eq!(events(&records, "session_started").len(), 1);
    assert_eq!(events(&records, "tool_execution_finished").len(), 1);
    assert!(records.iter().all(|record| record.get("content").is_none()));
}

#[tokio::test]
async fn opaque_fault_control_warns_once_without_changing_the_turn_result() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![Reply::Http(200, "", SUCCESS)]);
    let (mut session, _, control) = server.session(config(root.path())).await.unwrap();
    let recording = session.recording();
    let turn = session.prepare_turn();
    control.fail_writes();
    assert_eq!(turn.run("complete normally").await.unwrap(), "done");
    assert!(
        recording
            .take_warning()
            .unwrap()
            .contains("Diagnostics recording failed and is disabled")
    );
    assert!(recording.take_warning().is_none());
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn loopback_setup_preserves_normal_session_validation() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![]);
    let mut settings = config(root.path());
    settings.workspace = "relative-workspace".into();
    assert_eq!(
        server.session(settings).await.err().unwrap().to_string(),
        "workspace must be an absolute path"
    );
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn terminal_read_helper_uses_the_injected_workspace_and_reports_real_failures() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("example.txt"), "workspace fixture").unwrap();
    assert_eq!(
        read_file(root.path(), "example.txt").await,
        ("workspace fixture".into(), false)
    );
    let (error, failed) = read_file(root.path(), "missing.txt").await;
    assert!(failed);
    assert!(!error.is_empty());
}
