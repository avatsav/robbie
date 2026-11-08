//! Agent conversation and execution loop, separate from startup and the frontend.
use std::sync::Arc;

use anyhow::Result;

use crate::{
    diagnostics::{
        TranscriptContext,
        agent::{AgentTurn, Decision, TurnEnd},
        tool::ToolContext,
    },
    events::{AgentEvent, EventKind, EventSender},
    model::{self, Message, ModelClient, Output, ToolResult},
    tools::ToolRegistry,
};

const SYSTEM: &str = "You are a coding agent. Use tools to inspect and change the workspace. Load relevant skills before acting, then report clearly.";

pub(crate) struct Agent {
    model: Arc<ModelClient>,
    tools: ToolRegistry,
    events: EventSender,
    source: &'static str,
    // Keep complete assistant blocks, including tool-use IDs. Future persistence
    // must repair incomplete exchanges without blindly replaying side effects.
    messages: Vec<Message>,
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
        }
    }

    pub(crate) fn model_name(&self) -> &str {
        self.model.model_name()
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
        loop {
            self.emit(EventKind::ModelStarted);
            // Only complete() retries HTTP. History changes and tool execution
            // stay outside it, for parent and child alike.
            let model::Completion { reply, request_id } = match self
                .model
                .complete(
                    turn.model_call(),
                    &self.system_prompt(),
                    &self.messages,
                    &self.tools.schemas(),
                )
                .await
            {
                Ok(completion) => completion,
                Err(error) => {
                    turn.finish(TurnEnd::ModelFailed);
                    return Err(error);
                }
            };
            self.messages.push(Message::Assistant(reply.clone()));
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
