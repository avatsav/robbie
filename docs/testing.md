# Test organization

## Test scope and ownership

- Rust tests that need crate-private state stay under the owning crate's `src/`, behind `#[cfg(test)]`. Cross-module scenarios can also live here; Cargo still compiles them as unit tests.
- Each crate's `tests/*.rs` is reserved for Cargo integration tests of an exposed library or executable interface. Expose production interfaces for real consumers, not merely to relocate tests.
- `tests/system/` contains Python black-box tests of repository tooling. `tests/terminal/` contains the optional Unix PTY driver. These already test system/integration behavior; changing language is unnecessary.

Frontend ownership matters:

| Location | Responsibility |
| --- | --- |
| `crates/robbie-cli/src/ui/tests/` | Draft input, presentation, and tool-output formatting. |
| `crates/robbie-core/src/agent/tests/` | Agent-turn lifecycle, tool-span outcomes, and delegation causality. |
| `crates/robbie-core/src/session/tests.rs` | Terminal-independent construction, prepared-turn ownership, and session execution. |
| `crates/robbie-core/tests/session.rs` | External-consumer session/tool/activity/recording contract; requires `test-support`. |
| `crates/robbie-cli/src/tests/configuration.rs` | CLI environment resolution and recording startup; subprocess fixtures isolate environment changes. |
| `crates/robbie-cli/src/fullscreen/tests/` | Frontend orchestration, transcript, selection, and viewport behavior. |
| `crates/robbie-cli/src/fullscreen/tests/diagnostics/` | Frontend receipt/append/completion evidence through the real core/channel path; not proof of visible terminal pixels. |
| `crates/robbie-core/src/diagnostics/tests/` | Journal storage, privacy, configuration, and failure handling. |

## Suite convention

- Keep short, single-concern unit suites inline or in `<module>/tests.rs`.
- Review a suite around **300 lines or 15 tests**, or sooner when it covers distinct behaviors. This is a project review trigger, not a Rust rule or hard limit.
- Split larger, mixed-concern suites into `<module>/tests/<behavior>.rs`. Keep `<module>/tests.rs` as the test-module index and home for helpers shared only by that suite. Child files use `use super::*` to retain private access.
- Name files for behaviors such as `input`, `retries`, or `lifecycle`; avoid numbered parts and repeated `_tests` suffixes beneath a `tests` directory.
- Keep specialized fixtures with their suite. Shared fixtures live in `crates/robbie-core/src/test_support/`. Loopback session setup, opaque recording fault controls, journal readers, and the one-shot PTY read helper compile under `#[cfg(test)]` or the default-off `test-support` feature. CLI enables that feature through its dev-dependency, never its production dependency. Raw model/agent inspection remains core-local and `#[cfg(test)]`-only.
- Preserve test cases when reorganizing. Update subprocess fixture selectors and compare test inventories as well as running the tests.

## Checks and limits

Commands are in [Development](development.md#checks). Use `--workspace` to include both crates; the default member is `robbie-cli`. `cargo test` does not discover Python tests. Python unittest discovery includes `tests/system/`; the terminal driver is invoked explicitly.

The PTY driver retains its known resize/Ctrl+C timeout. Moving it does not fix that issue or establish terminal restoration as passing.
