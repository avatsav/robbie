//! Explicit workspace exports, independent from the session journal.
use super::*;
use robbie_core::Session;
use serde_json::{Value, json};
use std::{fs, path::Path};

fn config(root: &Path, recording: bool) -> SessionConfig {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    SessionConfig {
        skills_directory: workspace.join("skills"),
        workspace,
        model: ModelSettings {
            api_key: "PRIVATE_EXPORT_CONNECTION_SECRET".into(),
            model: "fixture/model".into(),
            upstream: None,
            prompt_cache: PromptCache::Off,
        },
        recording: RecordingSettings {
            state_directory: recording.then(|| root.join("state")),
            capture: CapturePolicy::MetadataOnly,
        },
    }
}

async fn session(server: &Server, root: &Path, recording: bool) -> Session {
    server.session(config(root, recording)).await.unwrap().0
}

fn snapshot(session: &Session) -> Value {
    serde_json::from_slice(&fs::read(session.dump_transcript().unwrap()).unwrap()).unwrap()
}

#[path = "transcript/fidelity.rs"]
mod fidelity;
#[path = "transcript/storage.rs"]
mod storage;
