use super::*;

#[cfg(unix)]
mod diagnostics;
use crate::session::EventKind;
use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

#[cfg(unix)]
fn session_config(root: &std::path::Path, transcript: bool) -> crate::session::SessionConfig {
    use crate::session::{
        CapturePolicy, ModelSettings, PromptCache, RecordingSettings, SessionConfig,
    };
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    SessionConfig {
        skills_directory: workspace.join("skills"),
        workspace,
        model: ModelSettings {
            api_key: "test-secret".into(),
            model: "test".into(),
            upstream: Some("test-provider".into()),
            prompt_cache: PromptCache::Short,
        },
        recording: RecordingSettings {
            state_directory: Some(root.to_owned()),
            capture: if transcript {
                CapturePolicy::Transcript
            } else {
                CapturePolicy::MetadataOnly
            },
        },
    }
}

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
