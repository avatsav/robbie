use super::*;

fn press(app: &mut App, code: KeyCode) -> InputAction {
    app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

#[test]
fn menu_is_highlighted_above_the_input_without_entering_the_transcript_or_moving_the_cursor_row() {
    let mut app = App::new("test/model");
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| app.view(frame)).unwrap();
    let cursor_before = terminal.get_cursor_position().unwrap();
    let history_before = history(&mut app);
    let height_before = app.history_area.height;
    press(&mut app, KeyCode::Char('/'));
    terminal.draw(|frame| app.view(frame)).unwrap();
    let rows = screen(&terminal);
    let menu_y = app.history_area.bottom();
    assert_eq!(app.history_area.height, height_before - 5);
    assert!(rows[menu_y as usize].contains("Commands"));
    assert!(rows[menu_y as usize + 1].contains("› /help"));
    assert!(rows[menu_y as usize + 2].contains("/compact"));
    assert!(rows[menu_y as usize + 2].contains("billed summary request"));
    assert!(rows[menu_y as usize + 3].contains("/exit"));
    assert!(rows[menu_y as usize + 3].contains("exit Robbie"));
    assert!(rows[menu_y as usize + 4].contains("Tab/Enter fill"));
    let buffer = terminal.backend().buffer();
    assert_ne!(buffer[(1, menu_y + 1)].bg, buffer[(1, menu_y + 2)].bg);
    assert_eq!(terminal.get_cursor_position().unwrap().y, cursor_before.y);
    assert_eq!(history(&mut app), history_before);
    assert!(rows.last().unwrap().ends_with("test/model"));

    press(&mut app, KeyCode::Down);
    terminal.draw(|frame| app.view(frame)).unwrap();
    assert!(screen(&terminal)[menu_y as usize + 2].contains("› /compact"));
    let buffer = terminal.backend().buffer();
    assert_ne!(buffer[(1, menu_y + 1)].bg, buffer[(1, menu_y + 2)].bg);
    press(&mut app, KeyCode::Esc);
    terminal.draw(|frame| app.view(frame)).unwrap();
    assert_eq!(app.history_area.height, height_before);
    assert!(!screen(&terminal).join("\n").contains("Commands"));
    assert_eq!(history(&mut app), history_before);
}

#[test]
fn filtering_dismissal_and_resize_keep_the_selected_row_visible_and_drawing_does_not_change_it() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Down);
    // At this height only one suggestion fits above the panel and four history rows.
    for (width, height) in [(20, 12), (100, 30), (20, 12), (100, 30)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.view(frame)).unwrap();
        let rows = screen(&terminal);
        assert!(rows.join("\n").contains("› /compact"));
        if height == 12 {
            assert!(!rows.join("\n").contains("/help"));
        }
        assert!(app.history_area.height >= 4);
        let cursor = terminal.get_cursor_position().unwrap();
        assert!(cursor.y >= app.history_area.bottom());
    }
    assert_eq!(press(&mut app, KeyCode::Tab), InputAction::None);
    assert_eq!(
        app.content.submit(),
        Some(Submission::Compact),
        "drawing did not reset selection"
    );
}

#[test]
fn exact_command_and_busy_hints_match_the_actual_enter_behavior() {
    for busy in [false, true] {
        let mut app = App::default();
        app.handle_event(Event::Paste("/compact".into()));
        if busy {
            app.content.start_compaction();
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.view(frame)).unwrap();
        let text = screen(&terminal).join("\n");
        if busy {
            assert!(text.contains("(busy)"));
            assert!(!text.contains("Enter run"));
            assert_eq!(press(&mut app, KeyCode::Enter), InputAction::None);
        } else {
            assert!(text.contains("Enter run"));
            assert_eq!(press(&mut app, KeyCode::Enter), InputAction::Submit);
        }
    }
}

#[test]
fn tiny_viewports_hide_or_clip_the_menu_without_losing_the_draft_or_cursor() {
    for (width, height) in [(1, 1), (10, 5), (20, 8), (20, 12), (80, 12), (100, 30)] {
        let mut app = App::default();
        app.handle_event(Event::Paste("/co".into()));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.view(frame)).unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert!(Rect::new(0, 0, width, height).contains(cursor));
        if width < 12 || height < 12 {
            assert!(!screen(&terminal).join("\n").contains("Commands"));
        }
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.content.submit(), Some(Submission::Compact));
    }
}
