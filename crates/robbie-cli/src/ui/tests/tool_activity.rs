use super::*;

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
