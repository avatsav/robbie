# Development

From the repository root, enable pre-commit formatting and lint checks once per clone:

```sh
rustup component add rustfmt clippy
git config --local core.hooksPath .githooks
```
