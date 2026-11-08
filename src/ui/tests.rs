use super::*;
use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
use serde_json::json;

fn render_output(terminal: &mut Terminal<TestBackend>, app: &mut App) -> Result<()> {
    let width = terminal.size()?.width as usize;
    let rows: Vec<_> = app
        .take_output()
        .iter()
        .flat_map(|line| line.rows(width))
        .collect();
    terminal.draw(|frame| frame.render_widget(Paragraph::new(rows), frame.area()))?;
    Ok(())
}

#[test]
fn clipping_respects_grapheme_boundaries_and_terminal_cell_budgets() {
    for glyph in ["#\u{fe0f}", "1\u{fe0f}\u{20e3}", "👩‍💻", "👍🏽", "e\u{301}"] {
        let text = format!("abc{glyph}xyz{glyph}");
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain([text.len()])
            .collect();
        for width in 0..20 {
            let head = single_line(&text, width);
            assert!(head.width() <= width, "{text:?}: {head:?} at {width}");
            let tail = input_tail(&text, width);
            assert!(tail.width() <= width);
            assert!(boundaries.contains(&(text.len() - tail.len())));
            if text.width() > width && width > 0 {
                assert!(head.ends_with('…'));
                assert!(boundaries.contains(&(head.len() - '…'.len_utf8())));
            }
        }
    }
}

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
fn submitted_prompts_wrap_without_splitting_graphemes() {
    let rows = user_prompt_rows("👩‍💻👩‍💻", 4);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["▌ 👩‍💻", "▌ 👩‍💻"]
    );
    assert!(rows.iter().all(|row| row.width() == 4));
}

#[test]
fn notices_are_visible_plain_text_without_marking_a_turn_failed() {
    let mut app = App::default();
    app.notice("[warning] Diagnostics unavailable\u{1b}[31m");
    let output = app.take_output();
    assert!(matches!(&output[0], OutputLine::Wrapped((text, _))
            if text.contains("[warning] Diagnostics unavailable") && !text.contains('\u{1b}')));
    assert!(!app.failed);
    assert!(!app.working);
}

#[test]
fn banner_brand_is_bold_white_and_metadata_is_regular_white() -> Result<()> {
    use ratatui::style::Modifier;
    let mut terminal = Terminal::new(TestBackend::new(100, 12))?;
    let mut app = App::default();
    app.banner("test/model");
    render_output(&mut terminal, &mut app)?;
    let buffer = terminal.backend().buffer();
    for x in 2..8 {
        assert_eq!(buffer[(x, 0)].fg, Color::White);
        assert!(buffer[(x, 0)].modifier.contains(Modifier::BOLD));
    }
    for x in 9..38 {
        assert_eq!(buffer[(x, 0)].fg, Color::White);
        assert!(!buffer[(x, 0)].modifier.contains(Modifier::BOLD));
    }
    // The banner is a single row: the model line. No keyboard or
    // shell-approval hints follow it.
    assert_eq!(buffer[(0, 0)].fg, Color::Cyan);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    for removed in [
        "Enter send",
        "Ctrl+O cycle tool output",
        "Shell commands run without approval",
    ] {
        assert!(!text.contains(removed), "banner still shows {removed:?}");
    }
    assert!(
        !buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::DIM))
    );
    Ok(())
}

#[test]
fn submitted_prompt_repeats_its_gutter_on_wrapped_rows() {
    let mut app = App {
        input: "alpha beta gamma".into(),
        ..App::default()
    };
    assert_eq!(app.submit(), "alpha beta gamma");
    let rows = app.pending[0].rows(12);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["▌ alpha beta", "▌ gamma"]
    );
    for row in rows {
        assert_eq!(row.style.fg, Some(USER_ORANGE));
        assert!(row.width() <= 12);
    }
}

#[test]
fn submitted_prompt_keeps_literal_text_and_fits_tiny_widths() {
    let mut app = App {
        input: "**literal**\n\n猫猫 abcdefghijklmnop\u{1b}".into(),
        ..App::default()
    };
    let prompt = app.submit();
    assert!(
        prompt.ends_with('\u{1b}'),
        "display sanitizing must not change the submitted prompt"
    );
    let output = &app.pending[0];
    let wide = output.rows(80);
    assert_eq!(
        wide.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["▌ **literal**", "▌ ", "▌ 猫猫 abcdefghijklmnop",]
    );
    for width in 0..20 {
        let rows = output.rows(width);
        assert!(rows.iter().all(|row| row.width() <= width.max(2)));
        let body: String = rows
            .iter()
            .flat_map(|row| row.to_string().chars().collect::<Vec<_>>())
            .filter(|ch| !ch.is_whitespace() && *ch != '▌')
            .collect();
        assert_eq!(body, "**literal**猫猫abcdefghijklmnop");
        if width >= 4 {
            assert!(rows.iter().all(|row| row.to_string().starts_with("▌ ")));
        }
    }
    assert_eq!(output.rows(80), wide);
}

#[test]
fn submitted_prompt_has_an_aligned_orange_gutter_on_each_row() -> Result<()> {
    let mut app = App {
        input: "alpha beta gamma".into(),
        ..App::default()
    };
    app.submit();
    let mut terminal = Terminal::new(TestBackend::new(12, 12))?;
    render_output(&mut terminal, &mut app)?;
    let buffer = terminal.backend().buffer();
    for (y, first) in [(0, "a"), (1, "g")] {
        assert_eq!(buffer[(0, y)].symbol(), "▌");
        assert_eq!(buffer[(1, y)].symbol(), " ");
        assert_eq!(buffer[(2, y)].symbol(), first);
        assert_eq!(buffer[(0, y)].fg, USER_ORANGE);
        assert_eq!(buffer[(2, y)].fg, USER_ORANGE);
    }
    assert_eq!(
        buffer[(0, 2)].symbol(),
        " ",
        "trailing separator stays unmarked"
    );
    assert!(app.pending.is_empty());
    Ok(())
}

#[test]
fn assistant_markdown_preserves_styles_and_drains_once() -> Result<()> {
    use ratatui::style::Modifier;
    let mut app = App::default();
    app.activity(AgentEvent { source: "main", kind: EventKind::AssistantText("## Heading\n\n**bold** and `code`\n\n```rust\n    literal **code**\n```\n\n> quoted `code`".into(), None) });
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    render_output(&mut terminal, &mut app)?;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].symbol(), "H");
    assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(0, 2)].symbol(), "b");
    assert!(buffer[(0, 2)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(9, 2)].fg, Color::Cyan);
    assert_eq!(buffer[(4, 4)].symbol(), "l");
    assert!(!buffer[(4, 4)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(0, 6)].symbol(), "│");
    assert_eq!(buffer[(2, 6)].fg, MUTED_TEXT);
    assert_eq!(buffer[(9, 6)].fg, MUTED_TEXT);
    assert!(buffer[(2, 6)].modifier.contains(Modifier::ITALIC));
    assert!(!buffer[(0, 6)].modifier.contains(Modifier::ITALIC));
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!text.contains("```"));
    assert!(app.pending.is_empty());
    assert!(app.take_output().is_empty());
    Ok(())
}

#[test]
fn only_assistant_text_is_markdown_and_child_blocks_keep_their_label() {
    let mut app = App {
        input: "**literal prompt**".into(),
        ..App::default()
    };
    app.submit();
    assert_eq!(
        app.pending[0].rows(80)[0].to_string(),
        "▌ **literal prompt**"
    );
    app.pending.clear();
    app.output_mode = ToolOutputMode::Full;
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolFinished {
            name: "read_file".into(),
            output: "# literal file\n**not bold**".into(),
            is_error: false,
        },
    });
    let rows = app
        .take_output()
        .iter()
        .flat_map(|line| line.rows(80))
        .collect::<Vec<_>>();
    assert!(rows.iter().any(|line| line.to_string() == "# literal file"));
    assert!(rows.iter().any(|line| line.to_string() == "**not bold**"));
    app.activity(AgentEvent {
        source: "subagent",
        kind: EventKind::AssistantText(
            "# Child\n\n```\n    **literal**\n```\n\nhello\u{1b}[31m".into(),
            None,
        ),
    });
    let rows = app
        .take_output()
        .iter()
        .flat_map(|line| line.rows(80))
        .collect::<Vec<_>>();
    let text = rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.starts_with("[agent]\nChild\n"), "{text}");
    assert!(text.contains("    **literal**"));
    assert!(!text.contains('\u{1b}'));
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

#[test]
fn skill_tool_has_a_readable_header_while_running_and_after_completion() {
    for source in ["main", "subagent"] {
        let mut app = App::default();
        app.activity(AgentEvent {
            source,
            kind: EventKind::ToolStarted {
                name: "load_skill".into(),
                input: json!({"name": "rust-for-kotlin"}),
            },
        });
        let expected = if source == "main" {
            "[skill] rust-for-kotlin"
        } else {
            "[agent/skill] rust-for-kotlin"
        };
        assert_eq!(app.active_tools.get(source).unwrap().summary, expected);
        app.activity(AgentEvent {
            source,
            kind: EventKind::ToolFinished {
                name: "load_skill".into(),
                output: "Skill instructions".into(),
                is_error: false,
            },
        });
        assert_eq!(app.pending[0].text(), expected);
        assert_eq!(app.pending[0].rows(80)[0].to_string(), expected);
    }
}

#[test]
fn every_tool_has_one_compact_header() {
    for name in [
        "bash",
        "read_file",
        "write_file",
        "edit_file",
        "todo",
        "sub_agent",
        "load_skill",
    ] {
        for is_error in [false, true] {
            let mut app = App::default();
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                    name: name.into(), input: json!({"command":"printf hello", "path":"file.rs", "old_text":"old", "new_text":"new"}),
                }});
            assert!(
                app.pending.is_empty(),
                "running {name} must stay in the live area"
            );
            app.activity(AgentEvent {
                source: "main",
                kind: EventKind::ToolFinished {
                    name: name.into(),
                    output: "detail one\ndetail two".into(),
                    is_error,
                },
            });
            assert_eq!(
                app.pending
                    .iter()
                    .filter(|line| matches!(line, OutputLine::Tool { .. }))
                    .count(),
                1,
                "completed {name} must produce one header"
            );
        }
    }
}

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
fn parent_tool_stays_running_while_child_tools_complete() -> Result<()> {
    let mut terminal = Terminal::new(TestBackend::new(80, 6))?;
    let mut app = App::default();
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolStarted {
            name: "sub_agent".into(),
            input: json!({"prompt":"inspect dependencies"}),
        },
    });
    app.activity(AgentEvent {
        source: "subagent",
        kind: EventKind::ToolStarted {
            name: "read_file".into(),
            input: json!({"path":"Cargo.toml"}),
        },
    });
    terminal.draw(|frame| app.view(frame))?;
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("Working…"));
    let before_completion = text;
    app.activity(AgentEvent {
        source: "subagent",
        kind: EventKind::ToolFinished {
            name: "read_file".into(),
            output: "dependencies".into(),
            is_error: false,
        },
    });
    terminal.draw(|frame| app.view(frame))?;
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert_eq!(text, before_completion);
    assert!(app.active_tools.contains_key("main"));
    assert!(!app.active_tools.contains_key("subagent"));
    assert_eq!(terminal.backend().buffer()[(0, 0)].bg, Color::Reset);
    assert_eq!(app.pending.len(), 1);
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
