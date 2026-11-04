# Development

From the repository root, enable pre-commit formatting and lint checks once per clone:

```sh
rustup component add rustfmt clippy
git config --local core.hooksPath .githooks
```

## Session diagnostics

Robbie creates one private JSONL file per session and records model exchanges,
agent turn boundaries/decisions, tool executions, causal delegation links, and
frontend text consumption/turn completion.
This does not enable session resumption or change agent stopping/retry behavior.

Files live under `$XDG_STATE_HOME/robbie/sessions/`, falling back to
`~/.local/state/robbie/sessions/`. Empty/relative XDG values are ignored; an absolute
`HOME` is required for the fallback. Storage inside the current workspace is refused,
including symlinked aliases. Robbie-owned directories must be real directories, not
symlinks. Unix directory/file permissions are `0700`/`0600`; recording is unavailable
on platforms where this implementation cannot enforce private files.

The startup notice shows the filename and capture mode. By default, records contain
only allowlisted metadata:

| Event | Evidence |
| --- | --- |
| `session_started` | Capture mode and requested model |
| `agent_turn_started` | Turn ID and source (`main`/`subagent`); one agent run, with an optional parent tool context |
| `request_started` | Logical request ID, turn ID for agent calls, source, model |
| `attempt_started` / `attempt_finished` | Attempt number, elapsed milliseconds, HTTP status when known, typed outcome; non-2xx error details under `http_error` |
| `retry_scheduled` / `retry_stopped` | Delay and `delay_source` (`backoff`/`retry_after`), or reason recovery stopped; the preceding attempt records the HTTP/transport cause |
| `model_response` | Optional response ID/model, stop reason, token/cache usage, block counts, text bytes |
| `request_finished` | Elapsed milliseconds and success, exchange failure, protocol validation failure, or cancellation |
| `tool_execution_started` | Session-local execution ID, originating turn/request/source, bounded tool classification |
| `tool_execution_finished` | Same IDs/classification, duration, typed outcome, output bytes when a result was returned |
| `agent_decision` | Turn/request IDs, `continue_after_tool_results` or `stop_no_tool_results`, number of results |
| `agent_turn_finished` | Duration, reason (`no_tool_results`, `model_failed`, `cancelled`), cumulative counts, returned text bytes on normal return |
| `agent_text_emitted` | Turn/request/text sequence, raw bytes, `enqueued` or `receiver_closed` delivery |
| `ui_turn_started` | Submitted top-level turn identity and prompt byte count |
| `ui_text_received` | Text identity, owning UI turn, raw received bytes |
| `ui_text_appended` | Same text identity, sanitized Markdown-source bytes and non-whitespace character count appended to pending transcript state |
| `ui_turn_finished` | Duration, outcome, root/other-turn receipt and append totals; returned text bytes only on completion |

Every record has a schema version, session ID, ordered sequence, and Unix timestamp
in milliseconds. Attempts are identified by `(session_id, request_id, attempt)`;
a retry keeps the logical request ID. Non-2xx `attempt_finished` records add
`http_error` with a recognized structured `code`/`error_type` when available,
`category`, `body_state`, `captured_bytes`, and parsed `retry_after`. The latter
reports `missing`, `invalid`, `too_large`, or `delay` with `delay_ms`; raw header
values are never recorded. `retry_scheduled.delay_source` is `retry_after` only
when the header extends the exponential backoff (ties count as `backoff`). A parsed
wait is recorded even on the final attempt or when it exceeds the automatic wait
limit and no retry is scheduled.

Error categories come only from allowlisted structured fields in a complete JSON
error document, never keyword matching on a message. They describe what the provider
reported, not an independently verified cause. A generic 429 does not establish
quota exhaustion or upstream capacity trouble. Unknown labels, HTML, malformed JSON,
and incomplete bodies retain `category=unknown`; the HTTP status is still recorded.

Agent-driven model events also carry their
`turn_id`; a turn can contain several requests. Child `Agent::run` calls get separate
turn IDs. A delegated child's turn start/end also carries a `parent` object with the
parent source, turn ID, request ID and tool execution ID. Join it to the delegating
execution to follow parent → request → tool → child, then use the child's turn ID
for its own requests/tools. Root/unscoped turns omit `parent`; invalid delegation
input produces a tool error without inventing a child turn. Durations use a monotonic clock. Missing usage is absent, not zero;
`text_bytes` counts decoded UTF-8 text, not rendered terminal cells or proof of UI
consumption. Unknown stop reasons/block types use an `unknown`
classification rather than copying arbitrary fields into metadata. Complete native
continuation blocks still survive unchanged in conversation history.

`model_response` records tolerant protocol inspection before executable-block
validation. It can therefore precede `request_finished: protocol_decode_failed`:
available IDs, stop reason, usage and block counts survive a rejected reply.
Missing/non-array content omits block counts and text bytes; a malformed text block
omits the byte total rather than treating its unknown length as zero. A real empty
content array still reports zero counts/bytes. Invalid JSON has no `model_response`.
Inspection never authorizes execution; malformed replies still fail without retrying
or executing even otherwise valid actions from the same response.

A successful request means the HTTP/JSON/protocol exchange succeeded, **not** that
the top-level turn has ended or a complete answer was displayed. For example,
`max_tokens` remains visible as response metadata without automatically continuing.

Agent decisions describe the existing loop, not a new completion policy: any tool
results (including errors) lead to another request; no results lead to return, even
for an empty reply or a token-limit stop. `no_tool_results` therefore does not promise
a complete or nonempty answer. Model exchange/decoding errors instead end the agent
turn with `model_failed`, without a fabricated stop decision.

Turn totals count `model_calls` (not HTTP attempts), `assistant_text_blocks`, raw
UTF-8 `assistant_text_bytes` handled by that agent, and `completed_tool_results`
(including errors). Text counts include intermediate/empty/whitespace blocks, exclude
thinking/unknown blocks and child agents' own text, and do not prove UI receipt.
`returned_text_bytes` counts only the final returned string; joining its text blocks
adds newlines, so this can differ from the raw block-byte count. Failure/drop endings
omit this field rather than inventing a successful empty return.

Text identity is `(session_id, turn_id, text_sequence)`; the sequence starts at 1
for each agent turn and continues across its requests. Each text record also carries
its `request_id`. Repeated or empty text blocks retain separate identities. Enqueued
means the channel accepted the event, not that the frontend received it. Receipt is
recorded before UI processing; append is recorded only after sanitized Markdown source
enters the UI's pending transcript state. Later drawing drains that state into retained
history. None of these events proves a successful terminal draw or pixels seen by a user.
Non-whitespace characters include Markdown syntax, not just visible rendered text.

There is one UI span per submitted top-level prompt, sharing the root agent's turn ID.
`ui_text_*` records retain the actual emitting agent's turn/request IDs and add `ui_turn`
with the owning root identity. End totals separate `root` from `other_turns` observed
while that UI span was active (normally delegated children); use parent links for
causal ancestry. Counts include intermediate, empty and whitespace text. An empty final
reply can therefore coexist with earlier UI appends. UI outcomes distinguish
`completed`, `agent_failed`, user `quit`, `frontend_failed`, and `abandoned` cleanup.
Completed means the frontend handled a successful agent return, not that the answer
was nonempty, complete, drawn, or seen. The returned string is not used to repair
missing channel output. Quit/error endings omit returned-text bytes.

Tool execution IDs are unique within a session, independent of provider tool-call
IDs (which may repeat). Known requested tool names are classified as `bash`,
`read_file`, `write_file`, `edit_file`, `load_skill`, `todo`, or `sub_agent`; other
names become `unknown`. Raw names/native IDs/arguments are content, not metadata.

A returned tool result records `succeeded` or `reported_error` from the tool's existing
error flag. Errors include validation, unknown dispatch, process failure/timeout and
child failures; this does not parse free-form output into exit-code/error categories.
`output_bytes` measures the returned UTF-8 string, after any tool formatting/truncation,
not raw process stdout. Error results still enter history and continue the existing loop.

Dropped turn/request/attempt guards record `cancelled` when cleanup runs without an
explicit ending (also possible during panic unwinding). A dropped tool guard records
`abandoned_outcome_unknown`, without output bytes or an invented result payload.
Neither claims rollback or proves an OS process, blocking task, child, or external
side effect stopped. Abrupt process death can leave only start records.

Set `ROBBIE_TRANSCRIPT=1` **before starting Robbie** to opt into sensitive content
capture; other values do not opt in. This adds a startup `session_context` record
(system prompt/tool declarations), the final provider-prepared JSON request body on
`request_started`, and fully received 2xx response bodies on `attempt_finished`.
For non-2xx responses, `attempt_finished` instead adds
`content.kind=http_error_response` with the bounded error-body prefix. Provider
messages and nested upstream details live only in this opted-in body, not default
metadata or the frontend's error message.
It also adds `content.kind=tool_call` (native ID, raw name and arguments) on tool start,
and `content.kind=tool_result` (the returned output) on normal/error tool finish.
These are captured at execution boundaries even if no follow-up request is sent.
`ui_turn_started` adds `content.kind=user_prompt` with the submitted prompt, and
`ui_text_received` adds `content.kind=ui_text` with the exact received assistant text
before terminal-control sanitization. Append/end events contain counts only.
Those bodies can include prompts, assistant text/thinking, tool arguments/results,
and unknown protocol fields. UTF-8 response bodies are JSON strings; invalid UTF-8
is preserved as an array of byte values. Malformed JSON can therefore be inspected
when opted in, even though it is never executed.

This is not a packet trace: authentication/raw headers, request URLs, and environment
dumps are excluded. Error-body reading retains at most **16 KiB** and has a **one-second
budget**, also bounded by the original attempt timeout. `http_error.body_state` is
`complete`, `truncated` (size cap), `read_failed`, or `timed_out`;
`captured_bytes` counts bytes buffered for inspection, not necessarily persisted.
Only opt-in transcripts retain the prefix, including any bytes obtained before a
read failure/timeout. Invalid UTF-8 prefixes use byte arrays as with 2xx bodies.
Treat the prefix as sensitive and check its state before assuming it is complete.

Error-body inspection is best effort and cannot change status-based retryability,
the attempt budget, or execute anything from the response. It can add up to one
second per HTTP error before the existing backoff. `Retry-After` is parsed at header
receipt and rounded up to milliseconds, so inspection never shortens the requested
wait. Partial **2xx** bodies lost to transport failure remain unavailable. Dropping
work during error-body reading still cancels the request/attempt guards; it does not
persist an unfinished prefix or schedule a detached retry.

Records are appended and immediately readable while the file is open. This is not an
`fsync`/power-loss durability guarantee; a failed write can leave a partial last line.
A recorder failure produces a visible warning and disables further recording for that
session without failing or replaying agent/tool work. The frontend checks for warnings
while idle/busy and after agent-turn completion; cleanup-time warnings are printed after
terminal restoration. Error messages in the journal and recorder warnings use typed
categories/fixed text, not raw OS errors or sensitive paths/payloads.

**Retention is manual:** files are kept until you delete them. There is no automatic
pruning, rotation, encryption, or secure-erasure guarantee. Treat opt-in transcripts as
sensitive local data and review them before sharing.

### Manual check

Start with `ROBBIE_TRANSCRIPT=0 cargo run --locked`, find the diagnostics path in the
startup notice, and open it in another terminal/editor (or use `tail -f` on that file).
Before submitting, it contains only `session_started`. Ask: **Reply with exactly hello
and do not use tools.** Expect `agent_turn_started` / `ui_turn_started`, request and
attempt records, then `model_response`, `agent_text_emitted` (`enqueued`), the
`stop_no_tool_results` decision and `agent_turn_finished` (`no_tool_results`).
Each text block should also have matching `ui_text_received` / `ui_text_appended`
records, and the root span ends with `ui_turn_finished` (`completed`). Receipt/append
can interleave with ongoing agent work; do not assume a single ordering across turns.
Check the HTTP status, provider stop reason, agent decision and UI counts; no event
should have a `content` field. A transient failure instead shows attempts/retry
decisions under the same request and turn IDs.

Then ask: **Do not inspect the repository. Run exactly one bash command:
`printf diagnostics-check`. Then reply with its output.** Look for a
`tool_execution_started` / `tool_execution_finished` pair (`tool=bash`,
`outcome=succeeded`) with the same execution, turn and request IDs. The continue
decision and follow-up request stay under that turn ID. A later prompt gets a new one.

To check delegation, ask: **Delegate this exact text-only instruction to sub_agent:
"Reply exactly CHILD_OK without using tools." After it returns, reply PARENT_OK.**
The child's turn should have a `parent` matching the delegating tool execution.
The child turn ends before that execution finishes. Child text keeps its own turn
and request IDs in `ui_text_*`, with `ui_turn` pointing to the parent UI span. Check
`root` and `other_turns` totals in the single `ui_turn_finished` record. Saved text
and completed execution alone do not prove frontend receipt.

An explicitly opted-in second session (`ROBBIE_TRANSCRIPT=1 cargo run --locked`)
adds the context/request/response payloads. If an HTTP error occurs, inspect the
failed attempt's `http_error` and `content.body` for the provider's actual explanation;
check `body_state` for truncation or an incomplete read. This does not recover error
bodies from older sessions. Review transcripts locally and do not share them without
checking for sensitive content.
Exit with Ctrl+C; files remain until manually deleted.

## Prototypes

- [Mascot playground](../prototypes/mascot-playground.html): open the HTML file directly in a browser to explore expressions, palettes, and idle/sleep/wake timing. No server or dependencies; playground changes do not modify the Rust implementation.

## Keyboard shortcuts

- **Ctrl+O** cycles tool-output detail.
- **Ctrl+G** shows or hides the mascot, whether rendered as an image or tiled fallback. Works while idle or busy; hiding it returns its space to the input. The setting lasts for the current session. `ROBBIE_MASCOT=off` disables both renderers and cannot be overridden by the shortcut.
- **Ctrl+C** exits.
