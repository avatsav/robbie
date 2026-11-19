use super::*;

fn key(app: &mut App, code: KeyCode) -> InputAction {
    app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[test]
fn backspace_removes_one_displayed_grapheme() {
    for glyph in ["e\u{301}", "👩‍💻", "👍🏽", "1\u{fe0f}\u{20e3}"] {
        let mut app = App {
            input: format!("prefix{glyph}").into(),
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
    app.input = "".into();
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
    assert_eq!(
        app.submit(),
        Some(Submission::Prompt("first prompt".into()))
    );
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
    assert_eq!(app.submit(), Some(Submission::Prompt("next draft".into())));
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

#[test]
fn arrows_move_by_grapheme_and_edits_use_the_cursor() {
    let mut app = App::default();
    app.handle_event(Event::Paste("ab猫👩‍💻".into()));
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Left);
    assert_eq!(app.input.cursor(), 2);

    key(&mut app, KeyCode::Char('X'));
    assert_eq!(app.input, "abX猫👩‍💻");
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.input, "ab猫👩‍💻");
    assert_eq!(app.input.cursor(), 2);

    key(&mut app, KeyCode::Delete);
    assert_eq!(app.input, "ab👩‍💻");
    assert_eq!(app.input.cursor(), 2);
    key(&mut app, KeyCode::Right);
    assert_eq!(app.input.cursor(), app.input.len());
    key(&mut app, KeyCode::Right);
    assert_eq!(app.input.cursor(), app.input.len());

    app.input = "".into();
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Delete] {
        assert_eq!(key(&mut app, code), InputAction::None);
    }
    assert_eq!(app.input, "");
}

#[test]
fn editing_and_pasting_happen_at_the_cursor() {
    let mut app = App {
        input: "ad".into(),
        ..App::default()
    };
    key(&mut app, KeyCode::Left);
    app.handle_event(Event::Paste("bc".into()));
    assert_eq!(app.input, "abcd");
    assert_eq!(app.input.cursor(), 3);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.submit(), Some(Submission::Prompt("abcd".into())));
}

#[test]
fn up_and_down_move_between_wrapped_rows_and_clamp_the_column() {
    let mut app = App {
        input: "abcdefghij".into(),
        ..App::default()
    };
    app.draft_width.set(4);
    // Rows are "abcd", "efgh", "ij"; the cursor starts at the end row.
    key(&mut app, KeyCode::Up);
    assert_eq!(app.input.cursor(), 6);
    key(&mut app, KeyCode::Up);
    assert_eq!(app.input.cursor(), 2);
    key(&mut app, KeyCode::Up);
    assert_eq!(app.input.cursor(), 2, "the top row keeps the cursor");
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.input.cursor(), 7);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.input.cursor(), 10, "a short row clamps to its end");
    key(&mut app, KeyCode::Down);
    assert_eq!(app.input.cursor(), 10, "the bottom row keeps the cursor");
}

#[test]
fn rendering_places_the_cursor_at_its_wrapped_position() -> Result<()> {
    let mut app = App {
        input: "abcdefghijklmnopqrst".into(),
        ..App::default()
    };
    app.mascot.toggle_visibility();
    let mut terminal = Terminal::new(TestBackend::new(10, 12))?;
    terminal.draw(|frame| app.view(frame))?;
    let end = terminal.get_cursor_position()?;
    for _ in 0..4 {
        key(&mut app, KeyCode::Left);
    }
    terminal.draw(|frame| app.view(frame))?;
    let middle = terminal.get_cursor_position()?;
    assert_eq!(middle.x, 6);
    assert_eq!(middle.y + 1, end.y);
    for _ in 0..16 {
        key(&mut app, KeyCode::Left);
    }
    terminal.draw(|frame| app.view(frame))?;
    let start = terminal.get_cursor_position()?;
    assert_eq!(start.x, 0);
    assert_eq!(start.y + 1, middle.y);
    Ok(())
}

#[test]
fn rendering_scrolls_to_a_cursor_moved_up_in_a_long_draft() -> Result<()> {
    let mut app = App {
        input: "0123456789".repeat(10).into(),
        ..App::default()
    };
    app.mascot.toggle_visibility();
    let mut terminal = Terminal::new(TestBackend::new(10, 8))?;
    terminal.draw(|frame| app.view(frame))?;
    for _ in 0..5 {
        key(&mut app, KeyCode::Up);
    }
    terminal.draw(|frame| app.view(frame))?;
    let cursor = terminal.get_cursor_position()?;
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 4, "the caret row scrolls into the input area");
    Ok(())
}
