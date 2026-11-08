use super::*;

#[cfg(unix)]
#[tokio::test]
async fn model_recorder_failure_becomes_one_visible_notice_without_losing_the_draft() {
    use crate::test_support::{Reply, SUCCESS, Server};
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![Reply::Http(200, "", SUCCESS)]);
    let (mut session, _, control) = server
        .session(session_config(root.path(), false))
        .await
        .unwrap();
    let mut app = App::new("test/model");
    app.recorder = Some(session.recording());
    app.handle_event(Event::Paste("my next draft".into()));
    let turn = session.prepare_turn();
    control.fail_writes();
    assert_eq!(turn.run("record a model request").await.unwrap(), "done");
    assert_eq!(server.count(), 1);
    app.recording_warning();
    app.recording_warning();
    let text = history(&mut app);
    assert_eq!(
        text.matches("[warning] Diagnostics recording failed")
            .count(),
        1
    );
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))),
        InputAction::Submit
    );
    assert_eq!(app.content.submit(), "my next draft");
}

#[test]
fn input_paste_quit_and_failed_sessions_use_shared_behavior() {
    let mut app = App::default();
    let key = |code, modifiers| Event::Key(KeyEvent::new(code, modifiers));
    assert_eq!(
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        InputAction::None
    );
    assert_eq!(
        app.handle_event(Event::Paste("hello\n猫\u{3}".into())),
        InputAction::None
    );
    assert_eq!(
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        InputAction::Submit
    );
    assert_eq!(app.content.submit(), "hello 猫");
    app.finish_turn(Err(anyhow::anyhow!("model failed")));
    app.handle_event(Event::Paste("another prompt".into()));
    assert_eq!(
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        InputAction::Submit,
        "a failed model request must not prevent submitting the next prompt"
    );
    assert_eq!(
        app.handle_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        InputAction::Quit
    );
    let mut empty = App::default();
    assert_eq!(
        empty.handle_event(key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        InputAction::Quit
    );
}

#[test]
fn events_strip_control_characters() {
    let mut app = App::default();
    assistant(&mut app, "hello\r\n\n\u{1b}[31m");
    let text = history(&mut app);
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("hello\n"));
}
