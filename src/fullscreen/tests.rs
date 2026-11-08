use super::*;
use crate::events::EventKind;
use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

fn history(app: &mut App) -> String {
    app.rows(100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant(app: &mut App, text: impl Into<String>) {
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::AssistantText(text.into(), None),
    });
}

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

#[cfg(unix)]
#[test]
fn model_recorder_failure_becomes_one_visible_notice_without_losing_the_draft() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let mut app = App::new("test/model");
    app.recorder = Some(recorder.clone());
    app.handle_event(Event::Paste("my next draft".into()));
    recorder.break_writer_for_test();
    drop(recorder.model_request("main", "test/model", &json!({})));
    app.recording_warning();
    app.recording_warning();
    let text = history(&mut app);
    assert_eq!(
        text.matches("[warning] Diagnostics recording failed")
            .count(),
        1
    );
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))),
        InputAction::Submit
    );
    assert_eq!(app.content.submit(), "my next draft");
}

#[cfg(unix)]
#[test]
#[ignore = "requires a PTY; run via tests/blocked_tool_exit.py"]
fn completed_read_terminal_fixture() {
    crate::run_with_runtime(async {
            let terminal_state = TerminalState::capture();
            let mut terminal = ratatui::init();
            let result = async {
                let _modes = ScreenModes::new()?;
                let mut workspace = ui::WorkspaceMonitor::start(std::env::current_dir().unwrap());
                let mut app = App::new("test/model");
                workspace.apply_current(&mut app.content);
                app.handle_event(Event::Paste("Read Cargo.toml and summarize dependencies".into()));
                app.content.submit();
                let mut keys = EventStream::new();
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let mut tools = crate::tools::ToolRegistry::new();
                let work = async {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    sender.send(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                        name: "read_file".into(), input: json!({"path":"Cargo.toml"}),
                    }})?;
                    let (output, is_error) = tools.execute("read_file", &json!({"path":"Cargo.toml"})).await;
                    anyhow::ensure!(!is_error, "Cargo.toml read failed");
                    sender.send(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                        name: "read_file".into(), output, is_error,
                    }})?;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let answer = "## Dependencies\n\n**Dependency-summary-rendered-successfully**\n\n```rust\nlet markdown_fixture = true;\n```\n\n> Quoted-fixture";
                    sender.send(AgentEvent { source: "main", kind: EventKind::AssistantText(answer.into(), None) })?;
                    Ok(answer.into())
                };
                match run_turn(&mut terminal, &mut app, &mut keys, &mut receiver, &mut workspace, work).await? {
                    TurnOutcome::Finished(result) => app.content.finish_turn(result),
                    TurnOutcome::Quit => anyhow::bail!("quit before response"),
                }
                while wait_for_input(&mut terminal, &mut app, &mut keys, &mut workspace).await? != InputAction::Quit {}
                Ok(())
            }.await;
            ratatui::restore();
            drop(terminal_state);
            result
        }).unwrap();
}

#[cfg(unix)]
#[test]
#[ignore = "requires a PTY; run via tests/blocked_tool_exit.py"]
fn blocked_tool_terminal_fixture() {
    crate::run_with_runtime(async {
        let path = std::env::var("ROBBIE_TEST_FIFO")?;
        let terminal_state = TerminalState::capture();
        let mut terminal = ratatui::init();
        let result = async {
            let _modes = ScreenModes::new()?;
            let mut workspace = ui::WorkspaceMonitor::start(std::env::current_dir().unwrap());
            let mut app = App::default();
            workspace.apply_current(&mut app.content);
            assistant(
                &mut app,
                (0..60)
                    .map(|n| format!("history {n:02}"))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
            assistant(&mut app, "Blocked-read-test");
            app.activity(AgentEvent {
                source: "main",
                kind: EventKind::ToolStarted {
                    name: "read_file".into(),
                    input: json!({"path": path}),
                },
            });
            let mut keys = EventStream::new();
            let (_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut tools = crate::tools::ToolRegistry::new();
            let work = async {
                let (output, error) = tools.execute("read_file", &json!({"path": path})).await;
                if error {
                    anyhow::bail!("{output}");
                }
                Ok(output)
            };
            let outcome = run_turn(
                &mut terminal,
                &mut app,
                &mut keys,
                &mut receiver,
                &mut workspace,
                work,
            )
            .await?;
            anyhow::ensure!(
                matches!(outcome, TurnOutcome::Quit),
                "expected Ctrl+C exit during blocked read"
            );
            Ok(())
        }
        .await;
        ratatui::restore();
        drop(terminal_state);
        result
    })
    .unwrap();
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

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
fn input_paste_quit_and_failed_sessions_use_shared_behavior() {
    let mut app = App::default();
    let key = |code, modifiers| Event::Key(KeyEvent::new(code, modifiers));
    assert_eq!(
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        InputAction::None
    );
    assert_eq!(
        app.handle_event(Event::Paste("hello\n猫\u{3}".into())),
        InputAction::None
    );
    assert_eq!(
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        InputAction::Submit
    );
    assert_eq!(app.content.submit(), "hello 猫");
    app.finish_turn(Err(anyhow::anyhow!("model failed")));
    app.handle_event(Event::Paste("another prompt".into()));
    assert_eq!(
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        InputAction::Submit,
        "a failed model request must not prevent submitting the next prompt"
    );
    assert_eq!(
        app.handle_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        InputAction::Quit
    );
    let mut empty = App::default();
    assert_eq!(
        empty.handle_event(key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        InputAction::Quit
    );
}

#[test]
fn events_strip_control_characters() {
    let mut app = App::default();
    assistant(&mut app, "hello\r\n\n\u{1b}[31m");
    let text = history(&mut app);
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("hello\n"));
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
