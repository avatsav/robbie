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

mod input;

mod presentation;

mod tool_modes;

mod tool_activity;

mod tool_output;

mod usage;
