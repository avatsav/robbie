use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::process::Command;

const SYSTEM: &str = "You are a coding agent. Use bash to inspect and change the workspace. Act first, then report clearly.";

struct Agent {
    client: reqwest::Client,
    api_key: String,
    model: String,
    provider: Option<String>,
    // Keep complete assistant content blocks, including tool-use IDs, for the next request.
    messages: Vec<Value>,
}

impl Agent {
    async fn run(&mut self, prompt: &str) -> Result<()> {
        self.messages.push(json!({"role": "user", "content": prompt}));

        loop {
            let mut request = json!({
                    "model": self.model,
                    "max_tokens": 8000,
                    "system": SYSTEM,
                    "messages": self.messages,
                    "tools": [{
                        "name": "bash",
                        "description": "Run a shell command in the current workspace.",
                        "input_schema": {
                            "type": "object",
                            "properties": { "command": { "type": "string" } },
                            "required": ["command"]
                        }
                    }]
                });
            if let Some(provider) = &self.provider {
                request["provider"] = json!({ "only": [provider] });
            }
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
                            println!("{text}");
                        }
                    }
                    Some("tool_use") => {
                        let id = block["id"].as_str().context("tool use has no ID")?;
                        let (output, is_error) = match (block["name"].as_str(), block["input"]["command"].as_str()) {
                            (Some("bash"), Some(command)) => {
                                println!("$ {command}");
                                run_bash(command).await
                            }
                            _ => ("Unknown tool or invalid command".to_string(), true),
                        };
                        println!("{output}");
                        results.push(json!({"type": "tool_result", "tool_use_id": id, "content": output, "is_error": is_error}));
                    }
                    _ => {}
                }
            }
            if results.is_empty() {
                break;
            }
            // Tool results are user messages in the Anthropic messages protocol.
            self.messages.push(json!({"role": "user", "content": results}));
        }
        Ok(())
    }
}

async fn run_bash(command: &str) -> (String, bool) {
    let mut process = Command::new("sh");
    process.arg("-c").arg(command).kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(120), process.output()).await {
        Ok(Ok(output)) => {
            let combined = [output.stdout, output.stderr].concat();
            let text = String::from_utf8_lossy(&combined);
            let text = text.trim();
            let result = if text.is_empty() { "(no output)".to_string() } else { text.chars().take(50_000).collect() };
            (format!("{result}\n(exit status: {})", output.status), !output.status.success())
        }
        Ok(Err(error)) => (format!("Could not run command: {error}"), true),
        Err(_) => ("Command timed out after 120 seconds".to_string(), true),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let mut agent = Agent {
        client: reqwest::Client::new(),
        api_key: std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is not set")?,
        model: std::env::var("OPENROUTER_MODEL").context("OPENROUTER_MODEL is not set")?,
        provider: std::env::var("OPENROUTER_PROVIDER").ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        messages: Vec::new(),
    };

    println!("Warning: the agent can run shell commands without asking. Use only in a trusted environment. Type exit() to quit.");
    loop {
        print!("\nYou: ");
        io::stdout().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 || input.trim() == "exit()" {
            break;
        }
        if !input.trim().is_empty() {
            agent.run(input.trim()).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shell_returns_output_and_exit_status() {
        let (output, error) = run_bash("printf hello").await;
        assert!(output.contains("hello"));
        assert!(!error);
        let (_, error) = run_bash("exit 1").await;
        assert!(error);
    }
}
