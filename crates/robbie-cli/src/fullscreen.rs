use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result};
use crossterm::{
    SynchronizedUpdate,
    clipboard::CopyToClipboard,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
};
use futures_util::StreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    session::{
        AgentEvent, EventKind, FrontendEnd as UiEnd, FrontendTurn as UiTurn, Recording, Session,
    },
    ui::{self, InputAction, OutputLine},
};

mod selection;
use selection::Selection;

// UI state owns formatting/input; this screen owns the transcript and viewport.
#[derive(Default)]
struct App {
    content: ui::App,
    transcript: Vec<OutputLine>,
    // Zero follows the latest output; otherwise count rows back from the bottom.
    scroll_back: usize,
    selection: Selection,
    history_area: Rect,
    visible_start: usize,
    rendered_rows: Vec<Line<'static>>,
    rendered_width: Option<usize>,
    pending_copy: Option<String>,
    recorder: Option<Recording>,
    ui_turn: Option<UiTurn>,
}

impl App {
    fn new(model: &str) -> Self {
        let mut app = Self::default();
        app.content.banner(model);
        app
    }

    fn activity(&mut self, event: AgentEvent) {
        if let EventKind::AssistantText(text, Some(context)) = &event.kind
            && let Some(turn) = &mut self.ui_turn
        {
            turn.received(*context, text);
        }
        let receipt = self.content.activity(event);
        if let Some(append) = receipt.assistant
            && let Some(turn) = &mut self.ui_turn
        {
            turn.appended(append);
        }
    }

    fn end_ui_turn(&mut self, end: UiEnd) {
        if let Some(turn) = self.ui_turn.take() {
            turn.finish(end);
        }
    }

    fn finish_turn(&mut self, result: Result<String>) {
        let end = match &result {
            Ok(text) => UiEnd::Completed {
                returned_text_bytes: text.len(),
            },
            Err(_) => UiEnd::AgentFailed,
        };
        self.content.finish_turn(result);
        self.end_ui_turn(end);
        self.recording_warning();
    }

    fn recording_warning(&mut self) {
        if let Some(warning) = self.recorder.as_ref().and_then(Recording::take_warning) {
            self.content.notice(format!("[warning] {warning}"));
        }
    }

    fn update_rows(&mut self, width: usize) {
        let appended_at = self.transcript.len();
        self.transcript.extend(self.content.take_output());
        if self.rendered_width != Some(width) {
            self.rendered_rows = self
                .transcript
                .iter()
                .flat_map(|line| line.rows(width))
                .collect();
            self.rendered_width = Some(width);
        } else {
            self.rendered_rows.extend(
                self.transcript[appended_at..]
                    .iter()
                    .flat_map(|line| line.rows(width)),
            );
        }
    }

    #[cfg(test)]
    fn rows(&mut self, width: usize) -> Vec<Line<'static>> {
        self.update_rows(width);
        self.rendered_rows.clone()
    }

    fn view(&mut self, frame: &mut Frame) {
        let [history, live] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(
                self.content
                    .live_height(frame.area().width, frame.area().height),
            ),
        ])
        .areas(frame.area());
        let previous_row_count = self.rendered_rows.len();
        self.update_rows(history.width as usize);
        let rows = &self.rendered_rows;
        if history != self.history_area {
            // Mouse coordinates may refer to different text after a resize.
            self.selection.clear();
        } else if self.selection.is_active() {
            // New model/tool output must not move the text under an active drag.
            self.scroll_back = self
                .scroll_back
                .saturating_add(rows.len().saturating_sub(previous_row_count));
        }
        let max_scroll = rows.len().saturating_sub(history.height as usize);
        self.scroll_back = self.scroll_back.min(max_scroll);
        let start = max_scroll.saturating_sub(self.scroll_back);
        let visible = rows
            .iter()
            .skip(start)
            .take(history.height as usize)
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), history);
        self.selection
            .highlight(frame.buffer_mut(), history, start, rows);
        self.history_area = history;
        self.visible_start = start;
        self.content.view_live(frame, live);
    }

    fn handle_terminal_event(&mut self, event: Event) -> Result<InputAction> {
        let action = self.handle_event(event);
        self.write_pending_copy(&mut std::io::stdout())?;
        Ok(action)
    }

    fn write_pending_copy(&mut self, writer: &mut impl Write) -> Result<()> {
        if let Some(text) = self.pending_copy.take() {
            // OSC 52 asks the terminal to write its host clipboard. No clipboard
            // reads, platform subprocesses, or model output are involved.
            execute!(writer, CopyToClipboard::to_clipboard_from(text))
                .context("could not send selected text to the terminal clipboard")?;
        }
        Ok(())
    }

    fn mouse_position(&self, mouse: &MouseEvent, clamp: bool) -> Option<(usize, usize)> {
        let area = self.history_area;
        let visible = self
            .rendered_rows
            .len()
            .saturating_sub(self.visible_start)
            .min(area.height as usize);
        if area.width == 0 || visible == 0 {
            return None;
        }
        if !clamp
            && (!area.contains((mouse.column, mouse.row).into())
                || mouse.row.saturating_sub(area.y) as usize >= visible)
        {
            return None;
        }
        Some((
            self.visible_start + (mouse.row.saturating_sub(area.y) as usize).min(visible - 1),
            mouse.column.saturating_sub(area.x).min(area.width - 1) as usize,
        ))
    }

    fn handle_event(&mut self, event: Event) -> InputAction {
        match &event {
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        self.scroll_back = self.scroll_back.saturating_add(3)
                    }
                    MouseEventKind::ScrollDown => {
                        self.scroll_back = self.scroll_back.saturating_sub(3)
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.selection.clear();
                        if let Some((row, column)) = self.mouse_position(mouse, false) {
                            self.selection.begin(row, column);
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some((row, column)) = self.mouse_position(mouse, true) {
                            self.selection.extend(row, column);
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if let Some((row, column)) = self.mouse_position(mouse, true) {
                            self.selection.extend(row, column);
                        }
                        self.pending_copy = self.selection.finish(&self.rendered_rows);
                    }
                    _ => {}
                }
                return InputAction::None;
            }
            Event::Resize(_, _) | Event::FocusLost => self.selection.clear(),
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.selection.clear();
                match key.code {
                    KeyCode::PageUp => {
                        self.scroll_back = self.scroll_back.saturating_add(10);
                        return InputAction::None;
                    }
                    KeyCode::PageDown => {
                        self.scroll_back = self.scroll_back.saturating_sub(10);
                        return InputAction::None;
                    }
                    KeyCode::End => {
                        self.scroll_back = 0;
                        return InputAction::None;
                    }
                    _ => {}
                }
            }
            Event::Paste(_) => self.selection.clear(),
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
        // The application handles both wheel events and drag selection.
        execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)
            .context("could not enable terminal input modes")?;
        Ok(modes)
    }
}

impl Drop for ScreenModes {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
    }
}

// Ratatui restores the termios it captured, but macOS can leave the transient
// PENDIN flag set when canonical mode returns. Restore the exact pre-init state
// with TCSAFLUSH so the shell does not inherit a pending-input marker.
#[cfg(unix)]
struct TerminalState {
    original: libc::termios,
}

#[cfg(unix)]
impl TerminalState {
    fn capture() -> Option<Self> {
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr initializes the struct on success.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) } == 0 {
            Some(Self {
                original: unsafe { original.assume_init() },
            })
        } else {
            None
        }
    }
}

#[cfg(unix)]
impl Drop for TerminalState {
    fn drop(&mut self) {
        // SAFETY: original was initialized by tcgetattr.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.original);
        }
    }
}

#[cfg(not(unix))]
struct TerminalState;

#[cfg(not(unix))]
impl TerminalState {
    fn capture() -> Option<Self> {
        None
    }
}

pub async fn run(
    mut session: Session,
    mut receiver: UnboundedReceiver<AgentEvent>,
    workspace: PathBuf,
) -> Result<()> {
    let recorder = session.recording();
    let terminal_state = TerminalState::capture();
    let mut terminal = ratatui::init();
    let result = async {
        let _modes = ScreenModes::new()?;
        run_app(
            &mut terminal,
            &mut session,
            &mut receiver,
            &recorder,
            workspace,
        )
        .await
    }
    .await;
    // Restore the alternate screen on both normal return and error paths.
    ratatui::restore();
    drop(terminal_state);
    // A failure during cancellation/cleanup may occur after the last UI draw.
    if let Some(warning) = recorder.take_warning() {
        eprintln!("[warning] {warning}");
    }
    result
}

async fn run_app(
    terminal: &mut DefaultTerminal,
    session: &mut Session,
    receiver: &mut UnboundedReceiver<AgentEvent>,
    recorder: &Recording,
    directory: PathBuf,
) -> Result<()> {
    let mut app = App::new(session.model_name());
    app.content.activity(AgentEvent {
        source: "main",
        kind: EventKind::UsageUpdated(session.statistics()),
    });
    app.recorder = Some(recorder.clone());
    if let Some(warning) = recorder.take_warning() {
        app.content.notice(format!("[warning] {warning}"));
    } else if let Some(description) = recorder.description() {
        app.content.notice(description);
    }
    // Capability probing owns stdin briefly, before EventStream starts reading.
    app.content.configure_mascot_graphics();
    let mut workspace = ui::WorkspaceMonitor::start(directory);
    workspace.apply_current(&mut app.content);
    let mut keys = EventStream::new();
    loop {
        match wait_for_input(terminal, &mut app, &mut keys, &mut workspace).await? {
            InputAction::Quit => return Ok(()),
            InputAction::None => continue,
            InputAction::Submit => {}
        }
        let prompt = app.content.submit();
        app.scroll_back = 0;
        let turn = session.prepare_turn();
        app.ui_turn = Some(turn.record_frontend(&prompt));
        match run_turn(
            terminal,
            &mut app,
            &mut keys,
            receiver,
            &mut workspace,
            turn.run(prompt.trim()),
        )
        .await
        {
            Ok(TurnOutcome::Finished(result)) => app.finish_turn(result),
            Ok(TurnOutcome::Quit) => {
                app.end_ui_turn(UiEnd::Quit);
                return Ok(());
            }
            Err(error) => {
                app.end_ui_turn(UiEnd::FrontendFailed);
                return Err(error);
            }
        }
    }
}

// Present text, first-use image uploads, and image placements together rather
// than letting the terminal paint an intermediate, partially updated screen.
fn draw(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    std::io::stdout().sync_update(|_| terminal.draw(|frame| app.view(frame)).map(|_| ()))??;
    Ok(())
}

// Shared by the application and its PTY fixture: idle/completion animations
// need redraws too, without blocking input, scrolling, or workspace updates.
async fn wait_for_input(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    keys: &mut EventStream,
    workspace: &mut ui::WorkspaceMonitor,
) -> Result<InputAction> {
    let mut ticks = tokio::time::interval(ui::ANIMATION_TICK);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        app.recording_warning();
        draw(terminal, app)?;
        tokio::select! {
            event = keys.next() => {
                let event = event.context("terminal input closed")??;
                match app.handle_terminal_event(event)? {
                    InputAction::None => {}
                    action => return Ok(action),
                }
            }
            _ = ticks.tick() => app.content.advance_tick(),
            Some(snapshot) = workspace.changed() => app.content.set_workspace(snapshot),
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
    workspace: &mut ui::WorkspaceMonitor,
    run: impl std::future::Future<Output = Result<String>>,
) -> Result<TurnOutcome> {
    tokio::pin!(run);
    let mut ticks = tokio::time::interval(ui::ANIMATION_TICK);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        app.recording_warning();
        draw(terminal, app)?;
        tokio::select! {
            result = &mut run => {
                while let Ok(event) = receiver.try_recv() { app.activity(event); }
                app.recording_warning();
                return Ok(TurnOutcome::Finished(result));
            }
            Some(event) = receiver.recv() => app.activity(event),
            Some(snapshot) = workspace.changed() => app.content.set_workspace(snapshot),
            _ = ticks.tick() => app.content.advance_tick(),
            event = keys.next() => {
                let event = event.context("terminal input closed")??;
                // Enter is ignored while busy, but drafting, modes and scrolling work.
                match app.handle_terminal_event(event)? {
                    InputAction::Quit => return Ok(TurnOutcome::Quit),
                    InputAction::None | InputAction::Submit => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
