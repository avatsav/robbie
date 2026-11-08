use super::*;

#[test]
fn ctrl_o_cycles_modes_without_rewriting_history_or_draft() -> Result<()> {
    let mut app = App {
        input: "next prompt".into(),
        ..App::default()
    };
    app.push("already printed output", Style::new());
    let before: Vec<_> = app
        .pending
        .iter()
        .map(|line| line.text().to_string())
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(80, 6))?;
    assert_eq!(app.output_mode, ToolOutputMode::Concise);
    for expected in [
        ToolOutputMode::Relaxed,
        ToolOutputMode::Full,
        ToolOutputMode::Concise,
    ] {
        assert_eq!(
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL
            ))),
            InputAction::None
        );
        assert_eq!(app.output_mode, expected);
        assert_eq!(app.input, "next prompt");
        assert_eq!(
            app.pending
                .iter()
                .map(|line| line.text().to_string())
                .collect::<Vec<_>>(),
            before
        );
        terminal.draw(|frame| app.view(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("next prompt"));
        assert!(
            !text.contains("tools:"),
            "output mode must not appear in the prompt"
        );
    }
    Ok(())
}

#[test]
fn concise_bash_shows_exit_code_and_a_text_dot_only_on_failure() {
    for code in [0, 7] {
        let mut app = App {
            output_mode: ToolOutputMode::Concise,
            ..App::default()
        };
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolStarted {
                name: "bash".into(),
                input: json!({"command":"a very long command ".repeat(20)}),
            },
        });
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolFinished {
                name: "bash".into(),
                output: format!("hidden output\n[exit {code}]"),
                is_error: code != 0,
            },
        });
        assert_eq!(app.pending.len(), 1);
        let rows = app.pending[0].rows(36);
        let text: String = rows[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.ends_with(&format!("[exit {code}]")));
        assert!(text.contains('…'), "command must truncate before outcome");
        assert_eq!(text.starts_with("• "), code != 0);
        assert!(rows[0].width() <= 36);
        if code != 0 {
            assert_eq!(rows[0].spans[0].style.fg, Some(MUTED_RED));
        }
        for width in 0..40 {
            assert!(app.pending[0].rows(width)[0].width() <= width);
        }
    }
    assert_eq!(bash_footer("Command timed out after 120 seconds"), None);
    assert_eq!(
        bash_footer("(no output)\n[signal: 9 (SIGKILL)]"),
        Some("[signal: 9 (SIGKILL)]".into())
    );
    let mut app = App {
        output_mode: ToolOutputMode::Concise,
        ..App::default()
    };
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolFinished {
            name: "bash".into(),
            output: "Command timed out after 120 seconds".into(),
            is_error: true,
        },
    });
    assert!(
        app.pending[0].rows(40)[0]
            .to_string()
            .ends_with("[no exit code]")
    );
}

#[test]
fn full_bash_shows_all_returned_lines_without_preview_truncation() {
    let mut app = App {
        output_mode: ToolOutputMode::Full,
        ..App::default()
    };
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
        + "\n[exit 0]";
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolFinished {
            name: "bash".into(),
            output,
            is_error: false,
        },
    });
    assert!(app.pending.iter().any(|line| line.text() == "output 00"));
    assert!(app.pending.iter().any(|line| line.text() == "output 19"));
    assert!(app.pending.iter().any(|line| line.text() == "[exit 0]"));
    assert!(
        !app.pending
            .iter()
            .any(|line| line.text().contains("omitted"))
    );
}

#[test]
fn mode_changes_while_busy_apply_to_the_next_completed_result() {
    let mut app = App {
        output_mode: ToolOutputMode::Relaxed,
        ..App::default()
    };
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolStarted {
            name: "read_file".into(),
            input: json!({"path":"Cargo.toml"}),
        },
    });
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL,
    )));
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolFinished {
            name: "read_file".into(),
            output: "dependencies".into(),
            is_error: false,
        },
    });
    assert!(app.pending.iter().any(|line| line.text() == "dependencies"));
    app.pending.clear();
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL,
    )));
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
            output: "dependencies".into(),
            is_error: false,
        },
    });
    assert_eq!(app.pending.len(), 1);
}

#[test]
fn bash_shows_a_capped_tail_and_preserves_the_exit_code() {
    let mut app = App {
        output_mode: ToolOutputMode::Relaxed,
        ..App::default()
    };
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
    assert!(app.pending.iter().any(|line| line.text() == "output 19"));
    assert!(app.pending.iter().any(|line| line.text() == "[exit 7]"));
    assert!(!app.pending.iter().any(|line| line.text() == "output 00"));
    assert!(app.pending.len() <= 10);
}
