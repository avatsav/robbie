use super::*;

#[test]
fn fullscreen_preserves_banner_and_conversation_styling() -> Result<()> {
    use ratatui::style::{Color, Modifier};
    let mut app = App::new("test/model");
    app.handle_event(Event::Paste("user prompt".into()));
    app.content.submit();
    assistant(&mut app, "assistant answer");
    let mut terminal = Terminal::new(TestBackend::new(100, 24))?;
    terminal.draw(|frame| app.view(frame))?;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(2, 0)].fg, Color::White);
    assert!(buffer[(2, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(9, 0)].fg, Color::White);
    assert!(!buffer[(9, 0)].modifier.contains(Modifier::BOLD));
    let rows = app.rows(100);
    let user_y = rows
        .iter()
        .position(|row| row.to_string() == "▌ user prompt")
        .unwrap() as u16;
    let assistant_y = rows
        .iter()
        .position(|row| row.to_string() == "assistant answer")
        .unwrap() as u16;
    assert_eq!(buffer[(2, user_y)].fg, Color::Rgb(217, 139, 92));
    assert_eq!(buffer[(0, assistant_y)].fg, Color::Reset);
    assert!(
        !buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::DIM))
    );
    // Transcript styling is unchanged; the mascot has dark eye backgrounds.
    assert!(
        buffer
            .content
            .iter()
            .take(100 * app.history_area.height as usize)
            .all(|cell| cell.bg == Color::Reset)
    );
    Ok(())
}

#[test]
fn renders_at_normal_and_tiny_sizes_and_clamps_scroll() -> Result<()> {
    for (width, height) in [(80, 24), (10, 5), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let mut app = App::default();
        assistant(&mut app, "Hello from Robbie");
        app.scroll_back = usize::MAX;
        terminal.draw(|frame| app.view(frame))?;
        assert!(app.scroll_back < usize::MAX);
        if width == 80 {
            let content: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(content.contains("Hello from Robbie"));
        }
    }
    Ok(())
}
