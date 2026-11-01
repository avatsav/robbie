# Development

From the repository root, enable pre-commit formatting and lint checks once per clone:

```sh
rustup component add rustfmt clippy
git config --local core.hooksPath .githooks
```

## Keyboard shortcuts

- **Ctrl+O** cycles tool-output detail.
- **Ctrl+G** shows or hides the mascot, whether rendered as an image or tiled fallback. Works while idle or busy; hiding it returns its space to the input. The setting lasts for the current session. `ROBBIE_MASCOT=off` disables both renderers and cannot be overridden by the shortcut.
- **Ctrl+C** exits.
