//! Draft-only command suggestions. Rendering never changes selection or input.
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType},
};
use unicode_width::UnicodeWidthStr;

use super::{
    MUTED_CYAN, MUTED_TEXT,
    commands::{COMMANDS, Definition},
    single_line,
};

const BACKGROUND: Color = Color::Rgb(25, 28, 36);
const SELECTED: Color = Color::Rgb(45, 48, 65);

#[derive(Default)]
pub(super) struct Completion {
    selected: usize,
    dismissed: bool,
}

impl Completion {
    pub(super) fn edited(&mut self) {
        *self = Self::default();
    }

    fn candidates(&self, input: &str) -> Vec<&'static Definition> {
        let prefix = input.trim_start();
        if self.dismissed || !prefix.starts_with('/') || prefix.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .filter(|definition| definition.name.starts_with(prefix))
            .collect()
    }

    // true consumes the key locally. A complete command's Enter is deliberately
    // left to ordinary submission; accepting a partial suggestion never submits.
    pub(super) fn handle_key(&mut self, input: &mut String, key: KeyCode) -> bool {
        let candidates = self.candidates(input);
        if candidates.is_empty() {
            return false;
        }
        let selected = self.selected.min(candidates.len() - 1);
        match key {
            KeyCode::Up => self.selected = (selected + candidates.len() - 1) % candidates.len(),
            KeyCode::Down => self.selected = (selected + 1) % candidates.len(),
            KeyCode::Esc => self.dismissed = true,
            KeyCode::Enter if input.trim_start() == candidates[selected].name => return false,
            KeyCode::Tab | KeyCode::Enter => {
                let leading = input.len() - input.trim_start().len();
                input.truncate(leading);
                input.push_str(candidates[selected].name);
                self.dismissed = true;
            }
            _ => return false,
        }
        true
    }

    pub(super) fn height(&self, input: &str, width: u16, available: u16) -> u16 {
        if width < 12 || available < 3 {
            return 0;
        }
        let count = self.candidates(input).len();
        if count == 0 {
            0
        } else {
            (count as u16 + 2).min(available)
        }
    }

    pub(super) fn view(&self, frame: &mut Frame, area: Rect, input: &str, busy: bool) {
        if area.width < 12 || area.height < 3 {
            return;
        }
        let candidates = self.candidates(input);
        if candidates.is_empty() {
            return;
        }
        let area = Rect {
            width: area.width.min(76),
            ..area
        };
        let selected = self.selected.min(candidates.len() - 1);
        let complete = input.trim_start() == candidates[selected].name;
        let hint = match (area.width >= 54, busy, complete) {
            (true, true, _) => " ↑↓ choose · Tab fill · Esc close (busy) ",
            (true, false, true) => " ↑↓ choose · Tab fill · Enter run · Esc close ",
            (true, false, false) => " ↑↓ choose · Tab/Enter fill · Esc close ",
            (false, true, _) => " Tab fill · busy ",
            (false, false, true) => " Enter run · Esc ",
            (false, false, false) => " Tab fill · Esc ",
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(MUTED_CYAN))
            .style(Style::new().bg(BACKGROUND))
            .title(Line::from(" Commands ").fg(MUTED_CYAN).bold())
            .title_bottom(Line::from(hint).fg(MUTED_TEXT));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        // A short viewport must still show the selected candidate.
        let first = selected.saturating_sub(inner.height.saturating_sub(1) as usize);
        for (row, definition) in candidates
            .iter()
            .skip(first)
            .take(inner.height as usize)
            .enumerate()
        {
            let active = first + row == selected;
            let style = Style::new().bg(if active { SELECTED } else { BACKGROUND });
            let name = single_line(
                &format!("{}{}", if active { "› " } else { "  " }, definition.name),
                inner.width as usize,
            );
            let remaining = (inner.width as usize).saturating_sub(name.width());
            let description = if remaining > 3 {
                format!("  {}", single_line(definition.description, remaining - 2))
            } else {
                String::new()
            };
            let row_area = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
            frame.buffer_mut().set_style(row_area, style);
            frame.render_widget(
                Line::from(vec![
                    Span::styled(name, Style::new().fg(MUTED_CYAN).bold()),
                    Span::styled(description, Style::new().fg(MUTED_TEXT)),
                ])
                .style(style),
                row_area,
            );
        }
    }
}
