use super::*;

#[test]
fn backspace_removes_one_displayed_grapheme() {
    for glyph in ["e\u{301}", "👩‍💻", "👍🏽", "1\u{fe0f}\u{20e3}"] {
        let mut app = App {
            input: format!("prefix{glyph}"),
            ..App::default()
        };
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.input, "prefix");
    }
    let mut empty = App::default();
    empty.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Backspace,
        KeyModifiers::NONE,
    )));
    assert!(empty.input.is_empty());
}

#[test]
fn input_actions_and_paste_do_not_accidentally_submit() {
    let mut app = App::default();
    let enter = || Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.handle_event(enter()), InputAction::None);
    assert_eq!(
        app.handle_event(Event::Paste("hello\n猫\u{3}".into())),
        InputAction::None
    );
    assert_eq!(app.input, "hello 猫");
    assert_eq!(app.handle_event(enter()), InputAction::Submit);
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ))),
        InputAction::None
    );
    app.failed = true;
    assert_eq!(app.handle_event(enter()), InputAction::Submit);
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        InputAction::Quit
    );
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL
        ))),
        InputAction::None
    );
    assert_eq!(app.output_mode, ToolOutputMode::Relaxed);
    app.input.clear();
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ))),
        InputAction::Quit
    );
    assert_eq!(
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE
        })),
        InputAction::None
    );
}

#[test]
fn finishing_a_turn_preserves_the_next_draft_and_allows_submission_after_failure() {
    let mut app = App {
        input: "first prompt".into(),
        ..App::default()
    };
    assert_eq!(app.submit(), "first prompt");
    assert!(app.working);
    app.handle_event(Event::Paste("next draft".into()));
    app.finish_turn(Ok("answer".into()));
    assert_eq!(app.input, "next draft");
    assert!(!app.working);
    app.finish_turn(Err(anyhow::anyhow!("request failed")));
    assert!(app.failed);
    assert!(!app.working);
    assert_eq!(app.input, "next draft");
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))),
        InputAction::Submit
    );
    assert_eq!(app.submit(), "next draft");
    assert!(app.working);
    assert!(!app.failed);
    assert!(
        app.pending
            .iter()
            .any(|line| line.text().contains("request failed"))
    );
}

#[test]
fn prompt_fits_tiny_and_normal_viewports() -> Result<()> {
    for (width, height) in [(80, 24), (10, 5), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let mut app = App {
            input: "hello猫".into(),
            ..App::default()
        };
        app.push("hello\u{1b}[31m\r\nworld", Style::new());
        assert!(!app.pending[0].text().contains('\u{1b}'));
        terminal.draw(|frame| app.view(frame))?;
    }
    Ok(())
}
