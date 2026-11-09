use std::collections::HashMap;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
#[cfg(test)]
use ratatui::Frame;
use ratatui::{
    style::{Color, Style, Stylize},
    text::Line,
};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    markdown::Markdown,
    session::{AgentEvent, EventKind, TextAppend},
};

mod footer;
mod mascot;
mod tool_display;
mod workspace;

pub(super) use mascot::TICK as ANIMATION_TICK;
pub(super) use workspace::WorkspaceMonitor;

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

pub(super) struct ActivityReceipt {
    pub assistant: Option<TextAppend>,
}

pub(super) struct ToolOutcome {
    failed: bool,
    footer: Option<String>,
}

// Conversation text wraps; tool headers and preview rows stay compact and muted.
pub(super) enum OutputLine {
    Banner {
        model: String,
    },
    UserPrompt {
        text: String,
    },
    Wrapped(StyledLine),
    Markdown(Markdown),
    Tool {
        summary: String,
        outcome: ToolOutcome,
    },
    ToolOutput {
        prefix: &'static str,
        text: String,
    },
}

impl OutputLine {
    pub(super) fn rows(&self, width: usize) -> Vec<Line<'static>> {
        match self {
            Self::Banner { model } => banner_rows(model, width),
            Self::UserPrompt { text } => user_prompt_rows(text, width),
            Self::Markdown(markdown) => markdown.rows(width),
            Self::Wrapped((text, style)) => crate::terminal_text::wrap(text, width.max(1))
                .into_iter()
                .map(|line| Line::styled(line.to_string(), *style))
                .collect(),
            Self::Tool { summary, outcome } => vec![tool_line(summary, Some(outcome), width)],
            Self::ToolOutput { prefix, text } => {
                let prefix = single_line(&format!("{prefix} "), width);
                let content = single_line(text, width.saturating_sub(prefix.width()));
                vec![Line::from(vec![
                    prefix.fg(MUTED_CYAN),
                    content.fg(MUTED_TEXT),
                ])]
            }
        }
    }

    #[cfg(test)]
    fn text(&self) -> &str {
        match self {
            Self::Banner { model } => model,
            Self::UserPrompt { text } => text,
            Self::Wrapped((text, _)) => text,
            Self::Markdown(markdown) => markdown.source(),
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
    working: bool,
    model: String,
    workspace: workspace::Workspace,
    tick: usize,
    mascot: mascot::Mascot,
    // Colors the last turn's error; it must not disable the input.
    failed: bool,
    // A child can run tools while the parent's sub_agent call is still active.
    active_tools: HashMap<&'static str, ToolActivity>,
    output_mode: ToolOutputMode,
    // Survives draining pending rows into the retained transcript.
    needs_text_separator: bool,
}

impl App {
    // The screen host drains these formatted events into its retained transcript.
    pub(super) fn take_output(&mut self) -> Vec<OutputLine> {
        std::mem::take(&mut self.pending)
    }

    pub(super) fn advance_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.mascot.advance();
    }

    pub(super) fn banner(&mut self, model: &str) {
        self.model = clean(model).replace('\n', " ");
        self.pending.push(OutputLine::Banner {
            model: model.to_string(),
        });
        self.blank();
    }

    pub(super) fn notice(&mut self, text: impl Into<String>) {
        self.push(text, Style::new().fg(MUTED_TEXT));
        self.blank();
    }

    fn push(&mut self, text: impl Into<String>, style: Style) {
        let text = clean(&text.into());
        for line in text.split('\n') {
            self.pending
                .push(OutputLine::Wrapped((line.to_string(), style)));
        }
    }

    fn blank(&mut self) {
        self.pending
            .push(OutputLine::Wrapped((String::new(), Style::new())));
        self.needs_text_separator = false;
    }

    pub(super) fn submit(&mut self) -> String {
        let prompt = std::mem::take(&mut self.input);
        self.pending.push(OutputLine::UserPrompt {
            text: clean(&prompt),
        });
        self.blank();
        self.working = true;
        self.failed = false;
        self.mascot.set_activity(mascot::Activity::Thinking);
        prompt
    }

    pub(super) fn finish_turn(&mut self, result: Result<String>) {
        self.working = false;
        self.failed = result.is_err();
        self.active_tools.clear();
        self.mascot.set_activity(if result.is_ok() {
            mascot::Activity::Success
        } else {
            mascot::Activity::Error
        });
        if let Err(error) = result {
            self.push(format!("[error] {error:#}"), Style::new().red());
            self.blank();
        }
    }

    pub(super) fn activity(&mut self, event: AgentEvent) -> ActivityReceipt {
        let mut receipt = ActivityReceipt { assistant: None };
        if !matches!(&event.kind, EventKind::UsageUpdated(_)) {
            self.working = true;
        }
        let source = event.source;
        match event.kind {
            // Statistics rendering is the next slice. Receiving them must not
            // change working/mascot state or create transcript output.
            EventKind::UsageUpdated(_) => return receipt,
            EventKind::ModelStarted => {}
            EventKind::AssistantText(text, context) => {
                if self.needs_text_separator {
                    self.blank();
                }
                if source != "main" {
                    // Keep the source label outside Markdown so a child's first
                    // heading, list, or code fence is still parsed as a block.
                    self.push("[agent]", Style::new());
                }
                let sanitized = clean(&text);
                receipt.assistant = context.map(|context| TextAppend {
                    context,
                    sanitized_bytes: sanitized.len(),
                    non_whitespace_chars: sanitized
                        .chars()
                        .filter(|ch| !ch.is_whitespace())
                        .count(),
                });
                self.pending
                    .push(OutputLine::Markdown(Markdown::new(sanitized)));
                self.blank();
            }
            EventKind::ToolStarted { name, input } => {
                let display = tool_display::presentation(&name);
                let label = display.label;
                let prefix = if source == "main" {
                    label.to_string()
                } else {
                    format!("agent/{label}")
                };
                let arguments = clean(&display.summary(&input))
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let summary = format!("[{prefix}] {arguments}");
                self.active_tools
                    .insert(source, ToolActivity { summary, input });
            }
            EventKind::ToolFinished {
                name,
                output,
                is_error,
            } => {
                let activity = self.active_tools.remove(source);
                let summary = activity
                    .as_ref()
                    .map(|tool| tool.summary.clone())
                    .unwrap_or_else(|| format!("[{}]", tool_display::presentation(&name).label));
                let footer = if name == "bash" && self.output_mode == ToolOutputMode::Concise {
                    bash_footer(&output).or_else(|| is_error.then(|| "[no exit code]".into()))
                } else {
                    None
                };
                self.pending.push(OutputLine::Tool {
                    summary,
                    outcome: ToolOutcome {
                        failed: is_error,
                        footer,
                    },
                });
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
                                text: clean(&output)
                                    .lines()
                                    .next()
                                    .unwrap_or("tool failed")
                                    .to_string(),
                            });
                        }
                    }
                    ToolOutputMode::Full => {
                        self.push(output, Style::new().fg(MUTED_TEXT));
                        if !is_error && let Some(activity) = activity {
                            let input = activity.input;
                            let mut changes = Vec::new();
                            if name == "edit_file" {
                                diff_lines(
                                    &mut changes,
                                    input["old_text"].as_str().unwrap_or(""),
                                    "- ",
                                    Style::new().fg(MUTED_RED),
                                );
                                diff_lines(
                                    &mut changes,
                                    input["new_text"].as_str().unwrap_or(""),
                                    "+ ",
                                    Style::new().fg(MUTED_GREEN),
                                );
                            } else if name == "write_file" {
                                diff_lines(
                                    &mut changes,
                                    input["content"].as_str().unwrap_or(""),
                                    "+ ",
                                    Style::new().fg(MUTED_GREEN),
                                );
                            }
                            self.pending
                                .extend(changes.into_iter().map(OutputLine::Wrapped));
                        }
                        self.blank();
                    }
                }
            }
        }
        self.sync_mascot_activity();
        receipt
    }

    fn sync_mascot_activity(&mut self) {
        self.mascot.set_activity(if self.active_tools.is_empty() {
            mascot::Activity::Thinking
        } else {
            mascot::Activity::Working
        });
    }

    pub(super) fn handle_event(&mut self, event: Event) -> InputAction {
        // Paste is inserted as text, never interpreted as submit/quit keys.
        if let Event::Paste(text) = event {
            self.mascot.interact();
            self.input.push_str(&clean(&text).replace('\n', " "));
            return InputAction::None;
        }
        let Event::Key(key) = event else {
            return InputAction::None;
        };
        if key.kind == KeyEventKind::Release {
            return InputAction::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => InputAction::Quit,
                KeyCode::Char('d') if self.input.is_empty() => InputAction::Quit,
                KeyCode::Char('o') => {
                    self.output_mode = self.output_mode.next();
                    InputAction::None
                }
                KeyCode::Char('g') => {
                    self.mascot.toggle_visibility();
                    InputAction::None
                }
                _ => InputAction::None,
            };
        }
        // Draft interaction wakes/suppresses idle emotes even if it does not
        // change text (e.g. backspace on an empty draft). Display shortcuts and
        // key releases above deliberately leave the animation untouched.
        if !key.modifiers.contains(KeyModifiers::ALT)
            && (matches!(
                key.code,
                KeyCode::Enter
                    | KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Home
                    | KeyCode::End
            ) || matches!(key.code, KeyCode::Char(c) if !c.is_control()))
        {
            self.mascot.interact();
        }
        match key.code {
            KeyCode::Enter if !self.input.trim().is_empty() => InputAction::Submit,
            KeyCode::Backspace => {
                if let Some((start, _)) = self.input.grapheme_indices(true).next_back() {
                    self.input.truncate(start);
                }
                InputAction::None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) && !c.is_control() => {
                self.input.push(c);
                InputAction::None
            }
            _ => InputAction::None,
        }
    }

    #[cfg(test)]
    fn view(&self, frame: &mut Frame) {
        self.view_live(frame, frame.area());
    }
}

fn user_prompt_rows(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    // Wrap only the body, then add the gutter to every physical row. Reserve
    // room for wide glyphs even when the viewport is too narrow for a full gutter.
    let prefix: String = "▌ ".chars().take(width.saturating_sub(2)).collect();
    crate::terminal_text::wrap(text, width - prefix.width())
        .into_iter()
        .map(|part| Line::styled(format!("{prefix}{part}"), Style::new().fg(USER_ORANGE)))
        .collect()
}

fn banner_rows(model: &str, width: usize) -> Vec<Line<'static>> {
    if width < 3 {
        return vec![Line::from(single_line("▌ robbie", width)).cyan()];
    }
    let text = format!(
        "robbie {} › openrouter · {}",
        env!("CARGO_PKG_VERSION"),
        clean(model).replace('\n', " ")
    );
    let mut brand_remaining = "robbie".len();
    crate::terminal_text::wrap(&text, width - 2)
        .into_iter()
        .map(|part| {
            // Only the ASCII brand is bold, even if a very narrow terminal wraps it.
            let bold_len = brand_remaining.min(part.len());
            brand_remaining -= bold_len;
            Line::from(vec![
                "▌ ".cyan(),
                part[..bold_len].to_string().white().bold(),
                part[bold_len..].to_string().white(),
            ])
        })
        .collect()
}

fn tool_line(summary: &str, outcome: Option<&ToolOutcome>, width: usize) -> Line<'static> {
    let failed = outcome.is_some_and(|outcome| outcome.failed);
    let footer = outcome
        .and_then(|outcome| outcome.footer.as_deref())
        .map(|footer| format!(" {footer}"))
        .unwrap_or_default();
    // Preserve the actual outcome even when the command itself is very long.
    let footer = input_tail(&footer, width).to_string();
    let head_width = width.saturating_sub(footer.width());
    let marker = if failed && head_width >= 2 {
        "• "
    } else if failed && head_width == 1 {
        "•"
    } else {
        ""
    };
    let summary = single_line(summary, head_width.saturating_sub(marker.width()));
    let mut spans = vec![marker.fg(MUTED_RED)];
    if let Some((label, arguments)) = summary.split_once("] ") {
        spans.extend([
            format!("{label}] ").fg(MUTED_CYAN),
            arguments.to_string().fg(MUTED_TEXT),
        ]);
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
        preview.push(OutputLine::ToolOutput {
            prefix: "┌",
            text: "… earlier output omitted".into(),
        });
    }
    for (index, line) in lines.iter().enumerate().skip(skip) {
        let prefix = if index + 1 == lines.len() {
            "└"
        } else if index == 0 {
            "┌"
        } else {
            "│"
        };
        preview.push(OutputLine::ToolOutput {
            prefix,
            text: line.to_string(),
        });
    }
    preview
}

fn clean(text: &str) -> String {
    text.replace('\t', "    ")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}

fn single_line(text: &str, width: usize) -> String {
    let text = clean(text).replace('\n', " ");
    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    for (index, grapheme) in text.grapheme_indices(true) {
        if text[..index + grapheme.len()].width() > width - 1 {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

fn diff_lines(lines: &mut Vec<StyledLine>, text: &str, prefix: &str, style: Style) {
    for line in clean(text).lines() {
        lines.push((format!("  {prefix}{line}"), style));
    }
}

// Tail clipping also keeps shell exit markers visible in narrow tool rows.
fn input_tail(input: &str, width: usize) -> &str {
    let mut start = input.len();
    for (index, _) in input.grapheme_indices(true).rev() {
        if input[index..].width() > width {
            break;
        }
        start = index;
    }
    &input[start..]
}

#[cfg(test)]
mod tests;
