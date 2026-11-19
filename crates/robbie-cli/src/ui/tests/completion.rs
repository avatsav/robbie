use super::*;

fn key(app: &mut App, code: KeyCode) -> InputAction {
    app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn paste(app: &mut App, text: &str) {
    assert_eq!(
        app.handle_event(Event::Paste(text.into())),
        InputAction::None
    );
}

#[test]
fn typing_slash_opens_suggestions_and_tab_only_fills_the_selected_command() {
    let mut app = App::default();
    let base = app.live_height(100, 30);
    assert_eq!(key(&mut app, KeyCode::Char('/')), InputAction::None);
    assert_eq!(app.live_height(100, 30), base + 6);
    key(&mut app, KeyCode::Down);
    assert_eq!(key(&mut app, KeyCode::Tab), InputAction::None);
    assert_eq!(app.input, "/compact");
    assert_eq!(app.live_height(100, 30), base);
    assert!(app.pending.is_empty());
    assert!(!app.working);
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::Submit);
    assert_eq!(app.submit(), Some(Submission::Compact));
}

#[test]
fn enter_completes_a_prefix_but_submits_an_already_complete_command() {
    let mut app = App::default();
    paste(&mut app, "/co");
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::None);
    assert_eq!(app.input, "/compact");
    assert!(app.pending.is_empty());
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::Submit);
    assert_eq!(app.submit(), Some(Submission::Compact));

    paste(&mut app, "/help");
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::Submit);
    assert!(app.submit().is_none());
    assert!(app.pending.iter().any(|line| line.text() == "commands"));
}

#[test]
fn aliases_complete_and_submit_as_the_alias_typed() {
    for (prefix, expected) in [("/q", "/quit"), ("/qu", "/quit"), ("/e", "/exit")] {
        let mut app = App::default();
        paste(&mut app, prefix);
        assert_eq!(key(&mut app, KeyCode::Tab), InputAction::None);
        assert_eq!(app.input, expected);
    }
    for (prefix, expected) in [("/tr", "/transcript"), ("/du", "/dump")] {
        let mut app = App::default();
        paste(&mut app, prefix);
        assert_eq!(key(&mut app, KeyCode::Tab), InputAction::None);
        assert_eq!(app.input, expected);
    }
    let mut app = App::default();
    paste(&mut app, "/quit");
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::Submit);
    assert_eq!(app.submit(), Some(Submission::Exit));
}

#[test]
fn up_and_down_wrap_and_editing_resets_the_selection_before_filtering() {
    for (arrows, expected) in [
        (vec![KeyCode::Up], "/exit"),
        (vec![KeyCode::Down, KeyCode::Down], "/transcript"),
        (vec![KeyCode::Down, KeyCode::Up], "/help"),
    ] {
        let mut app = App::default();
        paste(&mut app, "/");
        for arrow in arrows {
            key(&mut app, arrow);
        }
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.input, expected);
    }
    let mut app = App::default();
    paste(&mut app, "/");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(
        app.live_height(100, 30),
        App::default().live_height(100, 30) + 3
    );
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.input, "/help");
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.input, "/help", "an edit reopens completion");
}

#[test]
fn escape_dismisses_until_an_actual_draft_edit() {
    let mut app = App::default();
    let base = app.live_height(100, 30);
    paste(&mut app, "/");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.live_height(100, 30), base);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Tab);
    paste(&mut app, "");
    assert_eq!(app.input, "/");
    assert_eq!(app.live_height(100, 30), base);
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.live_height(100, 30), base + 3);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.input, "/help");
}

#[test]
fn suggestions_are_case_sensitive_and_only_for_the_first_command_token() {
    for input in [
        "hello /",
        "/HELP",
        "/unknown",
        "/tmp/file",
        "/help ",
        "/compact extra",
        "/transcript extra",
        "/ co",
    ] {
        let mut app = App::default();
        let base = app.live_height(100, 30);
        paste(&mut app, input);
        assert_eq!(app.live_height(100, 30), base, "{input:?}");
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.input, input);
    }
    let mut app = App::default();
    paste(&mut app, "\u{2003}  /co");
    key(&mut app, KeyCode::Tab);
    assert_eq!(
        app.input, "\u{2003}  /compact",
        "leading whitespace is preserved"
    );
}

#[test]
fn busy_enter_is_ignored_but_tab_can_complete_the_next_draft() {
    let mut app = App::default();
    app.start_compaction();
    paste(&mut app, "/co");
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::None);
    assert_eq!(app.input, "/co");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.input, "/compact");
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::None);
    app.finish_compaction(Ok(CompactionOutcome::Unchanged));
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::Submit);
    assert_eq!(app.submit(), Some(Submission::Compact));
}

#[test]
fn control_shortcuts_alt_keys_and_key_releases_do_not_select_or_accept_suggestions() {
    let mut app = App::default();
    paste(&mut app, "/");
    for event in [
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT)),
        Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Down,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
    ] {
        assert_eq!(app.handle_event(event), InputAction::None);
    }
    assert_eq!(app.output_mode, ToolOutputMode::Relaxed);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.input, "/help");
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        InputAction::Quit
    );
}

#[test]
fn completing_a_command_moves_the_cursor_after_the_command() {
    let mut app = App::default();
    paste(&mut app, "/co");
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Left);
    assert_eq!(key(&mut app, KeyCode::Tab), InputAction::None);
    assert_eq!(app.input, "/compact");
    assert_eq!(app.input.cursor(), app.input.len());
    assert_eq!(key(&mut app, KeyCode::Enter), InputAction::Submit);
    assert_eq!(app.submit(), Some(Submission::Compact));
}
