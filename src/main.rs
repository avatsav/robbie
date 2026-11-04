use std::sync::Arc;

use anyhow::{Context, Result, bail};

mod diagnostics;
mod events;
mod fullscreen;
mod markdown;
mod model;
mod skills;
mod tools;
mod ui;
use diagnostics::{
    agent::{Decision, TurnEnd},
    tool::ToolContext,
};
use events::{AgentEvent, EventKind, EventSender};
use model::{AnthropicMessages, Message, ModelClient, OpenRouter, Output, ToolResult};
use tools::ToolRegistry;

const SYSTEM: &str = "You are a coding agent. Use tools to inspect and change the workspace. Load relevant skills before acting, then report clearly.";

struct Agent {
    model: Arc<ModelClient>,
    tools: ToolRegistry,
    events: EventSender,
    source: &'static str,
    // Keep complete assistant content blocks, including tool-use IDs, for the next request.
    // TODO: When adding conversation persistence or turn resumption, repair
    // incomplete tool exchanges. A missing result may mean the tool's outcome is
    // unknown, not that it was cancelled; do not blindly retry side effects.
    messages: Vec<Message>,
}

impl Agent {
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

    // Headless test convenience. The fullscreen host prepares the root guard
    // first, so its UI span shares the identity even before work is polled.
    #[cfg(test)]
    async fn run(&mut self, prompt: &str) -> Result<String> {
        self.run_with_parent(prompt, None).await
    }

    async fn run_with_parent(
        &mut self,
        prompt: &str,
        parent: Option<ToolContext>,
    ) -> Result<String> {
        let turn = self.model.agent_turn(self.source, parent);
        self.run_in_turn(prompt, turn).await
    }

    async fn run_in_turn(
        &mut self,
        prompt: &str,
        mut turn: diagnostics::agent::AgentTurn,
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
                // Return only the final reply, not intermediate tool-use text.
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
}

fn validate_args(args: &[String]) -> Result<()> {
    if let Some(arg) = args.first() {
        bail!(
            "Unexpected argument {arg:?}. Run Robbie without arguments to start the full-screen UI."
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    validate_args(&std::env::args().skip(1).collect::<Vec<_>>())?;
    run_with_runtime(run())
}

fn run_with_runtime(future: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(future);
    // Running blocking tasks cannot be aborted. Bound shutdown rather than
    // trapping the terminal session forever; process exit ends remaining threads.
    runtime.shutdown_timeout(std::time::Duration::from_millis(100));
    result
}

async fn run() -> Result<()> {
    dotenvy::dotenv().ok();
    let skills_directory = std::env::current_dir()?.join("skills");
    let skills = Arc::new(
        tokio::task::spawn_blocking(move || skills::SkillRegistry::scan(&skills_directory))
            .await
            .context("skill scanning task failed")??,
    );
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    let api_key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is not set")?;
    let model_name = std::env::var("OPENROUTER_MODEL").context("OPENROUTER_MODEL is not set")?;
    let upstream = std::env::var("OPENROUTER_PROVIDER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let recorder = diagnostics::Recorder::start(&model_name);
    let model = Arc::new(ModelClient::new(
        model::client()?,
        OpenRouter::new(api_key, upstream),
        AnthropicMessages,
        model_name,
        recorder.clone(),
    ));
    let mut agent = Agent {
        model,
        tools: ToolRegistry::with_skills(skills),
        events,
        source: "main",
        messages: Vec::new(),
    };
    agent
        .tools
        .enable_sub_agent(Arc::clone(&agent.model), agent.events.clone());

    recorder.session_context(|| diagnostics::TranscriptContext {
        system: agent.system_prompt(),
        tools: agent.tools.schemas(),
    });
    fullscreen::run(agent, receiver, recorder).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_is_the_only_frontend_and_obsolete_flags_are_rejected() {
        assert!(validate_args(&[]).is_ok());
        for arg in ["--inline", "--console", "--fullscreen", "--unknown"] {
            let error = validate_args(&[arg.to_string()]).unwrap_err().to_string();
            assert!(error.contains(arg));
            assert!(error.contains("without arguments"));
        }
    }

    #[test]
    fn shutdown_does_not_wait_forever_for_blocking_work() {
        use std::time::{Duration, Instant};
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::blocked_shutdown_fixture",
                "--ignored",
                "--nocapture",
            ])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("process shutdown waited indefinitely for blocking work");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // Runs only in a child process: its deliberately stuck worker cannot be
    // cleaned up by the test runtime. The parent enforces a hard deadline.
    #[test]
    #[ignore = "subprocess fixture for shutdown regression"]
    fn blocked_shutdown_fixture() {
        run_with_runtime(async {
            let (started, ready) = tokio::sync::oneshot::channel();
            tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                loop {
                    std::thread::park();
                }
            });
            ready.await?;
            Ok(())
        })
        .unwrap();
    }
}
