use super::*;
use crate::test_support::{Reply, SUCCESS, Server, events, log};

mod cancellation;
mod compaction;

const SUMMARY: &str = r#"{"stop_reason":"end_turn","usage":{"input_tokens":900,"output_tokens":12},"content":[{"type":"text","text":"SAVED_CHECKPOINT"}]}"#;
const BAD_SUMMARY: &str = r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"  "}]}"#;

async fn known_session(
    server: &Server,
    root: &std::path::Path,
    transcript: bool,
) -> (Session, UnboundedReceiver<AgentEvent>) {
    let (mut session, receiver, _) = server
        .session(session_config(root, transcript))
        .await
        .unwrap();
    session
        .resolve_model_limits(Some(crate::session::ModelLimits {
            context_window: 32_768,
            max_input_tokens: None,
            max_output_tokens: Some(512),
        }))
        .await
        .unwrap();
    session
        .configure_auto_compaction(crate::session::CompactionPolicy {
            enabled: false,
            ..crate::session::CompactionPolicy::default()
        })
        .unwrap();
    (session, receiver)
}

fn consume(app: &mut App, receiver: &mut UnboundedReceiver<AgentEvent>) {
    while let Ok(event) = receiver.try_recv() {
        app.activity(event);
    }
}

async fn ordinary_prompt(
    app: &mut App,
    session: &mut Session,
    receiver: &mut UnboundedReceiver<AgentEvent>,
    prompt: &str,
) {
    app.handle_event(Event::Paste(prompt.into()));
    let Some(Submission::Prompt(prompt)) = app.content.submit() else {
        panic!("expected prompt")
    };
    let turn = session.prepare_turn();
    app.ui_turn = Some(turn.record_frontend(&prompt));
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn.run(&prompt))
        .await
        .unwrap();
    assert_eq!(result.as_deref().unwrap(), "done");
    consume(app, receiver);
    app.finish_turn(result);
}

async fn seed(app: &mut App, session: &mut Session, receiver: &mut UnboundedReceiver<AgentEvent>) {
    ordinary_prompt(app, session, receiver, &"OLD_PRIVATE_MARKER ".repeat(256)).await;
    // Exceeds the 20K recent target, leaving the first exchange eligible.
    ordinary_prompt(
        app,
        session,
        receiver,
        &"RECENT_PRIVATE_MARKER ".repeat(4_000),
    )
    .await;
}

fn submit_compact(app: &mut App) {
    app.handle_event(Event::Paste("/compact".into()));
    assert_eq!(app.content.submit(), Some(Submission::Compact));
    assert!(app.ui_turn.is_none());
    app.content.start_compaction();
}

fn request_bodies(root: &std::path::Path) -> Vec<serde_json::Value> {
    let records = log(root);
    events(&records, "request_started")
        .iter()
        .map(|event| event["content"]["body"].clone())
        .collect()
}

#[tokio::test]
async fn autocomplete_acceptance_does_not_prepare_a_turn_or_execute_compaction() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![]);
    let (session, _, _) = server
        .session(session_config(root.path(), true))
        .await
        .unwrap();
    let mut app = App {
        recorder: Some(session.recording()),
        ..App::default()
    };
    for code in [KeyCode::Char('/'), KeyCode::Down, KeyCode::Enter] {
        assert_eq!(
            app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE))),
            InputAction::None
        );
    }
    assert!(app.ui_turn.is_none());
    assert!(
        app.rows(100).is_empty(),
        "suggestions do not enter the transcript"
    );
    assert_eq!(server.count(), 0);
    let records = log(root.path());
    for event in [
        "agent_turn_started",
        "ui_turn_started",
        "compaction_started",
        "request_started",
    ] {
        assert!(events(&records, event).is_empty(), "{event}");
    }
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))),
        InputAction::Submit
    );
    assert_eq!(app.content.submit(), Some(Submission::Compact));
}

#[test]
fn transcript_command_dispatches_locally_without_a_turn() {
    let mut app = App::default();
    app.handle_event(Event::Paste("/transcript".into()));
    assert_eq!(app.content.submit(), Some(Submission::Transcript));
    assert!(app.ui_turn.is_none());
}

#[test]
fn exit_command_dispatches_locally_without_a_turn() {
    let mut app = App::default();
    app.handle_event(Event::Paste("/quit".into()));
    assert_eq!(app.content.submit(), Some(Submission::Exit));
    assert!(app.ui_turn.is_none());
}

#[tokio::test]
async fn local_commands_stay_out_of_agent_turns_recording_and_conversation_history() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::responding(|request| {
            let (_, body) = request.split_once("\r\n\r\n").unwrap();
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            let messages = body["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 1, "local commands must not enter history");
            assert_eq!(messages[0]["role"], "user");
            assert_eq!(messages[0]["content"][0]["text"], "ordinary prompt");
            Reply::Http(200, "", SUCCESS)
        });
        let (mut session, mut receiver, _) = server
            .session(session_config(root.path(), transcript))
            .await
            .unwrap();
        let mut app = App {
            recorder: Some(session.recording()),
            ..App::default()
        };
        for command in [
            "/help",
            "/help extra",
            "/unknown",
            "/transcript extra",
            "/exit extra",
        ] {
            app.handle_event(Event::Paste(command.into()));
            assert!(app.content.submit().is_none());
            assert!(app.ui_turn.is_none());
        }
        assert_eq!(server.count(), 0);
        let records = log(root.path());
        for event in ["agent_turn_started", "ui_turn_started", "request_started"] {
            assert!(events(&records, event).is_empty(), "{event}");
        }
        let text = history(&mut app);
        assert!(
            text.lines()
                .any(|line| line.trim_start().starts_with("/help ")
                    && line.contains("show this help"))
        );
        assert!(text.contains("[error] Unknown command."));

        // Use the same ordinary-prompt path as the host after the local commands.
        app.handle_event(Event::Paste("ordinary prompt".into()));
        let Some(Submission::Prompt(prompt)) = app.content.submit() else {
            panic!("expected ordinary prompt");
        };
        let turn = session.prepare_turn();
        app.ui_turn = Some(turn.record_frontend(&prompt));
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn.run(&prompt))
            .await
            .expect("loopback request timed out");
        while let Ok(event) = receiver.try_recv() {
            app.activity(event);
        }
        assert_eq!(result.as_deref().unwrap(), "done");
        app.finish_turn(result);
        assert_eq!(server.count(), 1);
        let records = log(root.path());
        for event in [
            "agent_turn_started",
            "ui_turn_started",
            "request_started",
            "ui_turn_finished",
        ] {
            assert_eq!(events(&records, event).len(), 1, "{event}");
        }
        assert!(history(&mut app).contains("done"));
    }
}
