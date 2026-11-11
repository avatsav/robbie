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
    agent::compaction::{Options as CompactionOptions, Outcome as CompactionOutcome},
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
        Self::connect(config, |provider| provider).await
    }

    // The fixture path changes only the connection address; construction and
    // agent assembly remain identical to normal session startup.
    pub(crate) async fn connect(
        config: SessionConfig,
        configure_provider: impl FnOnce(OpenRouter) -> OpenRouter,
    ) -> Result<(Self, ActivityReceiver)> {
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
                configure_provider(OpenRouter::new(config.model.api_key, config.model.upstream)),
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

    /// Resolve once per session, shared by parent and children. A configured
    /// override avoids network access. Discovery failures return unknown limits
    /// with a fixed warning, never a fabricated capacity or response payload.
    /// Invalid overrides or conflicting repeat overrides return an error.
    pub async fn resolve_model_limits(
        &mut self,
        configured: Option<crate::model::ModelLimits>,
    ) -> Result<crate::model::LimitResolution> {
        self.agent.resolve_limits(configured).await
    }

    /// Opt in once, after resolving limits. Children share this immutable policy,
    /// never the parent's history or usage baseline. Unknown capacity leaves
    /// automatic checks disabled; inspect the resolution's warning.
    /// Invalid budgets or conflicting repeat configuration return an error.
    pub fn configure_auto_compaction(&mut self, policy: crate::CompactionPolicy) -> Result<()> {
        self.agent.configure_auto_compaction(policy)
    }

    pub fn model_limit_resolution(&self) -> Option<crate::model::LimitResolution> {
        self.agent.limit_resolution()
    }

    pub fn recording(&self) -> Recording {
        self.recording.clone()
    }

    /// Session-wide reported totals and the root conversation's latest baseline.
    /// While a prepared/running turn borrows this session, consume UsageUpdated
    /// activities instead. Neither path depends on diagnostics recording.
    pub fn statistics(&self) -> crate::UsageSnapshot {
        self.agent.statistics()
    }

    /// Explicitly compact this conversation while no turn borrows the session.
    /// Limits are caller-supplied. Recent-size/shrink estimates use serialized
    /// message characters / 4; they do not guarantee model-window fit. A commit
    /// affects subsequent requests, not frontend transcript state. No tools run.
    /// Dropping the future while waiting leaves context unchanged; the provider
    /// may still have processed or billed the request. No detached task is spawned.
    ///
    /// # Errors
    /// Invalid limits/exchanges, preparation overflow, request/response failure,
    /// or an unusable/nonshrinking candidate leave the previous context intact.
    ///
    /// A prepared turn prevents concurrent compaction:
    /// ```compile_fail
    /// use robbie_core::{CompactionOptions, Session};
    /// fn conflicting_operations(session: &mut Session, options: CompactionOptions) {
    ///     let turn = session.prepare_turn();
    ///     let compaction = session.compact(options);
    ///     drop(turn);
    ///     drop(compaction);
    /// }
    /// ```
    pub async fn compact(&mut self, options: CompactionOptions) -> Result<CompactionOutcome> {
        self.agent.compact(options).await
    }

    #[cfg(all(unix, any(test, feature = "test-support")))]
    pub(crate) fn recording_control(&self) -> RecordingControl {
        RecordingControl {
            recorder: self.recording.recorder.clone(),
        }
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
    fn for_test(
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

/// Fixture-only fault control; never exposes the journal or changes capture policy.
#[cfg(all(unix, any(test, feature = "test-support")))]
#[derive(Clone)]
pub struct RecordingControl {
    recorder: Recorder,
}

#[cfg(all(unix, any(test, feature = "test-support")))]
impl RecordingControl {
    /// Make subsequent appends fail. Requires a successfully opened recording;
    /// the next real recording operation emits the normal nonfatal warning.
    pub fn fail_writes(&self) {
        self.recorder.break_writer_for_test();
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
