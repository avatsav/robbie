//! Narrow access to the real read tool for terminal responsiveness fixtures.
use std::{path::Path, sync::Arc};

use serde_json::json;

use crate::{skills::SkillRegistry, tools::ToolRegistry};

/// Run one real read, including its output limits and blocking/cancellation behavior.
/// No registry, mutable tool state, or arbitrary tool dispatch escapes the fixture.
pub async fn read_file(workspace: &Path, path: &str) -> (String, bool) {
    let mut tools =
        ToolRegistry::with_skills(Arc::new(SkillRegistry::default()), workspace.to_owned());
    tools
        .execute_in_context("read_file", &json!({"path": path}), None)
        .await
}
