# Code, architecture, and Rust review

Reviewed the complete tree at `f9f86ff`, with three independent specialist passes. This is a baseline review: locations and findings describe that revision, not the state after fixes. Implementation progress is tracked below.

## Verdict

The architecture is sound for this project's size. No broad rewrite is warranted. Rust ownership, retry placement, protocol separation, diagnostics, and child-agent isolation are generally well handled.

The main weaknesses are execution-resource safety, inconsistent Unicode handling, and a few interfaces that expose implementation details.

## Correctness and robustness

### 1. High — Shell descendants can survive timeout or cancellation

Location: `src/tools.rs:134–136`.

`kill_on_drop(true)` kills the spawned `sh`, not its entire process tree. Pipelines, compound commands, or background jobs can therefore continue running—and modifying files—after the tool times out or Robbie exits.

Checked against Tokio's implementation and confirmed with an isolated OS fixture.

Recommendation: give each command a dedicated process group, terminate that group on timeout/drop, and reap the direct child. Keep cancellation protection active until output collection finishes.

Missing tests: timeout and dropped-future cases where a descendant must not subsequently write a marker.

This is lifecycle management, not sandboxing or rollback. Descendants that deliberately escape their group need not be contained by this approach.

### 2. High — Output limits do not bound memory consumption

Locations: `src/tools.rs:136–146`, `src/tools/files.rs:54–74`.

Both limits apply after acquisition:

- Bash collects all stdout/stderr, allocates another combined buffer, then truncates.
- `read_file` reads the entire file before applying its line limit or character cap. Even `limit: 0` reads everything.

A noisy command or multi-gigabyte log can exhaust memory despite returning only a small result.

Recommendation:

- Drain stdout and stderr concurrently into bounded tail buffers.
- Read file prefixes incrementally, stopping at the applicable limit.
- Avoid line readers that can still allocate an unlimited first line.

Test bounded acquisition, not just final output length. This is separate from deferred conversation compaction.

### 3. Medium — Structurally invalid response blocks bypass validation

Location: `src/model/protocol.rs:92–122`.

The decoder preserves genuine unknown blocks, which is appropriate. But its catch-all also accepts malformed entries such as `null`, scalars, and objects without a string `type`.

A valid tool call followed by `null` passes decoding, allowing the tool to execute and malformed continuation data to enter history.

Recommendation: require every content entry to be an object with a string `type`; continue preserving unknown well-formed typed objects.

Add malformed entries after a valid tool call and assert rejection before any tool starts.

### 4. Medium — Unicode clipping can exceed its allocated width

Location: `src/ui.rs:521–557`.

Clipping measures the original string using whole-string width, but constructs the clipped result using individual character widths. These disagree for some emoji sequences.

Confirmed against the locked dependencies: `single_line("abc#\u{fe0f}x", 5)` returns `"abc#\u{fe0f}…"`, which occupies six cells despite its five-cell budget.

This can consume space reserved for tool-status footers or neighboring content. Reverse clipping can also split grapheme clusters.

Recommendation: clip at grapheme boundaries and measure retained text consistently with rendering.

### 5. Medium — Transcript wrapping splits displayed characters

Locations: `src/markdown.rs:172–178`, `src/ui.rs:412–425`.

`textwrap` performs forced breaks at Unicode scalar boundaries, not extended-grapheme boundaries. Confirmed at a two-cell body width: `👩‍💻` becomes separate rows containing `👩‍` and `💻`.

The displayed glyph changes, and copying those wrapped rows inserts a newline inside the original character.

Recommendation: retain word wrapping, but make forced breaks grapheme-aware. Cover styled Markdown and submitted prompts.

### 6. Medium — Backspace removes a scalar, not a displayed character

Location: `src/ui.rs:394–396`.

`String::pop()` partially deletes composed characters:

- `e` plus combining accent becomes plain `e`.
- `👩‍💻` becomes `👩‍`.

The draft layout and selection already understand graphemes; editing should use the same model.

Recommendation: truncate at the last extended-grapheme boundary. Add combining-accent, emoji-modifier, and ZWJ input tests.

## Architecture and organization

These are focused design improvements, not demonstrated correctness failures.

### Extract the agent into its own module

Locations: `src/main.rs:23–33`, `src/tools/sub_agent.rs:17–24`, `src/fullscreen.rs:333–361`.

Because `Agent` lives at crate root, descendant modules can access its private fields. Fullscreen reaches into its model/source, while delegation constructs it through field literals and initializes its history directly.

Move it to `src/agent.rs`, with private fields and a small interface for construction, model-label access, turn preparation/execution, and child construction without exposing history initialization.

Preserve the explicit shared UI/agent diagnostic identity. No agent trait, factory framework, or library/workspace split is necessary.

### Capture workspace identity once

Locations: `src/main.rs:188`, `src/tools/files.rs:104–106`, `src/tools.rs:134–136`, `src/ui/workspace.rs:65–69`.

Several modules independently depend on process cwd. File-tool tests already need subprocess isolation because changing cwd would race other tests.

Capture one workspace path during startup and pass it to tools, skill scanning, and the workspace monitor. Set Bash's `current_dir` explicitly.

This improves locality and testability without introducing a filesystem abstraction.

### Separate transcript invalidation from animation redraws

Locations: `src/fullscreen.rs:99–140`, `src/markdown.rs:30–68`.

Every animation draw traverses and reconstructs the whole transcript. Markdown parsing is cached, but cached rows are still deeply cloned. This happens on the 50-ms animation tick even when history is unchanged.

The repeated work is verified; no latency threshold was measured.

Retain flattened rows by width, append new output incrementally, and rebuild only on width changes. Selection can share that cache. No virtualization framework is needed.

### Give shared test fixtures an explicit home

Locations: `src/fullscreen/diagnostics_tests.rs:7–10`, `src/model.rs:366–561`.

Frontend tests import shared HTTP/journal helpers through another module's `tests` namespace.

Move only reusable fixtures into a crate-private `#[cfg(test)] test_support` module. Keep behavioral tests beside their owning modules and retain the real HTTP → agent → tools → frontend coverage.

## Rust usage assessment

Generally idiomatic and appropriately restrained.

Particularly good:

- Immutable `Arc<ModelClient>` sharing with separate child histories and tool state.
- HTTP retries below history mutation and tool execution; no local tool replay.
- Typed conversation output separated from opaque protocol continuation.
- Short-lived diagnostic locks, explicit correlation context, and drop guards.
- Lazy sensitive-payload construction, exact transcript opt-in, private files, and nonfatal recording failure.
- Blocking filesystem work kept off the async executor.
- Terminal cleanup guards and locally owned agent futures rather than detached execution.
- Deterministic mascot scheduling, shared frame catalogues, and renderer-independent tests.
- Concrete composition instead of excessive generic infrastructure.

Do not mechanically apply every Rust-skill suggestion:

- `Box<dyn Tool>` is justified by heterogeneous, stateful tools.
- `async_trait` is justified by that dynamic dispatch.
- Application-level `anyhow` is appropriate.
- A `lib.rs`, Cargo workspace, provider registry, or blanket public-item documentation requirement would not materially improve this binary today.
- Large files containing substantial tests are not automatically poorly organized.

A small future improvement is replacing repeated `(String, bool)` tool results with a named result type, making error semantics explicit without changing behavior. This is not required for the fixes below.

## Test files, library targets, and workspaces

### What the Rust skill recommends for tests

`test-cfg-test-module` recommends colocated `#[cfg(test)] mod tests` unit tests, with `use super::*` to access the owning module's private items. It also recommends grouping large suites into submodules. It does not impose a source-file line limit.

A separate file can still be a colocated unit-test module:

```text
src/ui.rs          # production implementation; declares #[cfg(test)] mod tests;
src/ui/tests.rs    # unit tests; use super::*; private access is retained
```

This is different from top-level `tests/*.rs`: those are separate integration-test crates, which import a library's exposed interface rather than private implementation details.

For Robbie, move large test bodies to sibling test files when that improves navigation. Keep small tests inline, keep behavior-focused groups, and do not split cohesive production modules merely to reduce line counts. Existing examples include `src/ui/mascot/tests.rs` and `src/fullscreen/diagnostics_tests.rs`.

### What `lib.rs` changes

Cargo treats `src/main.rs` as an executable crate root and `src/lib.rs` as a library crate root. One package can contain both. The executable can import that library, as can integration tests and other binaries.

`proj-lib-main-split` generally recommends thin executable entry points and reusable logic in a library. The useful goal here is separating startup from agent behavior. Extracting `agent.rs` within the existing binary achieves that without creating a new external interface. Robbie's unit tests can already exercise its private modules.

Add `lib.rs` when there is a real consumer: another binary, an external Rust caller, or integration tests that need to import a deliberate reusable interface. It is not required to organize modules or use separate unit-test files.

### What a Cargo workspace changes

A workspace groups multiple Cargo packages, usually sharing a lockfile, target directory, and dependency/lint configuration. Each package has its own `Cargo.toml` and produces its own library and/or executable targets.

`proj-workspace-large` recommends workspaces for multiple related crates, not a single binary. Modules provide Robbie's current separation; independent crate boundaries and extra manifests would add overhead without a demonstrated need. A package containing both `main.rs` and `lib.rs` does not itself require a workspace.

## Baseline validation and limits

Passed during review:

- Formatting and strict Clippy.
- 231 Rust tests; 5 ignored.
- 3 Python hook tests.
- Release build.
- Isolated Unicode reproductions and shell-parent termination fixture.

The known PTY anomaly was not rerun. No authenticated model calls were made. This review does not establish the historical missing-reply cause, measured rendering latency, or a sandbox guarantee.

## Implementation order

Each item should be a focused, validated commit. Keep behavior changes separate from mechanical organization changes.

- [x] Stop shell descendants on timeout/cancellation (Unix process-group guard; timeout and cancellation regressions).
- [x] Bound shell-output acquisition (concurrent pipe draining into fixed-size tails; storage and noisy-pipe regressions).
- [x] Bound file-read acquisition (fixed read-ahead, incremental UTF-8 prefix decoding, line/character limits; infinite-source and UTF-8 regressions).
- [x] Reject structurally malformed response blocks before execution (decoder and real-agent HTTP regressions; unknown typed blocks remain lossless).
- [x] Fix Unicode clipping, wrapping, and Backspace (grapheme-safe fragments/clipping/editing; cell-budget, styled Markdown, prompt, and input regressions).
- [x] Extract the agent module and hide its state (private fields, constructor, explicit turn preparation; fixture-only test access).
- [x] Pass an explicit workspace path (startup capture shared by tools/children, skill scanning, diagnostics, and footer monitoring; isolated workspace regressions).
- [x] Cache flattened transcript rows across animation redraws (append-only updates, width invalidation; allocation-reuse, append, resize, selection regressions).
- [x] Extract shared test support (HTTP server, journal queries, headless-agent factory; frontend no longer imports another module's tests).
- [x] Move large inline test suites into colocated test files (UI, fullscreen, model, diagnostics, Markdown); module identities and private unit-test access are unchanged.

Keep the current overall design; strengthen these specific seams rather than adding more architectural layers.
