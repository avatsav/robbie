# Robbie

A small Rust coding agent, following the [minimal agent loop chapter](https://github.com/wulawulu/learn-claude-code-rs/blob/main/s01_agent_loop/s01.en.md).

## Run

Create a `.env` file in the project root:

```dotenv
OPENROUTER_API_KEY=your_openrouter_api_key
OPENROUTER_MODEL=anthropic/claude-sonnet-4.6
# Optional: restrict requests to this OpenRouter provider slug
# OPENROUTER_PROVIDER=anthropic
```

Run `cargo run` and enter a prompt. Type `exit()` or send EOF to quit. Choose an available [OpenRouter model](https://openrouter.ai/models) that supports tool calling. Robbie uses OpenRouter's [Anthropic-compatible Messages endpoint](https://openrouter.ai/docs/api/api-reference/anthropic-messages/create-a-message); no Anthropic API key is needed. When `OPENROUTER_PROVIDER` is set, requests use OpenRouter's `provider.only` restriction; they can fail if that provider cannot serve the selected model. Leave it unset or blank for automatic routing. See [provider routing](https://openrouter.ai/docs/guides/routing/provider-selection) for provider slugs and behavior.

## The loop

1. Add the user's prompt to the conversation history.
2. Send the history and the `bash` tool schema to the model.
3. Save the assistant's response. For each tool request, run the command and return its output as a tool result.
4. Repeat until the model responds without requesting a tool. Keep the history for the next prompt.

**Safety:** Commands run automatically with your user account's permissions, in the directory where Robbie starts. There is no approval prompt or sandbox. The timeout kills the shell process, but may not stop subprocesses. Only run this in an isolated, trusted environment; do not use it with sensitive files or credentials.
