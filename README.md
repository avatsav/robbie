# Robbie

Robbie is a tiny AI agent built as an experiment.

## Development

Requires Rust with `rustfmt` and Clippy, Python 3.10+, and [Ruff](https://docs.astral.sh/ruff/installation/).

Enable the native Git pre-commit hook once per clone:

```sh
rustup component add rustfmt clippy
git config --local core.hooksPath .githooks
```

The hook checks Rust formatting, runs strict Clippy, and checks Python formatting
and linting. It checks the working tree, not a separate snapshot of staged files;
stage the intended versions before committing. It never edits or stages files.

Format before staging:

```sh
cargo fmt --all
ruff format tests
```

Run the commit checks manually:

```sh
.githooks/pre-commit
```

Tests remain a separate, explicit step:

```sh
cargo test --locked
python3 -m unittest discover -s tests -p 'test_*.py'
python3 tests/blocked_tool_exit.py
```
