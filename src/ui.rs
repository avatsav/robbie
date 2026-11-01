use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    cursor::Show,
    style::{Attribute, ResetColor, SetAttribute},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    DefaultTerminal, Frame, Terminal, TerminalOptions, Viewport,
    backend::{Backend, ClearType, CrosstermBackend},
    layout::Rect,
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Paragraph, Widget},
};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{Agent, events::{AgentEvent, EventKind}};

type StyledLine = (String, Style);

const USER_ORANGE: Color = Color::Rgb(217, 139, 92);
// Explicit colours, not the terminal-dependent DIM/faint attribute.
const MUTED_TEXT: Color = Color::Rgb(130, 132, 138);
const MUTED_CYAN: Color = Color::Rgb(0, 120, 130);
const MUTED_RED: Color = Color::Rgb(180, 95, 95);
const MUTED_GREEN: Color = Color::Rgb(90, 150, 100);
const BASH_PREVIEW_LINES: usize = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ToolOutputMode {
    #[default]
    Concise,
    Relaxed,
    Full,
}

impl ToolOutputMode {
    fn next(self) -> Self {
        match self {
            Self::Concise => Self::Relaxed,
            Self::Relaxed => Self::Full,
            Self::Full => Self::Concise,
        }
    }

}

pub(super) struct ToolOutcome {
    failed: bool,
    footer: Option<String>,
}

// Conversation text wraps; tool headers and preview rows stay compact and muted.
pub(super) enum OutputLine {
    Banner { model: String },
    BannerHint { text: &'static str },
    Wrapped(StyledLine),
    Tool { summary: String, outcome: ToolOutcome },
    ToolOutput { prefix: &'static str, text: String },
}

impl OutputLine {
    pub(super) fn rows(&self, width: usize) -> Vec<Line<'static>> {
        match self {
            Self::Banner { model } => banner_rows(model, width),
            Self::BannerHint { text } => banner_hint_rows(text, width),
            Self::Wrapped((text, style)) => textwrap::wrap(text, width.max(1)).into_iter()
                .map(|line| Line::styled(line.into_owned(), *style)).collect(),
            Self::Tool { summary, outcome } => vec![tool_line(summary, Some(outcome), width)],
            Self::ToolOutput { prefix, text } => {
                let prefix = single_line(&format!("{prefix} "), width);
                let content = single_line(text, width.saturating_sub(prefix.width()));
                vec![Line::from(vec![prefix.fg(MUTED_CYAN), content.fg(MUTED_TEXT)])]
            }
        }
    }

    #[cfg(test)]
    fn text(&self) -> &str {
        match self {
            Self::Banner { model } => model,
            Self::BannerHint { text } => text,
            Self::Wrapped((text, _)) => text,
            Self::Tool { summary, .. } => summary,
            Self::ToolOutput { text, .. } => text,
        }
    }
}

struct ToolActivity {
    summary: String,
    input: Value,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum InputAction {
    None,
    Submit,
    Quit,
}

#[derive(Default)]
pub(super) struct App {
    input: String,
    pending: Vec<OutputLine>,
    status: String,
    started: Option<Instant>,
    tick: usize,
    failed: bool,
    // A child can run tools while the parent's sub_agent call is still active.
    active_tools: HashMap<&'static str, ToolActivity>,
    output_mode: ToolOutputMode,
    // Survives draining pending rows into either frontend's history.
    needs_text_separator: bool,
}

impl App {
    // Both terminal frontends share event formatting, input and live-row styling.
    pub(super) fn take_output(&mut self) -> Vec<OutputLine> {
        std::mem::take(&mut self.pending)
    }

    pub(super) fn advance_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    fn active_tool(&self) -> Option<&ToolActivity> {
        self.active_tools.get("subagent").or_else(|| self.active_tools.get("main"))
    }

    pub(super) fn banner(&mut self, model: &str) {
        self.pending.push(OutputLine::Banner { model: model.to_string() });
        self.pending.push(OutputLine::BannerHint { text: "Enter send · Ctrl+D/C exit · Ctrl+O cycle tool output" });
        self.pending.push(OutputLine::BannerHint { text: "Shell commands run without approval. Use a trusted workspace." });
        self.blank();
    }

    fn push(&mut self, text: impl Into<String>, style: Style) {
        let text = clean(&text.into());
        for line in text.split('\n') {
            self.pending.push(OutputLine::Wrapped((line.to_string(), style)));
        }
    }

    fn blank(&mut self) {
        self.pending.push(OutputLine::Wrapped((String::new(), Style::new())));
        self.needs_text_separator = false;
    }

    pub(super) fn submit(&mut self) -> String {
        let prompt = std::mem::take(&mut self.input);
        self.push(format!("▌ {prompt}"), Style::new().fg(USER_ORANGE));
        self.blank();
        self.started = Some(Instant::now());
        self.status = "waiting for model…".into();
        prompt
    }

    pub(super) fn finish_turn(&mut self, result: Result<String>) {
        self.started = None;
        self.active_tools.clear();
        match result {
            Ok(_) => self.status.clear(),
            Err(error) => {
                self.push(format!("[error] {error:#}"), Style::new().red());
                self.blank();
                self.failed = true;
                self.status = "Session failed; restart Robbie. Ctrl+D/C to exit.".into();
            }
        }
    }

    pub(super) fn activity(&mut self, event: AgentEvent) {
        let source = event.source;
        match event.kind {
            EventKind::ModelStarted => {
                self.status = if source == "main" { "waiting for model…".into() } else { "agent: waiting for model…".into() };
            }
            EventKind::AssistantText(text) => {
                if self.needs_text_separator { self.blank(); }
                if source == "main" {
                    self.push(text, Style::new());
                } else {
                    self.push(format!("[agent] {text}"), Style::new());
                }
                self.blank();
            }
            EventKind::ToolStarted { name, input } => {
                let label = tool_label(&name);
                let prefix = if source == "main" { label.to_string() } else { format!("agent/{label}") };
                let summary = format!("[{prefix}] {}", tool_summary(&name, &input));
                self.status = format!("running {prefix}…");
                self.active_tools.insert(source, ToolActivity { summary, input });
            }
            EventKind::ToolFinished { name, output, is_error } => {
                let activity = self.active_tools.remove(source);
                let summary = activity.as_ref().map(|tool| tool.summary.clone())
                    .unwrap_or_else(|| format!("[{}]", tool_label(&name)));
                let footer = if name == "bash" && self.output_mode == ToolOutputMode::Concise {
                    bash_footer(&output).or_else(|| is_error.then(|| "[no exit code]".into()))
                } else {
                    None
                };
                self.pending.push(OutputLine::Tool { summary, outcome: ToolOutcome { failed: is_error, footer } });
                self.needs_text_separator = true;
                match self.output_mode {
                    ToolOutputMode::Concise => {}
                    ToolOutputMode::Relaxed => {
                        if name == "bash" {
                            self.pending.extend(bash_preview(&output));
                            self.blank();
                        } else if is_error {
                            self.pending.push(OutputLine::ToolOutput {
                                prefix: "└",
                                text: clean(&output).lines().next().unwrap_or("tool failed").to_string(),
                            });
                        }
                    }
                    ToolOutputMode::Full => {
                        self.push(output, Style::new().fg(MUTED_TEXT));
                        if !is_error && let Some(activity) = activity {
                            let input = activity.input;
                            let mut changes = Vec::new();
                            if name == "edit_file" {
                                diff_lines(&mut changes, input["old_text"].as_str().unwrap_or(""), "- ", Style::new().fg(MUTED_RED));
                                diff_lines(&mut changes, input["new_text"].as_str().unwrap_or(""), "+ ", Style::new().fg(MUTED_GREEN));
                            } else if name == "write_file" {
                                diff_lines(&mut changes, input["content"].as_str().unwrap_or(""), "+ ", Style::new().fg(MUTED_GREEN));
                            }
                            self.pending.extend(changes.into_iter().map(OutputLine::Wrapped));
                        }
                        self.blank();
                    }
                }
                self.status = "waiting for model…".into();
            }
        }
    }

    pub(super) fn handle_event(&mut self, event: Event) -> InputAction {
        // Paste is inserted as text, never interpreted as submit/quit keys.
        if let Event::Paste(text) = event {
            self.input.push_str(&clean(&text).replace('\n', " "));
            return InputAction::None;
        }
        let Event::Key(key) = event else { return InputAction::None };
        if key.kind == KeyEventKind::Release { return InputAction::None }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => InputAction::Quit,
                KeyCode::Char('d') if self.input.is_empty() => InputAction::Quit,
                KeyCode::Char('o') => {
                    self.output_mode = self.output_mode.next();
                    InputAction::None
                },
                _ => InputAction::None,
            };
        }
        match key.code {
            KeyCode::Enter if !self.failed && !self.input.trim().is_empty() => InputAction::Submit,
            KeyCode::Backspace => { self.input.pop(); InputAction::None }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) && !c.is_control() => {
                self.input.push(c);
                InputAction::None
            }
            _ => InputAction::None,
        }
    }

    fn view(&self, frame: &mut Frame) {
        self.view_live(frame, frame.area());
    }

    pub(super) fn view_live(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 { return }
        let prefix = if area.width >= 2 { "❯ " } else { "" };
        let tail = input_tail(&self.input, area.width.saturating_sub(prefix.width() as u16).saturating_sub(1) as usize);
        let prompt = Line::from(vec![prefix.cyan(), tail.into()]);
        let prompt_y = area.bottom() - 1;
        frame.render_widget(prompt, Rect::new(area.x, prompt_y, area.width, 1));
        frame.set_cursor_position((area.x + (prefix.width() + tail.width()) as u16, prompt_y));
        if area.height > 1 {
            let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'][self.tick % 10];
            // Prefer the child's current tool, then restore the parent call's
            // running row when that child tool finishes.
            if let Some(tool) = self.active_tool() {
                frame.render_widget(tool_line(&tool.summary, None, area.width as usize), Rect::new(area.x, area.y, area.width, 1));
            } else {
                let status = if let Some(start) = self.started {
                    format!("{spinner} {}s · {}", start.elapsed().as_secs(), self.status)
                } else {
                    self.status.clone()
                };
                frame.render_widget(Line::from(single_line(&status, area.width as usize)).fg(MUTED_TEXT), Rect::new(area.x, area.y, area.width, 1));
            }
        }
    }
}

fn banner_rows(model: &str, width: usize) -> Vec<Line<'static>> {
    if width < 3 {
        return vec![Line::from(single_line("▌ robbie", width)).cyan()];
    }
    let text = format!("robbie {} › openrouter · {}", env!("CARGO_PKG_VERSION"), clean(model).replace('\n', " "));
    let mut brand_remaining = "robbie".len();
    textwrap::wrap(&text, width - 2).into_iter().map(|part| {
        // Only the ASCII brand is bold, even if a very narrow terminal wraps it.
        let bold_len = brand_remaining.min(part.len());
        brand_remaining -= bold_len;
        Line::from(vec![
            "▌ ".cyan(),
            part[..bold_len].to_string().white().bold(),
            part[bold_len..].to_string().white(),
        ])
    }).collect()
}

fn banner_hint_rows(text: &str, width: usize) -> Vec<Line<'static>> {
    if width < 3 {
        return vec![Line::from(single_line("▌", width)).cyan()];
    }
    textwrap::wrap(text, width - 2).into_iter().map(|part| {
        Line::from(vec!["▌ ".cyan(), part.into_owned().fg(MUTED_TEXT)])
    }).collect()
}

fn tool_line(summary: &str, outcome: Option<&ToolOutcome>, width: usize) -> Line<'static> {
    let failed = outcome.is_some_and(|outcome| outcome.failed);
    let footer = outcome.and_then(|outcome| outcome.footer.as_deref())
        .map(|footer| format!(" {footer}")).unwrap_or_default();
    // Preserve the actual outcome even when the command itself is very long.
    let footer = input_tail(&footer, width).to_string();
    let head_width = width.saturating_sub(footer.width());
    let marker = if failed && head_width >= 2 { "• " } else if failed && head_width == 1 { "•" } else { "" };
    let summary = single_line(summary, head_width.saturating_sub(marker.width()));
    let mut spans = vec![marker.fg(MUTED_RED)];
    if let Some((label, arguments)) = summary.split_once("] ") {
        spans.extend([format!("{label}] ").fg(MUTED_CYAN), arguments.to_string().fg(MUTED_TEXT)]);
    } else {
        spans.push(summary.fg(MUTED_CYAN));
    }
    spans.push(footer.fg(if failed { MUTED_RED } else { MUTED_TEXT }));
    Line::from(spans)
}

fn bash_footer(output: &str) -> Option<String> {
    let output = clean(output);
    let last = output.lines().last()?;
    if last.ends_with(']') && (last.starts_with("[exit ") || last.starts_with("[signal:")) {
        Some(last.to_string())
    } else {
        None
    }
}

fn bash_preview(output: &str) -> Vec<OutputLine> {
    let output = clean(output);
    let lines: Vec<_> = output.lines().collect();
    let skip = lines.len().saturating_sub(BASH_PREVIEW_LINES);
    let mut preview = Vec::new();
    if skip > 0 {
        preview.push(OutputLine::ToolOutput { prefix: "┌", text: "… earlier output omitted".into() });
    }
    for (index, line) in lines.iter().enumerate().skip(skip) {
        let prefix = if index + 1 == lines.len() { "└" } else if index == 0 { "┌" } else { "│" };
        preview.push(OutputLine::ToolOutput { prefix, text: line.to_string() });
    }
    preview
}

fn clean(text: &str) -> String {
    text.replace('\t', "    ").chars().filter(|c| !c.is_control() || *c == '\n').collect()
}

fn tool_label(name: &str) -> &str {
    match name {
        "read_file" => "read",
        "write_file" => "write",
        "edit_file" => "edit",
        "sub_agent" => "agent",
        _ => name,
    }
}

fn tool_summary(name: &str, input: &Value) -> String {
    let value = match name {
        "bash" => input["command"].as_str(),
        "read_file" | "write_file" | "edit_file" => input["path"].as_str(),
        "sub_agent" => input["prompt"].as_str(),
        "todo" => input["action"].as_str(),
        _ => None,
    };
    let text = value.map(str::to_string).unwrap_or_else(|| input.to_string());
    clean(&text).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn single_line(text: &str, width: usize) -> String {
    let text = clean(text).replace('\n', " ");
    if text.width() <= width { return text }
    if width == 0 { return String::new() }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cells = ch.width().unwrap_or(0);
        if used + cells > width - 1 { break }
        result.push(ch);
        used += cells;
    }
    result.push('…');
    result
}

fn diff_lines(lines: &mut Vec<StyledLine>, text: &str, prefix: &str, style: Style) {
    for line in clean(text).lines() {
        lines.push((format!("  {prefix}{line}"), style));
    }
}

fn input_tail(input: &str, width: usize) -> &str {
    let mut used = 0;
    for (index, ch) in input.char_indices().rev() {
        used += ch.width().unwrap_or(0);
        if used > width { return &input[index + ch.len_utf8()..] }
    }
    input
}

fn inline_options() -> TerminalOptions {
    TerminalOptions { viewport: Viewport::Inline(2) }
}

// Only unflushed output lives in memory. Inserted lines belong to the terminal's
// normal scrollback, not to a second full-screen transcript or a mouse handler.
fn flush_output<B: Backend>(terminal: &mut Terminal<B>, pending: &mut Vec<OutputLine>) -> Result<(), B::Error> {
    let width = terminal.size()?.width.max(1) as usize;
    let mut rows = Vec::new();
    for line in pending.drain(..) {
        rows.extend(line.rows(width));
    }
    // Batch insertions, but bound temporary buffers even for a long response.
    for chunk in rows.chunks(128) {
        terminal.insert_before(chunk.len() as u16, |buffer| {
            Paragraph::new(chunk.to_vec()).render(buffer.area, buffer);
        })?;
    }
    Ok(())
}

struct InlineTerminal {
    terminal: DefaultTerminal,
    size: ratatui::layout::Size,
    _mode: NativeMode,
}

impl InlineTerminal {
    fn new() -> Result<Self> {
        let mode = NativeMode::new()?;
        let terminal = Terminal::with_options(CrosstermBackend::new(io::stdout()), inline_options())?;
        let size = terminal.size()?;
        Ok(Self { terminal, size, _mode: mode })
    }

    fn prepare(&mut self) -> Result<()> {
        let size = self.terminal.size()?;
        let old_height = self.terminal.get_frame().area().height;
        if size != self.size {
            // Ratatui 0.30's normal horizontal-shrink path clears the entire
            // screen. Re-anchor just the live viewport instead, keeping history.
            let cursor = self.terminal.get_cursor_position()?;
            let prompt_offset = old_height.saturating_sub(1);
            self.terminal.set_cursor_position((0, cursor.y.saturating_sub(prompt_offset)))?;
            self.terminal.backend_mut().clear_region(ClearType::AfterCursor)?;
            self.terminal = Terminal::with_options(CrosstermBackend::new(io::stdout()), inline_options())?;
            self.size = size;
        }
        Ok(())
    }

    fn draw(&mut self, app: &mut App) -> Result<()> {
        self.prepare()?;
        flush_output(&mut self.terminal, &mut app.pending)?;
        // An insertion can wait for terminal replies; the window may have been
        // resized meanwhile. Re-anchor before draw's automatic resize path.
        self.prepare()?;
        self.terminal.draw(|frame| app.view(frame))?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let origin = self.terminal.get_frame().area().as_position();
        // Cleanup needs no terminal round-trip; clear only our live viewport.
        self.terminal.set_cursor_position(origin)?;
        self.terminal.backend_mut().clear_region(ClearType::AfterCursor)?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

// Native mode must never use ratatui::restore(): it sends LeaveAlternateScreen
// even when we never entered one, restoring a stale cursor in some terminals.
// A shell redraw can then erase the recent conversation from that cursor onward.
struct NativeMode;

impl NativeMode {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mode = Self;
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_native_mode();
            previous_hook(info);
        }));
        execute!(io::stdout(), EnableBracketedPaste)?;
        Ok(mode)
    }
}

impl Drop for NativeMode {
    fn drop(&mut self) {
        restore_native_mode();
    }
}

fn restore_native_mode() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), DisableBracketedPaste, Show, ResetColor, SetAttribute(Attribute::Reset));
}

pub async fn run(mut agent: Agent, mut receiver: UnboundedReceiver<AgentEvent>) -> Result<()> {
    match InlineTerminal::new() {
        Ok(mut terminal) => {
            let result = async {
                let mut app = App::default();
                app.banner(&agent.model);
                run_app(&mut terminal, &mut app, &mut agent, &mut receiver).await
            }.await;
            result.and(terminal.finish())
        }
        Err(error) => Err(error),
    }
}

async fn run_app(terminal: &mut InlineTerminal, app: &mut App, agent: &mut Agent, receiver: &mut UnboundedReceiver<AgentEvent>) -> Result<()> {
    loop {
        terminal.draw(app)?;
        let event = next_terminal_event().await.context("terminal input failed")?;
        match app.handle_event(event) {
            InputAction::Quit => return Ok(()),
            InputAction::None => {}
            InputAction::Submit => {
                let prompt = app.submit();
                let result = run_turn(terminal, app, receiver, agent.run(prompt.trim())).await?;
                match result {
                    TurnOutcome::Finished(result) => app.finish_turn(result),
                    TurnOutcome::Quit => return Ok(()),
                }
            }
        }
    }
}

enum TurnOutcome {
    Finished(Result<String>),
    Quit,
}

// Inline rendering also reads cursor-position replies from the terminal. Do not
// use EventStream here: its background reader can hold Crossterm's input lock
// while a synchronous cursor query waits for that same lock. Poll ready keyboard
// events in this task instead, so terminal reads and renders never compete.
async fn next_terminal_event() -> io::Result<Event> {
    loop {
        if event::poll(Duration::ZERO)? {
            return event::read();
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}

async fn run_turn(
    terminal: &mut InlineTerminal,
    app: &mut App,
    receiver: &mut UnboundedReceiver<AgentEvent>,
    run: impl std::future::Future<Output = Result<String>>,
) -> Result<TurnOutcome> {
    tokio::pin!(run);
    let mut ticks = tokio::time::interval(Duration::from_millis(100));
    loop {
        terminal.draw(app)?;
        tokio::select! {
            result = &mut run => {
                while let Ok(event) = receiver.try_recv() { app.activity(event); }
                return Ok(TurnOutcome::Finished(result));
            }
            Some(event) = receiver.recv() => app.activity(event),
            _ = ticks.tick() => app.advance_tick(),
            event = next_terminal_event() => {
                let event = event.context("terminal input failed")?;
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
    use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use serde_json::json;

    #[test]
    fn banner_brand_is_bold_white_and_metadata_is_regular_white() -> Result<()> {
        use ratatui::style::Modifier;
        let mut terminal = Terminal::with_options(TestBackend::new(100, 12), inline_options())?;
        let mut app = App::default();
        app.banner("test/model");
        flush_output(&mut terminal, &mut app.pending)?;
        terminal.draw(|frame| app.view(frame))?;
        let buffer = terminal.backend().buffer();
        for x in 2..8 {
            assert_eq!(buffer[(x, 0)].fg, Color::White);
            assert!(buffer[(x, 0)].modifier.contains(Modifier::BOLD));
        }
        for x in 9..38 {
            assert_eq!(buffer[(x, 0)].fg, Color::White);
            assert!(!buffer[(x, 0)].modifier.contains(Modifier::BOLD));
        }
        for y in 0..3 {
            assert_eq!(buffer[(0, y)].fg, Color::Cyan);
        }
        assert!(!buffer.content.iter().any(|cell| cell.modifier.contains(Modifier::DIM)));
        Ok(())
    }

    #[test]
    fn input_actions_and_paste_do_not_accidentally_submit() {
        let mut app = App::default();
        let enter = || Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.handle_event(enter()), InputAction::None);
        assert_eq!(app.handle_event(Event::Paste("hello\n猫\u{3}".into())), InputAction::None);
        assert_eq!(app.input, "hello 猫");
        assert_eq!(app.handle_event(enter()), InputAction::Submit);
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))), InputAction::None);
        app.failed = true;
        assert_eq!(app.handle_event(enter()), InputAction::None);
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))), InputAction::Quit);
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))), InputAction::None);
        assert_eq!(app.output_mode, ToolOutputMode::Relaxed);
        app.input.clear();
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))), InputAction::Quit);
        assert_eq!(app.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: 0, row: 0, modifiers: KeyModifiers::NONE })), InputAction::None);
        assert_eq!(input_tail("ab猫", 2), "猫");
    }

    #[test]
    fn ctrl_o_cycles_modes_without_rewriting_history_or_draft() -> Result<()> {
        let mut app = App { input: "next prompt".into(), ..App::default() };
        app.push("already printed output", Style::new());
        let before: Vec<_> = app.pending.iter().map(|line| line.text().to_string()).collect();
        let mut terminal = Terminal::with_options(TestBackend::new(80, 6), inline_options())?;
        assert_eq!(app.output_mode, ToolOutputMode::Concise);
        for expected in [ToolOutputMode::Relaxed, ToolOutputMode::Full, ToolOutputMode::Concise] {
            assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))), InputAction::None);
            assert_eq!(app.output_mode, expected);
            assert_eq!(app.input, "next prompt");
            assert_eq!(app.pending.iter().map(|line| line.text().to_string()).collect::<Vec<_>>(), before);
            terminal.draw(|frame| app.view(frame))?;
            let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("next prompt"));
            assert!(!text.contains("tools:"), "output mode must not appear in the prompt");
        }
        Ok(())
    }

    #[test]
    fn concise_bash_shows_exit_code_and_a_text_dot_only_on_failure() {
        for code in [0, 7] {
            let mut app = App { output_mode: ToolOutputMode::Concise, ..App::default() };
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                name: "bash".into(), input: json!({"command":"a very long command ".repeat(20)}),
            }});
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                name: "bash".into(), output: format!("hidden output\n[exit {code}]"), is_error: code != 0,
            }});
            assert_eq!(app.pending.len(), 1);
            let rows = app.pending[0].rows(36);
            let text: String = rows[0].spans.iter().map(|span| span.content.as_ref()).collect();
            assert!(text.ends_with(&format!("[exit {code}]")));
            assert!(text.contains('…'), "command must truncate before outcome");
            assert_eq!(text.starts_with("• "), code != 0);
            assert!(rows[0].width() <= 36);
            if code != 0 { assert_eq!(rows[0].spans[0].style.fg, Some(MUTED_RED)); }
            for width in 0..40 {
                assert!(app.pending[0].rows(width)[0].width() <= width);
            }
        }
        assert_eq!(bash_footer("Command timed out after 120 seconds"), None);
        assert_eq!(bash_footer("(no output)\n[signal: 9 (SIGKILL)]"), Some("[signal: 9 (SIGKILL)]".into()));
        let mut app = App { output_mode: ToolOutputMode::Concise, ..App::default() };
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "bash".into(), output: "Command timed out after 120 seconds".into(), is_error: true,
        }});
        assert!(app.pending[0].rows(40)[0].to_string().ends_with("[no exit code]"));
    }

    #[test]
    fn full_bash_shows_all_returned_lines_without_preview_truncation() {
        let mut app = App { output_mode: ToolOutputMode::Full, ..App::default() };
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
            name: "bash".into(), input: json!({"command":"some command"}),
        }});
        let output = (0..20).map(|n| format!("output {n:02}")).collect::<Vec<_>>().join("\n") + "\n[exit 0]";
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "bash".into(), output, is_error: false,
        }});
        assert!(app.pending.iter().any(|line| line.text() == "output 00"));
        assert!(app.pending.iter().any(|line| line.text() == "output 19"));
        assert!(app.pending.iter().any(|line| line.text() == "[exit 0]"));
        assert!(!app.pending.iter().any(|line| line.text().contains("omitted")));
    }

    #[test]
    fn mode_changes_while_busy_apply_to_the_next_completed_result() {
        let mut app = App { output_mode: ToolOutputMode::Relaxed, ..App::default() };
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
            name: "read_file".into(), input: json!({"path":"Cargo.toml"}),
        }});
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)));
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "read_file".into(), output: "dependencies".into(), is_error: false,
        }});
        assert!(app.pending.iter().any(|line| line.text() == "dependencies"));
        app.pending.clear();
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)));
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
            name: "read_file".into(), input: json!({"path":"Cargo.toml"}),
        }});
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "read_file".into(), output: "dependencies".into(), is_error: false,
        }});
        assert_eq!(app.pending.len(), 1);
    }

    #[test]
    fn bash_shows_a_capped_tail_and_preserves_the_exit_code() {
        let mut app = App { output_mode: ToolOutputMode::Relaxed, ..App::default() };
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
            name: "bash".into(), input: json!({"command":"some command"}),
        }});
        let output = (0..20).map(|n| format!("output {n:02}")).collect::<Vec<_>>().join("\n") + "\n[exit 7]";
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "bash".into(), output, is_error: true,
        }});
        assert!(app.pending.iter().any(|line| line.text() == "output 19"));
        assert!(app.pending.iter().any(|line| line.text() == "[exit 7]"));
        assert!(!app.pending.iter().any(|line| line.text() == "output 00"));
        assert!(app.pending.len() <= 10);
    }

    #[test]
    fn every_tool_has_one_compact_header() {
        for name in ["bash", "read_file", "write_file", "edit_file", "todo", "sub_agent"] {
            for is_error in [false, true] {
                let mut app = App::default();
                app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                    name: name.into(), input: json!({"command":"printf hello", "path":"file.rs", "old_text":"old", "new_text":"new"}),
                }});
                assert!(app.pending.is_empty(), "running {name} must stay in the live area");
                app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                    name: name.into(), output: "detail one\ndetail two".into(), is_error,
                }});
                assert_eq!(app.pending.iter().filter(|line| matches!(line, OutputLine::Tool { .. })).count(), 1, "completed {name} must produce one header");
            }
        }
    }

    #[test]
    fn conversation_has_priority_over_muted_single_line_tools() -> Result<()> {
        use ratatui::style::Modifier;
        let mut app = App { input: "user prompt".into(), ..App::default() };
        app.submit();
        assert_eq!(app.pending[0].rows(80)[0].style.fg, Some(USER_ORANGE));
        assert!(!app.pending[0].rows(80)[0].style.add_modifier.contains(Modifier::DIM));
        app.pending.clear();
        app.activity(AgentEvent { source: "main", kind: EventKind::AssistantText("assistant answer".into()) });
        assert_eq!(app.pending[0].rows(80)[0].style, Style::new());
        app.pending.clear();
        let mut terminal = Terminal::with_options(TestBackend::new(18, 6), inline_options())?;
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
            name: "bash".into(), input: json!({"command":"printf 'a long command'"}),
        }});
        terminal.draw(|frame| app.view(frame))?;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].fg, MUTED_CYAN);
        assert_eq!(buffer[(8, 0)].fg, MUTED_TEXT);
        assert!(!buffer.content.iter().any(|cell| cell.modifier.contains(Modifier::DIM)));
        assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
        assert_eq!(buffer[(17, 0)].symbol(), "…");
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "bash".into(), output: "hello\n[exit 0]".into(), is_error: false,
        }});
        assert_eq!(app.pending[0].rows(18).len(), 1);
        flush_output(&mut terminal, &mut app.pending)?;
        terminal.draw(|frame| app.view(frame))?;
        assert!(terminal.backend().buffer().content.iter().all(|cell| cell.bg == Color::Reset));
        for width in 0..20 {
            let row = OutputLine::ToolOutput { prefix: "└", text: "a long line with 猫".into() };
            assert!(row.rows(width)[0].width() <= width);
        }
        assert_eq!(single_line("猫猫猫", 4), "猫…");
        Ok(())
    }

    #[test]
    fn parent_tool_stays_running_while_child_tools_complete() -> Result<()> {
        let mut terminal = Terminal::with_options(TestBackend::new(80, 6), inline_options())?;
        let mut app = App::default();
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
            name: "sub_agent".into(), input: json!({"prompt":"inspect dependencies"}),
        }});
        app.activity(AgentEvent { source: "subagent", kind: EventKind::ToolStarted {
            name: "read_file".into(), input: json!({"path":"Cargo.toml"}),
        }});
        terminal.draw(|frame| app.view(frame))?;
        let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("[agent/read] Cargo.toml"));
        app.activity(AgentEvent { source: "subagent", kind: EventKind::ToolFinished {
            name: "read_file".into(), output: "dependencies".into(), is_error: false,
        }});
        terminal.draw(|frame| app.view(frame))?;
        let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("[agent] inspect dependencies"));
        assert_eq!(terminal.backend().buffer()[(0, 0)].bg, Color::Reset);
        assert_eq!(app.pending.len(), 1);
        Ok(())
    }

    #[test]
    fn reads_show_their_contents_only_in_full_mode() {
        for mode in [ToolOutputMode::Concise, ToolOutputMode::Relaxed, ToolOutputMode::Full] {
            let mut app = App { output_mode: mode, ..App::default() };
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                name: "read_file".into(), input: json!({"path":"Cargo.toml"}),
            }});
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                name: "read_file".into(), output: "file contents".into(), is_error: false,
            }});
            assert_eq!(app.pending[0].text(), "[read] Cargo.toml");
            assert_eq!(app.pending.iter().any(|line| line.text().contains("file contents")), mode == ToolOutputMode::Full);
        }
    }

    #[test]
    fn edit_details_follow_the_output_mode() {
        for mode in [ToolOutputMode::Concise, ToolOutputMode::Relaxed, ToolOutputMode::Full] {
            let mut app = App { output_mode: mode, ..App::default() };
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                name: "edit_file".into(), input: json!({"path":"a.rs", "old_text":"old", "new_text":"new"}),
            }});
            assert!(app.pending.is_empty());
            app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                name: "edit_file".into(), output: "File edited".into(), is_error: false,
            }});
            assert_eq!(app.pending.iter().any(|line| line.text() == "  - old"), mode == ToolOutputMode::Full);
            assert_eq!(app.pending.iter().any(|line| line.text() == "  + new"), mode == ToolOutputMode::Full);
        }
    }

    #[test]
    fn output_goes_to_native_scrollback_and_is_not_reinserted() -> Result<()> {
        let mut terminal = Terminal::with_options(TestBackend::new(30, 6), inline_options())?;
        let mut app = App::default();
        app.push((0..30).map(|n| format!("history {n:02}")).collect::<Vec<_>>().join("\n"), Style::new());
        flush_output(&mut terminal, &mut app.pending)?;
        terminal.draw(|frame| app.view(frame))?;
        assert!(app.pending.is_empty());
        let scrollback = terminal.backend().scrollback().clone();
        let text: String = scrollback.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("history 00"));
        flush_output(&mut terminal, &mut app.pending)?;
        terminal.draw(|frame| app.view(frame))?;
        assert_eq!(terminal.backend().scrollback(), &scrollback);
        Ok(())
    }

    #[test]
    fn inline_ui_keeps_existing_shell_output() -> Result<()> {
        let mut backend = TestBackend::new(80, 24);
        let existing = ratatui::buffer::Buffer::with_lines(["existing shell output"]);
        backend.draw(existing.content.iter().enumerate().map(|(x, cell)| (x as u16, 0, cell)))?;
        backend.set_cursor_position((0, 2))?;
        let mut terminal = Terminal::with_options(backend, inline_options())?;
        let mut app = App::default();
        app.banner("test/model");
        flush_output(&mut terminal, &mut app.pending)?;
        terminal.draw(|frame| app.view(frame))?;
        let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.starts_with("existing shell output"));
        assert!(text.contains("▌ robbie"));
        assert!(text.contains("❯"));
        assert!(!text.contains('┌'));
        Ok(())
    }

    #[test]
    fn failed_edits_show_errors_not_success_previews() {
        let mut app = App { output_mode: ToolOutputMode::Relaxed, ..App::default() };
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolStarted {
            name: "edit_file".into(), input: json!({"path":"a.rs", "old_text":"old", "new_text":"new"}),
        }});
        app.activity(AgentEvent { source: "main", kind: EventKind::ToolFinished {
            name: "edit_file".into(), output: "old_text did not match".into(), is_error: true,
        }});
        assert_eq!(app.pending.len(), 2);
        assert_eq!(app.pending[0].text(), "[edit] a.rs");
        assert!(app.pending.iter().any(|line| line.text().contains("old_text did not match")));
        assert!(!app.pending.iter().any(|line| line.text() == "  + new"));
    }

    #[test]
    fn finishing_a_turn_preserves_the_next_draft_and_rejects_failed_sessions() {
        let mut app = App { input: "first prompt".into(), ..App::default() };
        assert_eq!(app.submit(), "first prompt");
        assert!(app.started.is_some());
        app.handle_event(Event::Paste("next draft".into()));
        app.finish_turn(Ok("answer".into()));
        assert_eq!(app.input, "next draft");
        assert!(app.started.is_none());
        app.finish_turn(Err(anyhow::anyhow!("request failed")));
        assert!(app.failed);
        assert_eq!(app.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))), InputAction::None);
        assert!(app.pending.iter().any(|line| line.text().contains("request failed")));
    }

    #[test]
    fn prompt_fits_tiny_and_normal_viewports() -> Result<()> {
        for (width, height) in [(80, 24), (10, 5), (1, 1)] {
            let mut terminal = Terminal::with_options(TestBackend::new(width, height), inline_options())?;
            let mut app = App { input: "hello猫".into(), ..App::default() };
            app.push("hello\u{1b}[31m\r\nworld", Style::new());
            assert!(!app.pending[0].text().contains('\u{1b}'));
            flush_output(&mut terminal, &mut app.pending)?;
            terminal.draw(|frame| app.view(frame))?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a PTY; run via tests/blocked_tool_exit.py"]
    fn completed_read_terminal_fixture() {
        crate::run_with_runtime(async {
            let mut terminal = InlineTerminal::new()?;
            let result = async {
                let mut app = App { input: "Read Cargo.toml and summarize dependencies".into(), ..App::default() };
                app.banner("test/model");
                app.submit();
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let mut tools = crate::tools::ToolRegistry::new();
                let work = async {
                    // Let keyboard input start waiting before tool/model events
                    // arrive, as during a real network round-trip.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    sender.send(AgentEvent { source: "main", kind: EventKind::ToolStarted {
                        name: "read_file".into(), input: json!({"path":"Cargo.toml"}),
                    }})?;
                    let (output, is_error) = tools.execute("read_file", &json!({"path":"Cargo.toml"})).await;
                    anyhow::ensure!(!is_error, "Cargo.toml read failed");
                    sender.send(AgentEvent { source: "main", kind: EventKind::ToolFinished {
                        name: "read_file".into(), output, is_error,
                    }})?;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    sender.send(AgentEvent { source: "main", kind: EventKind::AssistantText("Dependency summary rendered successfully".into()) })?;
                    Ok("Dependency summary rendered successfully".into())
                };
                match run_turn(&mut terminal, &mut app, &mut receiver, work).await? {
                    TurnOutcome::Finished(result) => app.finish_turn(result),
                    TurnOutcome::Quit => anyhow::bail!("quit before response"),
                }
                terminal.draw(&mut app)?;
                loop {
                    let event = next_terminal_event().await.context("terminal input failed")?;
                    match app.handle_event(event) {
                        InputAction::Quit => break,
                        InputAction::None | InputAction::Submit => {}
                    }
                    terminal.draw(&mut app)?;
                }
                Ok(())
            }.await;
            result.and(terminal.finish())
        }).unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a PTY; run via tests/blocked_tool_exit.py"]
    fn blocked_tool_terminal_fixture() {
        crate::run_with_runtime(async {
            let path = std::env::var("ROBBIE_TEST_FIFO")?;
            let mut terminal = InlineTerminal::new()?;
            let result = async {
                let mut app = App { started: Some(Instant::now()), status: "Blocked read test".into(), ..App::default() };
                app.push((0..60).map(|n| format!("history {n:02}")).collect::<Vec<_>>().join("\n"), Style::new());
                let (_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let mut tools = crate::tools::ToolRegistry::new();
                let work = async {
                    let (output, error) = tools.execute("read_file", &json!({"path":path})).await;
                    if error { anyhow::bail!("{output}"); }
                    Ok(output)
                };
                let outcome = run_turn(&mut terminal, &mut app, &mut receiver, work).await?;
                anyhow::ensure!(matches!(outcome, TurnOutcome::Quit), "expected Ctrl+C exit during blocked read");
                Ok(())
            }.await;
            result.and(terminal.finish())
        }).unwrap();
    }
}
