use anyhow::{Context, Result};
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind, MouseEventKind},
    execute,
};
use futures_util::StreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::Paragraph,
};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{Agent, events::AgentEvent, ui::{self, InputAction, OutputLine}};

// The shared UI state owns formatting and input. This frontend owns retained
// display history and scrolling instead of inserting it into terminal scrollback.
#[derive(Default)]
struct App {
    content: ui::App,
    transcript: Vec<OutputLine>,
    // Zero follows the latest output; otherwise count rows back from the bottom.
    scroll_back: usize,
}

impl App {
    fn new(model: &str) -> Self {
        let mut app = Self::default();
        app.content.banner(model);
        app
    }

    fn activity(&mut self, event: AgentEvent) {
        self.content.activity(event);
    }

    fn rows(&mut self, width: usize) -> Vec<Line<'static>> {
        self.transcript.extend(self.content.take_output());
        self.transcript.iter().flat_map(|line| line.rows(width)).collect()
    }

    fn view(&mut self, frame: &mut Frame) {
        let [history, live] = Layout::vertical([
            Constraint::Min(0), Constraint::Length(2),
        ]).areas(frame.area());
        let rows = self.rows(history.width as usize);
        let max_scroll = rows.len().saturating_sub(history.height as usize);
        self.scroll_back = self.scroll_back.min(max_scroll);
        let start = max_scroll.saturating_sub(self.scroll_back);
        let visible: Vec<_> = rows.into_iter().skip(start).take(history.height as usize).collect();
        frame.render_widget(Paragraph::new(visible), history);
        self.content.view_live(frame, live);
    }

    fn handle_event(&mut self, event: Event) -> InputAction {
        match &event {
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll_back = self.scroll_back.saturating_add(3),
                    MouseEventKind::ScrollDown => self.scroll_back = self.scroll_back.saturating_sub(3),
                    _ => {}
                }
                return InputAction::None;
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::PageUp => { self.scroll_back = self.scroll_back.saturating_add(10); return InputAction::None; }
                KeyCode::PageDown => { self.scroll_back = self.scroll_back.saturating_sub(10); return InputAction::None; }
                KeyCode::End => { self.scroll_back = 0; return InputAction::None; }
                _ => {}
            },
            _ => {}
        }
        self.content.handle_event(event)
    }
}

// Also release capture/paste mode during unwinding or a failed enable attempt.
struct ScreenModes;

impl ScreenModes {
    fn new() -> Result<Self> {
        let modes = Self;
        execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)
            .context("could not enable terminal input modes")?;
        Ok(modes)
    }
}

impl Drop for ScreenModes {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    }
}

pub async fn run(mut agent: Agent, mut receiver: UnboundedReceiver<AgentEvent>) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = async {
        let _modes = ScreenModes::new()?;
        run_app(&mut terminal, &mut agent, &mut receiver).await
    }.await;
    // Full-screen mode intentionally disappears on exit. Unlike native mode,
    // entering and leaving the alternate screen are a matched pair here.
    ratatui::restore();
    result
}

async fn run_app(terminal: &mut DefaultTerminal, agent: &mut Agent, receiver: &mut UnboundedReceiver<AgentEvent>) -> Result<()> {
    let mut app = App::new(&agent.model);
    let mut keys = EventStream::new();
    loop {
        terminal.draw(|frame| app.view(frame))?;
        let event = keys.next().await.context("terminal input closed")??;
        match app.handle_event(event) {
            InputAction::Quit => return Ok(()),
            InputAction::None => continue,
            InputAction::Submit => {}
        }
        let prompt = app.content.submit();
        app.scroll_back = 0;
        match run_turn(terminal, &mut app, &mut keys, receiver, agent.run(prompt.trim())).await? {
            TurnOutcome::Finished(result) => app.content.finish_turn(result),
            TurnOutcome::Quit => return Ok(()),
        }
    }
}

enum TurnOutcome {
    Finished(Result<String>),
    Quit,
}

// Keep the busy UI loop independent of the work it drives, so blocked-tool
// responsiveness can be exercised without calling a model.
async fn run_turn(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    keys: &mut EventStream,
    receiver: &mut UnboundedReceiver<AgentEvent>,
    run: impl std::future::Future<Output = Result<String>>,
) -> Result<TurnOutcome> {
    tokio::pin!(run);
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(100));
    loop {
        terminal.draw(|frame| app.view(frame))?;
        tokio::select! {
            result = &mut run => {
                while let Ok(event) = receiver.try_recv() { app.activity(event); }
                return Ok(TurnOutcome::Finished(result));
            }
            Some(event) = receiver.recv() => app.activity(event),
            _ = ticks.tick() => app.content.advance_tick(),
            event = keys.next() => {
                let event = event.context("terminal input closed")??;
                // Enter is ignored while busy, but drafting, modes and scrolling work.
                match app.handle_event(event) {
                    InputAction::Quit => return Ok(TurnOutcome::Quit),
                    InputAction::None | InputAction::Submit => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventKind;
    use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn history(app: &mut App) -> String {
        app.rows(100).iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    }

    fn assistant(app: &mut App, text: impl Into<String>) {
        app.activity(AgentEvent { source: "main", kind: EventKind::AssistantText(text.into()) });
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a PTY; run via tests/blocked_tool_exit.py"]
    fn completed_read_terminal_fixture() {
        crate::run_with_runtime(async {
            let mut terminal = ratatui::init();
            let result = async {
                let _modes = ScreenModes::new()?;
                let mut app = App::new("test/model");
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
                    sender.send(AgentEvent { source: "main", kind: EventKind::AssistantText(answer.into()) })?;
                    Ok(answer.into())
                };
                match run_turn(&mut terminal, &mut app, &mut keys, &mut receiver, work).await? {
                    TurnOutcome::Finished(result) => app.content.finish_turn(result),
                    TurnOutcome::Quit => anyhow::bail!("quit before response"),
                }
                loop {
                    terminal.draw(|frame| app.view(frame))?;
                    let event = keys.next().await.context("terminal input closed")??;
                    if app.handle_event(event) == InputAction::Quit { break }
                }
                Ok(())
            }.await;
            ratatui::restore();
            result
        }).unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a PTY; run via tests/blocked_tool_exit.py"]
    fn blocked_tool_terminal_fixture() {
        crate::run_with_runtime(async {
            let path = std::env::var("ROBBIE_TEST_FIFO")?;
            let mut terminal = ratatui::init();
            let result = async {
                let _modes = ScreenModes::new()?;
                let mut app = App::default();
                assistant(&mut app, (0..60).map(|n| format!("history {n:02}")).collect::<Vec<_>>().join("\n"));
                assistant(&mut app, "Blocked-read-test");
                let mut keys = EventStream::new();
                let (_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let mut tools = crate::tools::ToolRegistry::new();
                let work = async {
                    let (output, error) = tools.execute("read_file", &json!({"path": path})).await;
                    if error { anyhow::bail!("{output}"); }
                    Ok(output)
                };
                let outcome = run_turn(&mut terminal, &mut app, &mut keys, &mut receiver, work).await?;
                anyhow::ensure!(matches!(outcome, TurnOutcome::Quit), "expected Ctrl+C exit during blocked read");
                Ok(())
            }.await;
            ratatui::restore();
            result
        }).unwrap();
    }

    #[test]
    fn output_modes_apply_in_fullscreen_without_a_mode_indicator() {
        for presses in 0..3 {
            let mut app = App::default();
            for _ in 0..presses {
                app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)));
            }
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                name: "bash".into(), input: json!({"command":"some command"}),
            }});
            let output = (0..20).map(|n| format!("output {n:02}")).collect::<Vec<_>>().join("\n") + "\n[exit 7]";
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                name: "bash".into(), output, is_error: true,
            }});
            let text = history(&mut app);
            assert!(text.contains("• [bash] some command"));
            assert!(text.contains("[exit 7]"));
            assert_eq!(text.contains("output 00"), presses == 2, "only Full shows the first line");
            assert_eq!(text.contains("output 19"), presses != 0, "Concise hides output");
            assert!(!text.contains("tools:"));
        }
    }

    #[test]
    fn tool_groups_have_one_blank_line_before_assistant_text() -> Result<()> {
        for presses in 0..3 {
            let mut app = App::default();
            let mut terminal = Terminal::new(TestBackend::new(100, 24))?;
            for _ in 0..presses {
                app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)));
            }
            for command in ["first", "second"] {
                app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                    name: "bash".into(), input: json!({"command":command}),
                }});
                app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                    name: "bash".into(), output: "output\n[exit 0]".into(), is_error: false,
                }});
                // Rendering drains pending rows, just as it does between live events.
                terminal.draw(|frame| app.view(frame))?;
            }
            if presses == 0 {
                assert_eq!(app.rows(100).len(), 2, "Concise tool headers stay adjacent");
            }
            // Spacing depends on what was printed, not the current selection.
            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)));
            assistant(&mut app, "Here is the explanation.");
            let rows = app.rows(100);
            let answer = rows.iter().position(|row| row.to_string() == "Here is the explanation.").unwrap();
            assert_eq!(rows[answer - 1].to_string(), "", "missing tool-to-assistant gap after {presses} mode presses");
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
            let expected = if width == 12 { vec![(0, "a"), (1, "g")] } else { vec![(0, "a")] };
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
        assistant(&mut app, "# Heading\n\n**alpha beta gamma delta epsilon zeta eta theta**\n\n```rust\n    let x = 1;\n```\n\nLast paragraph.");
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
        let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
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
        assert_eq!(buffer[(2, 4)].fg, Color::Rgb(217, 139, 92));
        assert_eq!(buffer[(0, 6)].fg, Color::Reset);
        assert!(!buffer.content.iter().any(|cell| cell.modifier.contains(Modifier::DIM)));
        assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
        Ok(())
    }

    #[test]
    fn input_paste_quit_and_failed_sessions_use_shared_behavior() {
        let mut app = App::default();
        let key = |code, modifiers| Event::Key(KeyEvent::new(code, modifiers));
        assert_eq!(app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)), InputAction::None);
        assert_eq!(app.handle_event(Event::Paste("hello\n猫\u{3}".into())), InputAction::None);
        assert_eq!(app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)), InputAction::Submit);
        assert_eq!(app.content.submit(), "hello 猫");
        app.content.finish_turn(Err(anyhow::anyhow!("model failed")));
        app.handle_event(Event::Paste("another prompt".into()));
        assert_eq!(app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)), InputAction::None);
        assert_eq!(app.handle_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), InputAction::Quit);
        let mut empty = App::default();
        assert_eq!(empty.handle_event(key(KeyCode::Char('d'), KeyModifiers::CONTROL)), InputAction::Quit);
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
        assistant(&mut app, (0..40).map(|n| format!("row {n:02}")).collect::<Vec<_>>().join("\n\n"));
        let mut terminal = Terminal::new(TestBackend::new(80, 12))?;
        let screen = |terminal: &Terminal<TestBackend>| -> String {
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
        };
        let wheel = |kind| Event::Mouse(MouseEvent { kind, column: 5, row: 3, modifiers: KeyModifiers::NONE });
        terminal.draw(|frame| app.view(frame))?;
        let bottom = screen(&terminal);
        assert!(bottom.contains("row 39"));
        assert_eq!(app.handle_event(wheel(MouseEventKind::ScrollUp)), InputAction::None);
        terminal.draw(|frame| app.view(frame))?;
        assert_ne!(screen(&terminal), bottom);
        assert!(!screen(&terminal).contains("row 39"));
        app.handle_event(wheel(MouseEventKind::ScrollDown));
        terminal.draw(|frame| app.view(frame))?;
        assert_eq!(screen(&terminal), bottom);
        app.handle_event(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll_back, 0);
        for _ in 0..100 { app.handle_event(wheel(MouseEventKind::ScrollUp)); }
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
                let content: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
                assert!(content.contains("Hello from Robbie"));
            }
        }
        Ok(())
    }
}
