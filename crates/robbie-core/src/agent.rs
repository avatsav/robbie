//! Agent conversation and execution loop, separate from startup and the frontend.
use std::sync::Arc;

use anyhow::Result;

use crate::{
    diagnostics::{
        TranscriptContext,
        agent::{AgentTurn, Decision, TurnEnd},
        model::RequestOrigin,
        tool::ToolContext,
    },
    events::{AgentEvent, EventKind, EventSender},
    model::{self, Message, ModelClient, Output, ToolResult},
    tools::ToolRegistry,
    usage::{ContextUsage, UsageSnapshot},
};

const SYSTEM: &str = "You are a coding agent. Use tools to inspect and change the workspace. Load relevant skills before acting, then report clearly.";

#[derive(Clone, Copy)]
struct ContextBaseline {
    tokens: u64,
    messages: usize,
    prompt_key: u64,
}

pub(crate) struct Agent {
    model: Arc<ModelClient>,
    tools: ToolRegistry,
    events: EventSender,
    source: &'static str,
    // Keep complete assistant blocks, including tool-use IDs. Future persistence
    // must repair incomplete exchanges without blindly replaying side effects.
    messages: Vec<Message>,
    previous_summary: Option<String>,
    context_baseline: Option<ContextBaseline>,
    auto_blocked: bool,
}

impl Agent {
    pub(crate) fn new(
        model: Arc<ModelClient>,
        tools: ToolRegistry,
        events: EventSender,
        source: &'static str,
    ) -> Self {
        Self {
            model,
            tools,
            events,
            source,
            messages: Vec::new(),
            previous_summary: None,
            context_baseline: None,
            auto_blocked: false,
        }
    }

    pub(crate) fn model_name(&self) -> &str {
        self.model.model_name()
    }

    pub(crate) async fn resolve_limits(
        &self,
        configured: Option<model::ModelLimits>,
    ) -> Result<model::LimitResolution> {
        self.model.resolve_limits(configured).await
    }

    pub(crate) fn configure_auto_compaction(&self, policy: crate::CompactionPolicy) -> Result<()> {
        self.model.configure_compaction(policy)
    }

    pub(crate) fn limit_resolution(&self) -> Option<model::LimitResolution> {
        self.model.limit_resolution()
    }

    pub(crate) fn statistics(&self) -> UsageSnapshot {
        UsageSnapshot {
            totals: self.model.usage_totals(),
            estimate: self.estimated_context(),
            context: self.context_baseline.map(|baseline| ContextUsage {
                reported_tokens: baseline.tokens,
                trailing_messages: self.messages.len().saturating_sub(baseline.messages),
            }),
        }
    }

    fn prompt_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        serde_json::json!([
            self.system_prompt(),
            self.previous_summary,
            self.tools.schemas()
        ])
        .to_string()
        .hash(&mut hash);
        hash.finish()
    }

    fn estimated_context(&self) -> Option<crate::ContextEstimate> {
        let limits = self.limit_resolution()?.limits?;
        let tokens = match self.context_baseline.filter(|baseline| {
            baseline.messages <= self.messages.len() && baseline.prompt_key == self.prompt_key()
        }) {
            Some(baseline) => {
                self.messages[baseline.messages..]
                    .iter()
                    .fold(baseline.tokens, |sum, message| {
                        sum.saturating_add(self.model.estimate_message_tokens(message) as u64)
                    })
            }
            None => self.model.estimate_input_tokens(
                &self.system_prompt(),
                self.previous_summary.as_deref(),
                &self.messages,
                &self.tools.schemas(),
            ),
        };
        Some(crate::ContextEstimate {
            tokens,
            window: limits.context_window,
            automatic: self
                .model
                .compaction_policy()
                .is_some_and(|policy| policy.enabled),
        })
    }

    fn publish_statistics(&self) {
        self.emit(EventKind::UsageUpdated(self.statistics()));
    }

    pub(crate) fn enable_sub_agent(&mut self) {
        self.tools
            .enable_sub_agent(Arc::clone(&self.model), self.events.clone());
    }

    pub(crate) fn transcript_context(&self) -> TranscriptContext {
        TranscriptContext {
            system: self.system_prompt(),
            tools: self.tools.schemas(),
        }
    }

    pub(crate) fn prepare_turn(&self, parent: Option<ToolContext>) -> AgentTurn {
        self.model.agent_turn(self.source, parent)
    }

    pub(crate) async fn compact(
        &mut self,
        options: compaction::Options,
    ) -> Result<compaction::Outcome> {
        self.compact_in_context(options, self.source.into()).await
    }

    async fn compact_in_context(
        &mut self,
        options: compaction::Options,
        origin: RequestOrigin,
    ) -> Result<compaction::Outcome> {
        let before = self.statistics();
        let result = compaction::compact(
            &self.model,
            origin,
            &mut self.messages,
            &mut self.previous_summary,
            options,
        )
        .await;
        if matches!(result, Ok(compaction::Outcome::Compacted { .. })) {
            self.context_baseline = None;
            self.auto_blocked = false;
        }
        if self.statistics() != before {
            self.publish_statistics();
        }
        result
    }

    async fn prepare_request(&mut self, origin: RequestOrigin) -> Result<()> {
        let Some(policy) = self
            .model
            .compaction_policy()
            .filter(|policy| policy.enabled)
        else {
            return Ok(());
        };
        let Some(estimate) = self.estimated_context() else {
            return Ok(());
        };
        let Some(limits) = self
            .limit_resolution()
            .and_then(|resolution| resolution.limits)
        else {
            return Ok(());
        };
        compaction::validate_tool_exchanges(&self.messages)?;
        self.publish_statistics();
        let planned = crate::budget::plan(limits, policy, estimate.tokens)?;
        if planned.compact && !self.auto_blocked {
            // One failed/useless attempt per turn. No loop and no tool replay.
            self.auto_blocked = true;
            let result = self
                .compact_in_context(
                    compaction::Options {
                        keep_recent_tokens: planned.keep_recent_tokens,
                        max_input_chars: planned.max_summary_chars,
                        max_output_tokens: planned.summary_output_tokens,
                    },
                    origin,
                )
                .await;
            match result {
                Ok(compaction::Outcome::Compacted { .. }) => {}
                Ok(compaction::Outcome::Unchanged) => {
                    self.emit(EventKind::Warning(
                        "Automatic compaction found no safe prefix to retire.",
                    ));
                }
                Err(_) => {
                    self.emit(EventKind::Warning(
                        "Automatic compaction failed; the previous context is unchanged.",
                    ));
                }
            }
        }
        let input = self
            .estimated_context()
            .map_or(estimate.tokens, |estimate| estimate.tokens);
        let after = crate::budget::plan(limits, policy, input)?;
        anyhow::ensure!(
            after.fits,
            "next model request exceeds the estimated context budget; reduce input or compact with a smaller recent target"
        );
        Ok(())
    }

    async fn complete_model(&self, origin: RequestOrigin) -> Result<model::Completion> {
        let system = self.system_prompt();
        let tools = self.tools.schemas();
        match self.previous_summary.as_deref() {
            Some(summary) => {
                self.model
                    .complete_with_summary(origin, &system, summary, &self.messages, &tools)
                    .await
            }
            None => {
                self.model
                    .complete(origin, &system, &self.messages, &tools)
                    .await
            }
        }
    }

    fn system_prompt(&self) -> String {
        let descriptions = self.tools.skill_descriptions();
        if descriptions.is_empty() {
            SYSTEM.to_string()
        } else {
            format!(
                "{SYSTEM}\n\nAvailable skills (use load_skill for full instructions):\n{descriptions}"
            )
        }
    }

    fn emit(&self, kind: EventKind) -> bool {
        // A closed frontend must not turn a completed tool into a failed call.
        self.events
            .send(AgentEvent {
                source: self.source,
                kind,
            })
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) async fn run(&mut self, prompt: &str) -> Result<String> {
        self.run_with_parent(prompt, None).await
    }

    pub(crate) async fn run_with_parent(
        &mut self,
        prompt: &str,
        parent: Option<ToolContext>,
    ) -> Result<String> {
        let turn = self.prepare_turn(parent);
        self.run_in_turn(prompt, turn).await
    }

    pub(crate) async fn run_in_turn(
        &mut self,
        prompt: &str,
        mut turn: AgentTurn,
    ) -> Result<String> {
        self.messages.push(Message::User(prompt.to_owned()));
        self.auto_blocked = false;
        loop {
            if let Err(error) = self.prepare_request(turn.origin()).await {
                self.publish_statistics();
                turn.finish(TurnEnd::ContextBudgetFailed);
                return Err(error);
            }
            self.emit(EventKind::ModelStarted);
            // Only ModelClient retries HTTP. History changes and tool execution
            // stay outside it, for parent and child alike.
            let model::Completion {
                reply,
                request_id,
                usage,
            } = match self.complete_model(turn.model_call()).await {
                Ok(completion) => completion,
                Err(error) => {
                    self.publish_statistics();
                    turn.finish(TurnEnd::ModelFailed);
                    return Err(error);
                }
            };
            self.messages.push(Message::Assistant(reply.clone()));
            if let Some(tokens) = usage.context_tokens() {
                self.context_baseline = Some(ContextBaseline {
                    tokens,
                    messages: self.messages.len(),
                    prompt_key: self.prompt_key(),
                });
            }
            self.publish_statistics();
            let mut results = Vec::new();
            for output in &reply.output {
                match output {
                    Output::Text(text) => {
                        let context = turn.assistant_text(request_id, text);
                        let enqueued =
                            self.emit(EventKind::AssistantText(text.clone(), Some(context)));
                        turn.text_emitted(context, text, enqueued);
                    }
                    Output::ToolCall(call) => {
                        let execution = turn.tool_execution(request_id, call);
                        self.emit(EventKind::ToolStarted {
                            name: call.name.clone(),
                            input: call.arguments.clone(),
                        });
                        let (output, is_error) = self
                            .tools
                            .execute_in_context(
                                &call.name,
                                &call.arguments,
                                Some(execution.context()),
                            )
                            .await;
                        execution.finish(&output, is_error);
                        turn.tool_result();
                        self.emit(EventKind::ToolFinished {
                            name: call.name.clone(),
                            output: output.clone(),
                            is_error,
                        });
                        results.push(ToolResult {
                            tool_call_id: call.id.clone(),
                            content: output,
                            is_error,
                        });
                    }
                }
            }
            if results.is_empty() {
                turn.decision(request_id, Decision::StopNoToolResults, 0);
                let text = reply.text();
                turn.finish(TurnEnd::NoToolResults {
                    returned_text_bytes: text.len(),
                });
                return Ok(text);
            }
            turn.decision(
                request_id,
                Decision::ContinueAfterToolResults,
                results.len(),
            );
            self.messages.push(Message::ToolResults(results));
            self.publish_statistics();
        }
    }

    // Fixture-only access; production callers cannot mutate agent state.
    #[cfg(test)]
    pub(crate) fn history(&self) -> &[Message] {
        &self.messages
    }

    #[cfg(test)]
    pub(crate) fn with_history(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    #[cfg(test)]
    pub(crate) fn tools_for_test(&mut self) -> &mut ToolRegistry {
        &mut self.tools
    }

    #[cfg(test)]
    pub(crate) fn model_for_test(&self) -> &Arc<ModelClient> {
        &self.model
    }

    #[cfg(test)]
    pub(crate) fn emit_for_test(&self, kind: EventKind) -> bool {
        self.emit(kind)
    }

    #[cfg(test)]
    pub(crate) fn system_prompt_for_test(&self) -> String {
        self.system_prompt()
    }
}

pub(crate) mod compaction;

#[cfg(all(test, unix))]
mod tests;
