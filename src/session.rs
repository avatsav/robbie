//! Terminal-independent session ownership and host observations.
use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use tokio::sync::mpsc;

use crate::{
    agent::Agent,
    diagnostics::{Recorder, agent::AgentTurn},
    model::{self, AnthropicMessages, ModelClient, OpenRouter},
    skills::SkillRegistry,
    tools::ToolRegistry,
};

pub use crate::{
    diagnostics::{
        CapturePolicy, RecordingSettings,
        frontend::{TextAppend, UiEnd as FrontendEnd, UiTurn as FrontendTurn},
    },
    events::{AgentEvent, EventKind},
    model::PromptCache,
};

pub type ActivityReceiver = mpsc::UnboundedReceiver<AgentEvent>;

pub struct ModelSettings {
    pub api_key: String,
    pub model: String,
    pub upstream: Option<String>,
    pub prompt_cache: PromptCache,
}

pub struct SessionConfig {
    /// Absolute tool workspace; never inferred from the process directory.
    pub workspace: PathBuf,
    /// Absolute path, or relative to the workspace.
    pub skills_directory: PathBuf,
    pub model: ModelSettings,
    pub recording: RecordingSettings,
}

/// Cloneable recording status, without access to the journal or payload policy.
#[derive(Clone)]
pub struct Recording {
    recorder: Recorder,
}

impl Recording {
    pub fn description(&self) -> Option<String> {
        self.recorder.description()
    }

    pub fn take_warning(&self) -> Option<&'static str> {
        self.recorder.take_warning()
    }
}

pub struct Session {
    agent: Agent,
    recording: Recording,
}

impl Session {
    /// Uses the caller's Tokio runtime; creates no terminal or detached turn task.
    pub async fn new(config: SessionConfig) -> Result<(Self, ActivityReceiver)> {
        ensure!(
            config.workspace.is_absolute(),
            "workspace must be an absolute path"
        );
        let skills_directory = config.workspace.join(config.skills_directory);
        let skills = Arc::new(
            tokio::task::spawn_blocking(move || SkillRegistry::scan(&skills_directory))
                .await
                .context("skill scanning task failed")??,
        );
        let recorder = Recorder::start(&config.model.model, &config.workspace, config.recording);
        let model = Arc::new(
            ModelClient::new(
                model::client()?,
                OpenRouter::new(config.model.api_key, config.model.upstream),
                AnthropicMessages,
                config.model.model,
                recorder.clone(),
            )
            .with_prompt_cache(config.model.prompt_cache),
        );
        Ok(Self::assemble(model, skills, config.workspace, recorder))
    }

    fn assemble(
        model: Arc<ModelClient>,
        skills: Arc<SkillRegistry>,
        workspace: PathBuf,
        recorder: Recorder,
    ) -> (Self, ActivityReceiver) {
        let (events, receiver) = mpsc::unbounded_channel();
        let mut agent = Agent::new(
            model,
            ToolRegistry::with_skills(skills, workspace),
            events,
            "main",
        );
        agent.enable_sub_agent();
        recorder.session_context(|| agent.transcript_context());
        (
            Self {
                agent,
                recording: Recording { recorder },
            },
            receiver,
        )
    }

    pub fn model_name(&self) -> &str {
        self.agent.model_name()
    }

    pub fn recording(&self) -> Recording {
        self.recording.clone()
    }

    /// Allocates correlation now, before the execution future's first poll.
    pub fn prepare_turn(&mut self) -> PreparedTurn<'_> {
        let turn = self.agent.prepare_turn(None);
        PreparedTurn {
            session: self,
            turn,
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn for_test(
        model: Arc<ModelClient>,
        workspace: PathBuf,
        recorder: Recorder,
    ) -> (Self, ActivityReceiver) {
        Self::assemble(
            model,
            Arc::new(SkillRegistry::default()),
            workspace,
            recorder,
        )
    }
}

/// Holds exclusive access to its originating conversation until run or dropped.
#[must_use = "dropping a prepared turn cancels it without running the agent"]
pub struct PreparedTurn<'a> {
    session: &'a mut Session,
    turn: AgentTurn,
}

impl PreparedTurn<'_> {
    /// Record the host's submitted source, which may differ from its execution prompt.
    pub fn record_frontend(&self, submitted: &str) -> FrontendTurn {
        self.session
            .recording
            .recorder
            .ui_turn(self.turn.origin(), submitted)
    }

    /// Emits buffered activities; a closed receiver is nonfatal. Dropping this
    /// future cancels waiting, not external side effects or running blocking work.
    pub async fn run(self, prompt: &str) -> Result<String> {
        self.session.agent.run_in_turn(prompt, self.turn).await
    }
}

#[cfg(all(test, unix))]
mod tests;
