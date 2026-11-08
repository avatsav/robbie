use super::*;

#[cfg(unix)]
mod diagnostics;
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

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

#[cfg(unix)]
mod terminal;

mod selection;

mod transcript;

mod lifecycle;

mod rendering;
