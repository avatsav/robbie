use super::*;

#[test]
fn conversation_has_priority_over_muted_single_line_tools() -> Result<()> {
    use ratatui::style::Modifier;
    let mut app = App {
        input: "user prompt".into(),
        ..App::default()
    };
    app.submit();
    assert_eq!(app.pending[0].rows(80)[0].style.fg, Some(USER_ORANGE));
    assert!(
        !app.pending[0].rows(80)[0]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
    app.pending.clear();
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::AssistantText("assistant answer".into(), None),
    });
    assert_eq!(app.pending[0].rows(80)[0].style, Style::new());
    app.pending.clear();
    let mut terminal = Terminal::new(TestBackend::new(18, 6))?;
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolStarted {
            name: "bash".into(),
            input: json!({"command":"printf 'a long command'"}),
        },
    });
    terminal.draw(|frame| app.view(frame))?;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].fg, MUTED_CYAN);
    assert_eq!(buffer[(8, 0)].fg, MUTED_CYAN);
    assert!(
        !buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::DIM))
    );
    assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
    assert_eq!(buffer[(17, 0)].symbol(), "─");
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolFinished {
            name: "bash".into(),
            output: "hello\n[exit 0]".into(),
            is_error: false,
        },
    });
    assert_eq!(app.pending[0].rows(18).len(), 1);
    render_output(&mut terminal, &mut app)?;
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == Color::Reset)
    );
    for width in 0..20 {
        let row = OutputLine::ToolOutput {
            prefix: "└",
            text: "a long line with 猫".into(),
        };
        assert!(row.rows(width)[0].width() <= width);
    }
    assert_eq!(single_line("猫猫猫", 4), "猫…");
    Ok(())
}

#[test]
fn reads_show_their_contents_only_in_full_mode() {
    for mode in [
        ToolOutputMode::Concise,
        ToolOutputMode::Relaxed,
        ToolOutputMode::Full,
    ] {
        let mut app = App {
            output_mode: mode,
            ..App::default()
        };
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolStarted {
                name: "read_file".into(),
                input: json!({"path":"Cargo.toml"}),
            },
        });
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolFinished {
                name: "read_file".into(),
                output: "file contents".into(),
                is_error: false,
            },
        });
        assert_eq!(app.pending[0].text(), "[read] Cargo.toml");
        assert_eq!(
            app.pending
                .iter()
                .any(|line| line.text().contains("file contents")),
            mode == ToolOutputMode::Full
        );
    }
}

#[test]
fn edit_details_follow_the_output_mode() {
    for mode in [
        ToolOutputMode::Concise,
        ToolOutputMode::Relaxed,
        ToolOutputMode::Full,
    ] {
        let mut app = App {
            output_mode: mode,
            ..App::default()
        };
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolStarted {
                name: "edit_file".into(),
                input: json!({"path":"a.rs", "old_text":"old", "new_text":"new"}),
            },
        });
        assert!(app.pending.is_empty());
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolFinished {
                name: "edit_file".into(),
                output: "File edited".into(),
                is_error: false,
            },
        });
        assert_eq!(
            app.pending.iter().any(|line| line.text() == "  - old"),
            mode == ToolOutputMode::Full
        );
        assert_eq!(
            app.pending.iter().any(|line| line.text() == "  + new"),
            mode == ToolOutputMode::Full
        );
    }
}

#[test]
fn failed_edits_show_errors_not_success_previews() {
    let mut app = App {
        output_mode: ToolOutputMode::Relaxed,
        ..App::default()
    };
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolStarted {
            name: "edit_file".into(),
            input: json!({"path":"a.rs", "old_text":"old", "new_text":"new"}),
        },
    });
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolFinished {
            name: "edit_file".into(),
            output: "old_text did not match".into(),
            is_error: true,
        },
    });
    assert_eq!(app.pending.len(), 2);
    assert_eq!(app.pending[0].text(), "[edit] a.rs");
    assert!(
        app.pending
            .iter()
            .any(|line| line.text().contains("old_text did not match"))
    );
    assert!(!app.pending.iter().any(|line| line.text() == "  + new"));
}
