# Development

## Setup

Install Rust using [rustup](https://rustup.rs), then run from the repository root:

```sh
rustup component add rustfmt clippy
git config --local core.hooksPath .githooks
```

## Build

```sh
cargo build --locked
cargo build --locked -p robbie-core
cargo build --locked --workspace --release
```

## Run

Set `OPENROUTER_API_KEY` and `OPENROUTER_MODEL` in `.env` or your shell, then:

```sh
cargo run --locked
```

Press Ctrl+C to exit. Restart after changing environment variables.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo check --locked -p robbie-core --features test-support
cargo test --locked --workspace
cargo test --locked --workspace --all-features
python3 -m unittest discover -s tests -p 'test_*.py'
```

Verify Unix PTY target/fixture discovery without running terminal scenarios:

```sh
python3 tests/terminal/blocked_tool_exit.py --check-discovery
```

Optional Unix PTY check (known resize/Ctrl+C timeout):

```sh
python3 tests/terminal/blocked_tool_exit.py
```

## Environment variables

| Variable | Values | Default | Purpose |
| --- | --- | --- | --- |
| `OPENROUTER_API_KEY` | API key | Required | OpenRouter authentication. |
| `OPENROUTER_MODEL` | OpenRouter model ID | Required | Model used by parent and child agents. |
| `OPENROUTER_PROVIDER` | Provider slug; empty or unset | Unrestricted | Restrict upstream routing to one provider. |
| `ROBBIE_PROMPT_CACHE` | `short`, `off` | `short` | Generate prompt-cache markers using provider defaults, or omit them. |
| `ROBBIE_TRANSCRIPT` | `1`; any other value disables | Disabled | Capture sensitive prompts, tool payloads and model bodies alongside diagnostics. |
| `ROBBIE_MASCOT` | `off`; any other value enables | Enabled | Disable the mascot for the session. |
| `XDG_STATE_HOME` | Absolute directory; empty/relative ignored | `$HOME/.local/state` | Base directory for `robbie/sessions/` diagnostics. |
| `HOME` | Absolute home directory | Inherited | Diagnostics fallback and home-path abbreviation in the UI. |
