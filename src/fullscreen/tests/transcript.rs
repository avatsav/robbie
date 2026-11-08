use super::*;

#[test]
fn transcript_cache_reuses_existing_rows_on_ticks_and_appends_but_reflows_on_resize() {
    let mut app = App::default();
    assistant(&mut app, "**alpha beta gamma delta epsilon**");
    app.update_rows(80);
    let initial = app.rendered_rows.clone();
    let allocation = app.rendered_rows[0].spans[0].content.as_ptr();
    app.content.advance_tick();
    app.update_rows(80);
    assert_eq!(app.rendered_rows, initial);
    assert_eq!(app.rendered_rows[0].spans[0].content.as_ptr(), allocation);
    assistant(&mut app, "appended text");
    app.update_rows(80);
    assert_eq!(app.rendered_rows[0].spans[0].content.as_ptr(), allocation);
    assert!(app.rendered_rows.len() > initial.len());
    for width in [10, 80, 0, 10] {
        app.update_rows(width);
        let expected: Vec<_> = app
            .transcript
            .iter()
            .flat_map(|line| line.rows(width))
            .collect();
        assert_eq!(app.rendered_rows, expected);
    }
}

#[test]
fn output_modes_apply_in_fullscreen_without_a_mode_indicator() {
    for presses in 0..3 {
        let mut app = App::default();
        for _ in 0..presses {
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL,
            )));
        }
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolStarted {
                name: "bash".into(),
                input: json!({"command":"some command"}),
            },
        });
        let output = (0..20)
            .map(|n| format!("output {n:02}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n[exit 7]";
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolFinished {
                name: "bash".into(),
                output,
                is_error: true,
            },
        });
        let text = history(&mut app);
        assert!(text.contains("• [shell] some command"));
        assert!(text.contains("[exit 7]"));
        assert_eq!(
            text.contains("output 00"),
            presses == 2,
            "only Full shows the first line"
        );
        assert_eq!(
            text.contains("output 19"),
            presses != 0,
            "Concise hides output"
        );
        assert!(!text.contains("tools:"));
    }
}

#[test]
fn tool_groups_have_one_blank_line_before_assistant_text() -> Result<()> {
    for presses in 0..3 {
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(100, 24))?;
        for _ in 0..presses {
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL,
            )));
        }
        for command in ["first", "second"] {
            app.activity(AgentEvent {
                source: "main",
                kind: EventKind::ToolStarted {
                    name: "bash".into(),
                    input: json!({"command":command}),
                },
            });
            app.activity(AgentEvent {
                source: "main",
                kind: EventKind::ToolFinished {
                    name: "bash".into(),
                    output: "output\n[exit 0]".into(),
                    is_error: false,
                },
            });
            // Rendering drains pending rows, just as it does between live events.
            terminal.draw(|frame| app.view(frame))?;
        }
        if presses == 0 {
            assert_eq!(app.rows(100).len(), 2, "Concise tool headers stay adjacent");
        }
        // Spacing depends on what was printed, not the current selection.
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        assistant(&mut app, "Here is the explanation.");
        let rows = app.rows(100);
        let answer = rows
            .iter()
            .position(|row| row.to_string() == "Here is the explanation.")
            .unwrap();
        assert_eq!(
            rows[answer - 1].to_string(),
            "",
            "missing tool-to-assistant gap after {presses} mode presses"
        );
        assert_ne!(rows[answer - 2].to_string(), "", "do not add a second gap");
    }
    Ok(())
}

#[test]
fn submitted_prompt_gutter_reflows_with_retained_history() -> Result<()> {
    use ratatui::style::Color;
    let mut app = App::default();
    app.handle_event(Event::Paste("alpha beta gamma".into()));
    assert_eq!(app.content.submit(), "alpha beta gamma");
    for width in [80, 12, 80, 12] {
        let mut terminal = Terminal::new(TestBackend::new(width, 12))?;
        terminal.draw(|frame| app.view(frame))?;
        let buffer = terminal.backend().buffer();
        let expected = if width == 12 {
            vec![(0, "a"), (1, "g")]
        } else {
            vec![(0, "a")]
        };
        for (y, first) in expected {
            assert_eq!(buffer[(0, y)].symbol(), "▌");
            assert_eq!(buffer[(2, y)].symbol(), first);
            assert_eq!(buffer[(0, y)].fg, Color::Rgb(217, 139, 92));
            assert_eq!(buffer[(2, y)].fg, Color::Rgb(217, 139, 92));
        }
    }
    Ok(())
}

#[test]
fn markdown_reflows_on_resize_with_styling_and_scrolling_intact() -> Result<()> {
    use ratatui::style::Modifier;
    let mut app = App::default();
    assistant(
        &mut app,
        "# Heading\n\n**alpha beta gamma delta epsilon zeta eta theta**\n\n```rust\n    let x = 1;\n```\n\nLast paragraph.",
    );
    let wide = app.rows(80);
    let narrow = app.rows(16);
    assert!(narrow.len() > wide.len());
    assert_eq!(app.rows(80), wide);
    let mut terminal = Terminal::new(TestBackend::new(16, 8))?;
    app.scroll_back = usize::MAX;
    terminal.draw(|frame| app.view(frame))?;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].symbol(), "H");
    assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(0, 2)].symbol(), "a");
    assert!(buffer[(0, 2)].modifier.contains(Modifier::BOLD));
    app.handle_event(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
    terminal.draw(|frame| app.view(frame))?;
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("Last paragraph."));
    assert_eq!(app.scroll_back, 0);
    Ok(())
}
