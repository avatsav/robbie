# Robbie

A tiny, terminal-first AI coding agent written in Rust. It talks to OpenRouter and can inspect the workspace, edit files, run commands, and delegate work to a child agent.

## Features

- **Three frontends:** full-screen TUI (default), native-scrollback inline TUI (`--inline`), or plain console (`--console`)
- **Tools:** `bash`, `read_file`, `write_file`, `edit_file`, `todo`, and `sub_agent`
- **Streaming events:** assistant text and tool calls are rendered as they happen
- **Small and readable:** straightforward async Rust, no agent framework

## Quick start

Create a `.env` in the project root:

```sh
OPENROUTER_API_KEY=sk-or-...
OPENROUTER_MODEL=anthropic/claude-sonnet-4
OPENROUTER_PROVIDER=   # optional: pin a provider, e.g. anthropic
```

Then run:

```sh
cargo run                # full-screen TUI (default)
cargo run -- --inline     # native terminal scrollback
cargo run -- --console    # plain line-based console
```

## Usage

Type a request and press Enter. In the full-screen UI, use the mouse wheel or PgUp/PgDn to scroll, and End to return to the latest output. Ctrl+C quits; Ctrl+D also quits when the draft is empty. Exiting restores the shell without printing the conversation into scrollback. Use `--inline` if you prefer native terminal selection and scrollback, or `--console` for plain text (`exit()` quits).

In either TUI, Ctrl+O cycles **Concise → Relaxed → Full**, starting in Concise. Relaxed previews the Bash output tail; Full shows returned tool results within each tool's output limits; Concise shows headers and Bash exit codes only. Changing the mode affects subsequent results, not earlier output.

> [!WARNING]
> The agent can run shell commands without asking. Only use it in a trusted workspace.

## Project layout

```
src/
├── main.rs         # agent loop and OpenRouter client
├── ui.rs           # shared UI state/rendering and inline frontend
├── fullscreen.rs   # default full-screen UI and scrolling
├── events.rs       # event types shared by agent and UI
├── tools.rs        # tool registry and the bash tool
└── tools/          # file, todo, and sub-agent tools
```

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt
```

See `LEARNING.md` for design notes and background.
