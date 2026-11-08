use super::*;

#[test]
fn drag_highlights_then_copies_once_on_release_and_clears() -> Result<()> {
    use ratatui::style::Color;
    let mut app = App::default();
    assistant(&mut app, "**alpha** beta");
    let mut terminal = Terminal::new(TestBackend::new(40, 8))?;
    terminal.draw(|frame| app.view(frame))?;
    app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0));
    terminal.draw(|frame| app.view(frame))?;
    assert!(app.selection.is_active());
    for x in 0..5 {
        assert_eq!(
            terminal.backend().buffer()[(x, 0)].bg,
            Color::Rgb(50, 75, 110)
        );
    }
    let mut clipboard = Vec::new();
    app.write_pending_copy(&mut clipboard)?;
    assert!(
        clipboard.is_empty(),
        "dragging must not write the clipboard"
    );
    app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 4, 0));
    assert!(!app.selection.is_active());
    app.write_pending_copy(&mut clipboard)?;
    assert_eq!(clipboard, b"\x1b]52;c;YWxwaGE=\x1b\\");
    let copied = clipboard.clone();
    app.write_pending_copy(&mut clipboard)?;
    assert_eq!(clipboard, copied, "redraws must not copy again");
    terminal.draw(|frame| app.view(frame))?;
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == Color::Reset)
    );
    Ok(())
}

#[test]
fn clicks_and_drags_in_the_live_input_or_blank_area_do_not_copy() -> Result<()> {
    let mut app = App::default();
    assistant(&mut app, "hello");
    let mut terminal = Terminal::new(TestBackend::new(40, 8))?;
    terminal.draw(|frame| app.view(frame))?;
    for (y, end) in [(0, 0), (1, 10), (4, 10), (7, 10)] {
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, y));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), end, y));
        let mut clipboard = Vec::new();
        app.write_pending_copy(&mut clipboard)?;
        assert!(clipboard.is_empty(), "unexpected copy on row {y}");
        assert!(!app.selection.is_active());
    }
    Ok(())
}

#[test]
fn selection_tracks_scrolled_rows_and_survives_new_output_without_jumping() -> Result<()> {
    let mut app = App::default();
    assistant(
        &mut app,
        (0..40)
            .map(|n| format!("row-{n:02}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    let mut terminal = Terminal::new(TestBackend::new(40, 8))?;
    terminal.draw(|frame| app.view(frame))?;
    app.handle_event(mouse(MouseEventKind::ScrollUp, 0, 0));
    terminal.draw(|frame| app.view(frame))?;
    let original_start = app.visible_start;
    let y = (0..app.history_area.height)
        .find(|y| {
            !app.rendered_rows[original_start + *y as usize]
                .to_string()
                .is_empty()
        })
        .unwrap();
    let original_text = app.rendered_rows[original_start + y as usize].to_string();
    app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, y));
    app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 5, y));
    assistant(&mut app, "New output while selecting");
    terminal.draw(|frame| app.view(frame))?;
    assert_eq!(
        app.visible_start, original_start,
        "output moved the drag target"
    );
    app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, y));
    assert_eq!(app.pending_copy.as_deref(), Some(original_text.as_str()));
    assert!(!app.selection.is_active());
    Ok(())
}

#[test]
fn wheel_scrolling_during_a_drag_keeps_transcript_coordinates() -> Result<()> {
    let mut app = App::default();
    assistant(
        &mut app,
        (0..40)
            .map(|n| format!("row-{n:02}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    let mut terminal = Terminal::new(TestBackend::new(40, 8))?;
    terminal.draw(|frame| app.view(frame))?;
    let original_start = app.visible_start;
    app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    app.handle_event(mouse(MouseEventKind::ScrollUp, 0, 0));
    terminal.draw(|frame| app.view(frame))?;
    assert!(app.visible_start < original_start);
    app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0));
    app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 0, 0));
    let text = app.pending_copy.as_ref().unwrap();
    assert!(text.contains('\n'));
    assert!(text.starts_with(&app.rendered_rows[app.visible_start].to_string()));
    Ok(())
}

#[test]
fn resizing_or_keyboard_input_cancels_a_drag_without_copying() -> Result<()> {
    for event in [
        Event::Resize(20, 8),
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Event::Paste("draft".into()),
    ] {
        let mut app = App::default();
        assistant(&mut app, "alpha beta");
        let mut terminal = Terminal::new(TestBackend::new(40, 8))?;
        terminal.draw(|frame| app.view(frame))?;
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0));
        app.handle_event(event);
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 4, 0));
        assert!(!app.selection.is_active());
        assert!(app.pending_copy.is_none());
    }
    Ok(())
}

#[test]
fn viewport_resize_cancels_selection_even_before_the_resize_event_arrives() -> Result<()> {
    for (width, height) in [(20, 8), (40, 12), (1, 1), (0, 0)] {
        let mut app = App::default();
        assistant(&mut app, "alpha beta");
        let mut terminal = Terminal::new(TestBackend::new(40, 8))?;
        terminal.draw(|frame| app.view(frame))?;
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0));
        terminal.backend_mut().resize(width, height);
        terminal.draw(|frame| app.view(frame))?;
        assert!(
            !app.selection.is_active(),
            "selection survived viewport {width}x{height}"
        );
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 4, 0));
        assert!(app.pending_copy.is_none());
    }
    Ok(())
}

#[test]
fn mouse_wheel_moves_visible_transcript_and_stops_at_bounds() -> Result<()> {
    let mut app = App::default();
    assistant(
        &mut app,
        (0..40)
            .map(|n| format!("row {n:02}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 12))?;
    let screen = |terminal: &Terminal<TestBackend>| -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    };
    let wheel = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
    };
    terminal.draw(|frame| app.view(frame))?;
    let bottom = screen(&terminal);
    assert!(bottom.contains("row 39"));
    assert_eq!(
        app.handle_event(wheel(MouseEventKind::ScrollUp)),
        InputAction::None
    );
    terminal.draw(|frame| app.view(frame))?;
    assert_ne!(screen(&terminal), bottom);
    assert!(!screen(&terminal).contains("row 39"));
    app.handle_event(wheel(MouseEventKind::ScrollDown));
    terminal.draw(|frame| app.view(frame))?;
    assert_eq!(screen(&terminal), bottom);
    app.handle_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(app.scroll_back, 0);
    for _ in 0..100 {
        app.handle_event(wheel(MouseEventKind::ScrollUp));
    }
    terminal.draw(|frame| app.view(frame))?;
    assert!(screen(&terminal).contains("row 00"));
    app.handle_event(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
    terminal.draw(|frame| app.view(frame))?;
    assert_eq!(screen(&terminal), bottom);
    Ok(())
}
