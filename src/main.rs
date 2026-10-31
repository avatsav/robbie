use std::io::{self, Write};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

mod tools;
use tools::ToolRegistry;

const SYSTEM: &str = "You are a coding agent. Use bash to inspect and change the workspace. Act first, then report clearly.";

struct Agent {
    client: reqwest::Client,
    api_key: String,
    model: String,
    provider: Option<String>,
    tools: ToolRegistry,
    // Keep complete assistant content blocks, including tool-use IDs, for the next request.
    // TODO: When adding conversation persistence or recovery, repair incomplete tool
    // exchanges before resuming. A missing result may mean the tool's outcome is
    // unknown, not that it was cancelled; do not blindly retry side effects.
    messages: Vec<Value>,
}

impl Agent {
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
                        let name = block["name"].as_str().unwrap_or("");
                        let (output, is_error) = self.tools.execute(name, &block["input"]).await;
                        println!("{output}");
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
        tools: ToolRegistry::new(),
        messages: Vec::new(),
    };
    agent.tools.enable_sub_agent(
        agent.client.clone(), agent.api_key.clone(), agent.model.clone(), agent.provider.clone(),
    );

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
