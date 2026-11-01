use std::io::{self, Write};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

mod events;
mod fullscreen;
mod markdown;
mod tools;
mod ui;
use events::{AgentEvent, EventKind, EventSender};
use tools::ToolRegistry;

const SYSTEM: &str = "You are a coding agent. Use bash to inspect and change the workspace. Act first, then report clearly.";

struct Agent {
    client: reqwest::Client,
    api_key: String,
    model: String,
    provider: Option<String>,
    tools: ToolRegistry,
    events: EventSender,
    source: &'static str,
    // Keep complete assistant content blocks, including tool-use IDs, for the next request.
    // TODO: When adding conversation persistence or recovery, repair incomplete tool
    // exchanges before resuming. A missing result may mean the tool's outcome is
    // unknown, not that it was cancelled; do not blindly retry side effects.
    messages: Vec<Value>,
}

impl Agent {
    fn emit(&self, kind: EventKind) {
        // A closed frontend must not turn a completed tool into a failed call.
        let _ = self.events.send(AgentEvent { source: self.source, kind });
    }

    async fn run(&mut self, prompt: &str) -> Result<String> {
        self.messages.push(json!({"role": "user", "content": prompt}));

        loop {
            let mut request = json!({
                    "model": self.model,
                    "max_tokens": 8000,
                    "system": SYSTEM,
                    "messages": self.messages,
                    "tools": self.tools.schemas()
                });
            if let Some(provider) = &self.provider {
                request["provider"] = json!({ "only": [provider] });
            }
            self.emit(EventKind::ModelStarted);
            let response = self.client.post("https://openrouter.ai/api/v1/messages")
                .bearer_auth(&self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&request)
                .send().await.context("model request failed")?;
            let status = response.status();
            let body: Value = response.json().await.context("invalid model response")?;
            if !status.is_success() {
                bail!("model API returned {status}: {body}");
            }
            let blocks = body["content"].as_array().context("model response has no content blocks")?;
            self.messages.push(json!({"role": "assistant", "content": blocks}));

            let mut results = Vec::new();
            for block in blocks {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str() {
                            self.emit(EventKind::AssistantText(text.to_string()));
                        }
                    }
                    Some("tool_use") => {
                        let id = block["id"].as_str().context("tool use has no ID")?;
                        let name = block["name"].as_str().unwrap_or("");
                        self.emit(EventKind::ToolStarted { name: name.to_string(), input: block["input"].clone() });
                        let (output, is_error) = self.tools.execute(name, &block["input"]).await;
                        self.emit(EventKind::ToolFinished { name: name.to_string(), output: output.clone(), is_error });
                        results.push(json!({"type": "tool_result", "tool_use_id": id, "content": output, "is_error": is_error}));
                    }
                    _ => {}
                }
            }
            if results.is_empty() {
                return Ok(response_text(blocks));
            }
            // Tool results are user messages in the Anthropic messages protocol.
            self.messages.push(json!({"role": "user", "content": results}));
        }
    }
}

// Only the final model response is returned, not intermediate tool-use text.
fn response_text(blocks: &[Value]) -> String {
    blocks.iter()
        .filter(|block| block["type"].as_str() == Some("text"))
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, PartialEq, Eq)]
enum Frontend {
    Fullscreen,
    Inline,
    Console,
}

fn select_frontend(args: &[String]) -> Frontend {
    if args.iter().any(|arg| arg == "--console") {
        Frontend::Console
    } else if args.iter().any(|arg| arg == "--inline") {
        Frontend::Inline
    } else {
        Frontend::Fullscreen
    }
}

fn main() -> Result<()> {
    run_with_runtime(run())
}

fn run_with_runtime(future: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = runtime.block_on(future);
    // Running blocking tasks cannot be aborted. Bound shutdown rather than
    // trapping the terminal session forever; process exit ends remaining threads.
    runtime.shutdown_timeout(std::time::Duration::from_millis(100));
    result
}

async fn run() -> Result<()> {
    dotenvy::dotenv().ok();
    let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut agent = Agent {
        client: reqwest::Client::new(),
        api_key: std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is not set")?,
        model: std::env::var("OPENROUTER_MODEL").context("OPENROUTER_MODEL is not set")?,
        provider: std::env::var("OPENROUTER_PROVIDER").ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        tools: ToolRegistry::new(),
        events,
        source: "main",
        messages: Vec::new(),
    };
    agent.tools.enable_sub_agent(
        agent.client.clone(), agent.api_key.clone(), agent.model.clone(), agent.provider.clone(), agent.events.clone(),
    );

    match select_frontend(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Frontend::Fullscreen => return fullscreen::run(agent, receiver).await,
        Frontend::Inline => return ui::run(agent, receiver).await,
        Frontend::Console => {}
    }

    println!("Warning: the agent can run shell commands without asking. Use only in a trusted environment. Type exit() to quit.");
    loop {
        print!("\nYou: ");
        io::stdout().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 || input.trim() == "exit()" {
            break;
        }
        if !input.trim().is_empty() {
            run_console(&mut agent, input.trim(), &mut receiver).await?;
        }
    }
    Ok(())
}

// Drive the agent and display events concurrently, then drain any final events
// before showing the next blocking stdin prompt.
async fn run_console(
    agent: &mut Agent,
    prompt: &str,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) -> Result<()> {
    let run = agent.run(prompt);
    tokio::pin!(run);
    let result = loop {
        tokio::select! {
            result = &mut run => break result,
            Some(event) = receiver.recv() => events::render_console(event),
        }
    };
    while let Ok(event) = receiver.try_recv() {
        events::render_console(event);
    }
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_is_default_with_inline_and_console_opt_ins() {
        let choose = |args: &[&str]| select_frontend(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
        assert_eq!(choose(&[]), Frontend::Fullscreen);
        assert_eq!(choose(&["--fullscreen"]), Frontend::Fullscreen);
        assert_eq!(choose(&["--inline"]), Frontend::Inline);
        assert_eq!(choose(&["--console"]), Frontend::Console);
    }

    #[test]
    fn shutdown_does_not_wait_forever_for_blocking_work() {
        use std::time::{Duration, Instant};
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::blocked_shutdown_fixture", "--ignored", "--nocapture"])
            .stdout(std::process::Stdio::null())
            .spawn().unwrap();
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
                loop { std::thread::park(); }
            });
            ready.await?;
            Ok(())
        }).unwrap();
    }

    #[test]
    fn response_text_keeps_only_text_blocks_in_order() {
        let blocks = vec![
            json!({"type": "text", "text": "First"}),
            json!({"type": "tool_use", "id": "call", "name": "bash"}),
            json!({"type": "text", "text": "Second"}),
        ];
        assert_eq!(response_text(&blocks), "First\nSecond");
        assert_eq!(response_text(&[]), "");
    }
}
