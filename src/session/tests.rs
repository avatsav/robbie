use super::*;
use crate::test_support::{Reply, SUCCESS, Server, events, log};
use serde_json::Value;
use std::{fs, path::Path};

fn config(root: &Path) -> SessionConfig {
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join("skills")).unwrap();
    SessionConfig {
        workspace,
        skills_directory: "skills".into(),
        model: ModelSettings {
            api_key: "PRIVATE_CONNECTION_SECRET".into(),
            model: "explicit/model".into(),
            upstream: Some("explicit-upstream".into()),
            prompt_cache: PromptCache::Off,
        },
        recording: RecordingSettings {
            state_directory: Some(root.join("state")),
            ..RecordingSettings::default()
        },
    }
}

#[tokio::test]
async fn normal_construction_uses_explicit_skills_model_and_recording_without_a_frontend() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = config(root.path());
    fs::write(
        settings.workspace.join("skills/SKILL.md"),
        "---\nname: example\ndescription: Explicit skill summary.\n---\nPRIVATE_SKILL_BODY",
    )
    .unwrap();
    settings.recording.capture = CapturePolicy::Transcript;
    let (session, mut activities) = Session::new(settings).await.unwrap();
    assert_eq!(session.model_name(), "explicit/model");
    assert!(activities.try_recv().is_err());
    assert!(session.recording().take_warning().is_none());
    assert!(
        session
            .recording()
            .description()
            .unwrap()
            .contains("SENSITIVE transcript")
    );

    let records = log(&root.path().join("state"));
    assert_eq!(records[0]["event"], "session_started");
    assert_eq!(records[0]["model"], "explicit/model");
    assert_eq!(records[0]["transcript_enabled"], true);
    let context = &events(&records, "session_context")[0]["content"];
    assert!(
        context["system"]
            .as_str()
            .unwrap()
            .contains("- example: Explicit skill summary.")
    );
    assert!(
        !context["system"]
            .as_str()
            .unwrap()
            .contains("PRIVATE_SKILL_BODY")
    );
    let tools = context["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "bash",
            "edit_file",
            "load_skill",
            "read_file",
            "sub_agent",
            "todo",
            "write_file"
        ]
    );
    assert!(events(&records, "request_started").is_empty());
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("PRIVATE_CONNECTION_SECRET")
    );
}

#[tokio::test]
async fn normal_construction_defaults_to_metadata_and_rejects_invalid_skills() {
    let root = tempfile::tempdir().unwrap();
    let (session, _) = Session::new(config(root.path())).await.unwrap();
    assert!(
        session
            .recording()
            .description()
            .unwrap()
            .contains("metadata only")
    );
    let records = log(&root.path().join("state"));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["transcript_enabled"], false);
    assert!(records[0].get("content").is_none());

    let root = tempfile::tempdir().unwrap();
    let settings = config(root.path());
    fs::write(settings.workspace.join("skills/SKILL.md"), "---\nunclosed").unwrap();
    assert!(Session::new(settings).await.is_err());
    assert!(!root.path().join("state").exists());
}

#[tokio::test]
async fn explicit_workspace_is_required_and_recording_failures_are_sanitized_and_nonfatal() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = config(root.path());
    settings.workspace = "relative-workspace".into();
    let error = Session::new(settings).await.err().unwrap();
    assert_eq!(error.to_string(), "workspace must be an absolute path");

    for location in [None, Some("relative-state"), Some("workspace")] {
        let root = tempfile::tempdir().unwrap();
        let mut settings = config(root.path());
        settings.recording.state_directory = location.map(|location| {
            if location == "relative-state" {
                PathBuf::from(location)
            } else {
                root.path().join(location)
            }
        });
        let (session, _) = Session::new(settings).await.unwrap();
        let recording = session.recording();
        assert!(recording.description().is_none());
        let warning = recording.take_warning().unwrap();
        assert!(warning.contains("Diagnostics recording failed and is disabled"));
        assert!(!warning.contains(root.path().to_str().unwrap()));
        assert!(!warning.contains("PRIVATE_CONNECTION_SECRET"));
        assert!(recording.take_warning().is_none());
        assert!(!root.path().join("workspace/robbie").exists());
    }
}

#[tokio::test]
async fn dropping_prepared_turns_or_unpolled_execution_preserves_correlated_cleanup() {
    for as_future in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![]);
        let (mut session, _) = Session::for_test(
            server.model_with_recorder(recorder.clone()),
            root.path().to_owned(),
            recorder,
        );
        let turn = session.prepare_turn();
        let frontend = turn.record_frontend("submitted source");
        if as_future {
            drop(turn.run("execution prompt"));
        } else {
            drop(turn);
        }
        frontend.finish(FrontendEnd::Quit);
        // The exclusive borrow is released, so the session remains usable.
        drop(session.prepare_turn());
        let records = log(root.path());
        assert_eq!(server.count(), 0);
        assert!(events(&records, "request_started").is_empty());
        let starts = events(&records, "agent_turn_started");
        let endings = events(&records, "agent_turn_finished");
        let ui_start = events(&records, "ui_turn_started")[0];
        let ui_end = events(&records, "ui_turn_finished")[0];
        assert_eq!(starts.len(), 2);
        assert_eq!(endings.len(), 2);
        assert_eq!(starts[0]["turn_id"], ui_start["turn_id"]);
        assert_eq!(endings[0]["turn_id"], ui_end["turn_id"]);
        assert_ne!(starts[0]["turn_id"], starts[1]["turn_id"]);
        assert!(endings.iter().all(|end| end["reason"] == "cancelled"));
        assert_eq!(ui_end["outcome"], "quit");
        assert!(ui_start.get("content").is_none());
    }
}

#[tokio::test]
async fn sequential_execution_keeps_history_and_does_not_trim_or_require_a_receiver() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, receiver) = Session::for_test(
        server.model_with_recorder(recorder.clone()),
        root.path().to_owned(),
        recorder,
    );
    drop(receiver);
    for prompt in [" \nfirst prompt\t ", "second prompt"] {
        assert_eq!(session.prepare_turn().run(prompt).await.unwrap(), "done");
    }
    let requests = server.requests.lock().unwrap();
    let bodies: Vec<Value> = requests
        .iter()
        .map(|request| serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap())
        .collect();
    assert_eq!(bodies.len(), 2);
    assert_eq!(
        bodies[0]["messages"][0]["content"][0]["text"],
        " \nfirst prompt\t "
    );
    // The cache anchor moves to the new tail; the earlier user source is unchanged.
    assert_eq!(bodies[1]["messages"][0]["role"], "user");
    assert_eq!(bodies[1]["messages"][0]["content"], " \nfirst prompt\t ");
    assert_eq!(bodies[1]["messages"][1]["content"][0]["text"], "done");
    assert_eq!(
        bodies[1]["messages"][2]["content"][0]["text"],
        "second prompt"
    );
    let records = log(root.path());
    let emitted = events(&records, "agent_text_emitted");
    assert_eq!(emitted.len(), 2);
    assert!(
        emitted
            .iter()
            .all(|event| event["delivery"] == "receiver_closed")
    );
    assert_eq!(events(&records, "agent_turn_finished").len(), 2);
    assert!(events(&records, "ui_text_received").is_empty());
    assert!(events(&records, "ui_text_appended").is_empty());
}
