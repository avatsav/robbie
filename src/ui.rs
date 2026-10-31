use anyhow::{Context, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    execute,
};
use futures_util::StreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, Paragraph},
};
use tokio::sync::mpsc::UnboundedReceiver;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{Agent, events::{AgentEvent, EventKind}};

#[derive(Debug, PartialEq, Eq)]
enum InputAction {
    None,
    Submit,
    Quit,
}

#[derive(Default)]
struct App {
    transcript: Vec<(String, Style)>,
    input: String,
    status: String,
    // Zero follows the latest output; otherwise count rows back from the bottom.
    scroll_back: usize,
    failed: bool,
}

impl App {
    fn push(&mut self, text: String, style: Style) {
        // Do not interpret terminal escape sequences from shell/model output.
        let text = text.replace('\t', "    ");
        let text: String = text.chars().filter(|c| !c.is_control() || *c == '\n').collect();
        self.transcript.push((text, style));
    }

    fn activity(&mut self, event: AgentEvent) {
        let source = event.source;
        match event.kind {
            EventKind::ModelStarted => self.status = format!("{source}: waiting for model…"),
            EventKind::AssistantText(text) => self.push(format!("[{source}] {text}"), Style::new()),
            EventKind::ToolStarted { name, input } => {
                self.status = format!("{source}: running {name}");
                self.push(format!("[{source}] {name}: {input}"), Style::new().cyan());
            }
            EventKind::ToolFinished { name, output, is_error } => {
                let style = if is_error { Style::new().red() } else { Style::new().dim() };
                self.push(format!("[{source}] {name}\n{output}"), style);
            }
        }
    }

    fn rows(&self, width: usize) -> Vec<Line<'static>> {
        let mut rows = Vec::new();
        for (text, style) in &self.transcript {
            for line in text.split('\n') {
                for wrapped in textwrap::wrap(line, width.max(1)) {
                    rows.push(Line::styled(wrapped.into_owned(), *style));
                }
            }
            rows.push(Line::default());
        }
        rows
    }

    fn view(&mut self, frame: &mut Frame) {
        let [history, input, status] = Layout::vertical([
            Constraint::Min(0), Constraint::Length(3), Constraint::Length(2),
        ]).areas(frame.area());
        let block = Block::bordered().title(" Robbie ");
        let inner = block.inner(history);
        let rows = self.rows(inner.width as usize);
        let max_scroll = rows.len().saturating_sub(inner.height as usize);
        self.scroll_back = self.scroll_back.min(max_scroll);
        let start = max_scroll.saturating_sub(self.scroll_back);
        let visible: Vec<_> = rows.into_iter().skip(start).take(inner.height as usize).collect();
        frame.render_widget(Paragraph::new(visible).block(block), history);

        let block = Block::bordered().title(" Prompt · Enter to send ");
        let inner = block.inner(input);
        let tail = input_tail(&self.input, inner.width.saturating_sub(1) as usize);
        frame.render_widget(Paragraph::new(tail).block(block), input);
        if inner.width > 0 && inner.height > 0 {
            frame.set_cursor_position((inner.x + tail.width() as u16, inner.y));
        }
        frame.render_widget(Paragraph::new(vec![
            Line::from(self.status.as_str()).cyan(),
            Line::from("Wheel/PgUp/PgDn scroll · End latest · Ctrl+C exit · Shell runs without approval").dim(),
        ]), status);
    }

    // Scrolling and editing update state directly; callers handle submit and quit.
    fn handle_event(&mut self, event: Event) -> InputAction {
        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_back = self.scroll_back.saturating_add(3),
                MouseEventKind::ScrollDown => self.scroll_back = self.scroll_back.saturating_sub(3),
                _ => {}
            }
            return InputAction::None;
        }
        let Event::Key(key) = event else { return InputAction::None };
        if key.kind == KeyEventKind::Release { return InputAction::None }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return InputAction::Quit;
        }
        match key.code {
            KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(10),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(10),
            KeyCode::End => self.scroll_back = 0,
            KeyCode::Enter if !self.failed && !self.input.trim().is_empty() => return InputAction::Submit,
            KeyCode::Backspace => { self.input.pop(); }
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) && !c.is_control() => self.input.push(c),
            _ => {}
        }
        InputAction::None
    }
}

// Input editing is deliberately append/backspace only for this first version.
fn input_tail(input: &str, width: usize) -> &str {
    let mut used = 0;
    for (index, ch) in input.char_indices().rev() {
        used += ch.width().unwrap_or(0);
        if used > width { return &input[index + ch.len_utf8()..] }
    }
    input
}

// A guard also disables capture during unwinding or a failed enable attempt.
struct MouseCapture;

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
}

pub async fn run(mut agent: Agent, mut receiver: UnboundedReceiver<AgentEvent>) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = async {
        let _mouse = MouseCapture;
        execute!(std::io::stdout(), EnableMouseCapture).context("could not enable mouse capture")?;
        run_app(&mut terminal, &mut agent, &mut receiver).await
    }.await;
    // Restore on success AND ordinary errors. init also installs a panic hook.
    ratatui::restore();
    result
}

async fn run_app(terminal: &mut DefaultTerminal, agent: &mut Agent, receiver: &mut UnboundedReceiver<AgentEvent>) -> Result<()> {
    let mut app = App { status: "Idle".into(), ..App::default() };
    let mut keys = EventStream::new();
    loop {
        terminal.draw(|frame| app.view(frame))?;
        let event = keys.next().await.context("terminal input closed")??;
        match app.handle_event(event) {
            InputAction::Quit => return Ok(()),
            InputAction::None => continue,
            InputAction::Submit => {}
        }
        let prompt = std::mem::take(&mut app.input);
        app.push(format!("You: {prompt}"), Style::new().green());
        app.scroll_back = 0;
        app.status = "Starting…".into();
        // This future borrows the same agent for the whole turn, preserving history.
        let result = match run_turn(terminal, &mut app, &mut keys, receiver, agent.run(prompt.trim())).await? {
            TurnOutcome::Finished(result) => result,
            TurnOutcome::Quit => return Ok(()),
        };
        while let Ok(event) = receiver.try_recv() { app.activity(event); }
        match result {
            Ok(_) => app.status = "Idle".into(),
            Err(error) => {
                app.push(format!("Agent error: {error:#}"), Style::new().red());
                // An interrupted tool exchange may be incomplete. Do not reuse it.
                app.failed = true;
                app.status = "Session failed; restart Robbie to continue. Ctrl+C to exit.".into();
            }
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
    loop {
        terminal.draw(|frame| app.view(frame))?;
        tokio::select! {
            result = &mut run => return Ok(TurnOutcome::Finished(result)),
            Some(event) = receiver.recv() => app.activity(event),
            event = keys.next() => {
                let event = event.context("terminal input closed")??;
                // Enter is ignored while busy, but drafting and scrolling work.
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
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};

    // PTY fixture: tests/blocked_tool_exit.py supplies the FIFO and keyboard
    // input. It uses the production busy loop and real read_file dispatch.
    #[cfg(unix)]
    #[test]
    #[ignore = "requires a PTY; run via tests/blocked_tool_exit.py"]
    fn blocked_tool_terminal_fixture() {
        crate::run_with_runtime(async {
            let path = std::env::var("ROBBIE_TEST_FIFO")?;
            let mut terminal = ratatui::init();
            let result = async {
                let _mouse = MouseCapture;
                execute!(std::io::stdout(), EnableMouseCapture)?;
                let mut app = App { status: "Blocked read test".into(), ..App::default() };
                app.push((0..60).map(|n| format!("history {n:02}")).collect::<Vec<_>>().join("\n"), Style::new());
                let mut keys = EventStream::new();
                let (_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let mut tools = crate::tools::ToolRegistry::new();
                let work = async {
                    let (output, error) = tools.execute("read_file", &serde_json::json!({"path": path})).await;
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
    fn input_keys_submit_and_preserve_unicode() {
        let mut app = App::default();
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('猫'), KeyModifiers::NONE))), InputAction::None);
        assert_eq!(app.input, "猫");
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))), InputAction::Submit);
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))), InputAction::None);
        assert!(app.input.is_empty());
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))), InputAction::None);
        assert_eq!(input_tail("ab猫", 2), "猫");
        assert_eq!(input_tail("ab猫", 1), "");
    }

    #[test]
    fn input_actions_reject_invalid_submissions_and_allow_quitting() {
        let mut app = App { input: "   ".into(), ..App::default() };
        let enter = || Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.handle_event(enter()), InputAction::None);
        app.input = "hello".into();
        let mut released = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        released.kind = KeyEventKind::Release;
        assert_eq!(app.handle_event(Event::Key(released)), InputAction::None);
        assert_eq!(app.handle_event(enter()), InputAction::Submit);
        app.failed = true;
        assert_eq!(app.handle_event(enter()), InputAction::None);
        assert_eq!(app.handle_event(Event::Resize(80, 24)), InputAction::None);
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))), InputAction::Quit);
    }

    #[test]
    fn events_update_status_and_strip_control_characters() {
        let mut app = App::default();
        app.activity(AgentEvent { source: "subagent", kind: EventKind::ModelStarted });
        assert_eq!(app.status, "subagent: waiting for model…");
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "bash".into(), output: "hello\r\n\u{1b}[31m".into(), is_error: true,
        }});
        assert!(!app.transcript[0].0.contains('\u{1b}'));
        assert!(app.transcript[0].0.contains("hello\n"));
    }

    #[test]
    fn mouse_wheel_moves_visible_transcript_and_stops_at_bounds() -> Result<()> {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut app = App::default();
        app.push((0..40).map(|n| format!("row {n:02}")).collect::<Vec<_>>().join("\n"), Style::new());
        let mut terminal = Terminal::new(TestBackend::new(80, 12))?;
        let screen = |terminal: &Terminal<TestBackend>| -> String {
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
        };
        let wheel = |kind| Event::Mouse(MouseEvent {
            kind, column: 5, row: 3, modifiers: KeyModifiers::NONE,
        });
        terminal.draw(|frame| app.view(frame))?;
        let bottom = screen(&terminal);
        assert!(bottom.contains("row 39"));
        assert_eq!(app.handle_event(wheel(MouseEventKind::ScrollUp)), InputAction::None);
        terminal.draw(|frame| app.view(frame))?;
        assert_ne!(screen(&terminal), bottom, "wheel up must change visible rows");
        assert!(!screen(&terminal).contains("row 39"));
        app.handle_event(wheel(MouseEventKind::ScrollDown));
        terminal.draw(|frame| app.view(frame))?;
        assert_eq!(screen(&terminal), bottom);
        app.handle_event(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll_back, 0);
        for _ in 0..100 { app.handle_event(wheel(MouseEventKind::ScrollUp)); }
        terminal.draw(|frame| app.view(frame))?;
        assert!(screen(&terminal).contains("row 00"));
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))), InputAction::None);
        terminal.draw(|frame| app.view(frame))?;
        assert_eq!(screen(&terminal), bottom);
        Ok(())
    }

    #[test]
    fn renders_at_normal_and_tiny_sizes_and_clamps_scroll() -> Result<()> {
        for (width, height) in [(80, 24), (10, 5), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            let mut app = App::default();
            app.push("Hello from Robbie".into(), Style::new());
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
