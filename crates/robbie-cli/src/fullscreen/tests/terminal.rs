use super::*;

mod pty;
mod scenarios;

const COMPLETED_FIXTURE: &str = "fullscreen::tests::terminal::completed_read_terminal_fixture";
const BLOCKED_FIXTURE: &str = "fullscreen::tests::terminal::blocked_tool_terminal_fixture";

#[cfg(unix)]
#[test]
#[ignore = "PTY child fixture; launched by terminal::scenarios"]
fn completed_read_terminal_fixture() {
    crate::run_with_runtime(async {
            let terminal_state = TerminalState::capture();
            let mut terminal = ratatui::init();
            let result = async {
                let _modes = ScreenModes::new()?;
                let directory = std::env::current_dir().unwrap();
                let mut workspace = ui::WorkspaceMonitor::start(directory.clone());
                let mut app = App::new("test/model");
                workspace.apply_current(&mut app.content);
                app.handle_event(Event::Paste("Read Cargo.toml and summarize dependencies".into()));
                app.content.submit();
                let mut keys = EventStream::new();
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let work = async {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    sender.send(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                        name: "read_file".into(), input: json!({"path":"Cargo.toml"}),
                    }})?;
                    let (output, is_error) = crate::test_support::read_file(&directory, "Cargo.toml").await;
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
#[ignore = "PTY child fixture; launched by terminal::scenarios"]
fn blocked_tool_terminal_fixture() {
    crate::run_with_runtime(async {
        let path = std::env::var("ROBBIE_TEST_FIFO")?;
        let terminal_state = TerminalState::capture();
        let mut terminal = ratatui::init();
        let result = async {
            let _modes = ScreenModes::new()?;
            let directory = std::env::current_dir().unwrap();
            let mut workspace = ui::WorkspaceMonitor::start(directory.clone());
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
            let work = async {
                let (output, error) = crate::test_support::read_file(&directory, &path).await;
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
