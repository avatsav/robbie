# Context compaction

Compaction is in-memory and supports **OpenRouter + Anthropic Messages**. The CLI enables automatic checks for parent and child conversations independently. The core also exposes explicit `Session::compact(options)`. There is no terminal compaction command, persistence/resumption or context-overflow retry.

## Settings

See [environment settings](development.md#environment-variables) for automatic-compaction opt-out and an explicit context-window override.

At startup the CLI resolves and caches limits for the session. Unrestricted routing uses OpenRouter's key-scoped model metadata, falling back to public model metadata if unavailable. Pinned routing uses the smallest context capacity among matching endpoint tags; missing routes or capacities remain unknown. Metadata GETs have four-second request timeouts, an eight-second overall budget and an 8 MiB body cap. Unknown limits produce a warning and disable automatic checks, not a guessed window. An explicit override skips discovery; it does not enlarge the server's actual capacity.

Core callers opt in with `resolve_model_limits` followed by `configure_auto_compaction`. Limits and policy are configured once and shared with children; conversations and usage baselines are not shared.

Defaults:

- Trigger reserve: **16,384 tokens**.
- Recent-history target: **20,000 estimated tokens**; complete tool batches may exceed it.
- Desired ordinary and summary output caps: **8,000 tokens**, reduced to known model limits. Summary output and recent retention are further bounded to leave input/output room.
- Safety headroom: **4,096 tokens**.

Invalid or impossible enabled budgets fail configuration. Small windows may require a smaller core policy or CLI opt-out.

## Runtime behavior

Before each ordinary request, including a new prompt or a completed tool-result batch, Robbie estimates the next input. It uses the latest usable ordinary-response input plus output and estimates appended messages. Without a valid baseline, or after compaction/prompt changes, it estimates the encoded system, tools, latest summary and raw history. Estimates use serialized Unicode characters / 4, rounded up; this is not exact tokenization.

Compaction triggers above `window - reserve`, with reserve raised to at least ordinary output plus safety headroom. A separate fit check includes output, safety and any maximum-input limit. Summary requests get their own encoded-input/output preflight; they advertise no tools and omit generated prompt-cache markers.

An accepted summary replaces an older prefix; the retained tail, including opaque assistant continuation and tool IDs/results, stays exact. Later requests prepend the latest summary once. The frontend transcript and cumulative usage totals are not reset. Summary usage never becomes a conversation's occupancy baseline.

Each preflight attempts compaction at most once, then re-estimates. A failed or useless attempt is suppressed for the rest of that turn. If the request still fits the estimated budget, a warning accompanies continuation; otherwise it stops before ordinary inference. A new user turn permits another attempt. Tools are not replayed and incomplete exchanges are not repaired. Dropping a pending operation preserves its input context, not upstream work/billing or completed tool side effects.

Compaction lifecycle records have correlated operation/request/turn IDs and distinguish commit, unchanged, failure and cancellation. Recording failure is nonfatal; content remains subject to the existing transcript opt-in.

## Display and limits

The footer shows session input/output/cache-read/cache-write totals at left and the model at right, below the directory. Child and summary requests count toward totals, not the parent's context estimate. `?` means unavailable, `≥` marks incomplete reported totals, and `~` marks estimated context percentages. `(auto)` appears only when automatic checks are configured with a known window.

Metadata can be stale or aggregate rather than an exact routed-server limit. Usage can be missing, and character estimates can undercount or overcount. None of these checks guarantees provider acceptance or summary fidelity.

A huge first prompt or protected recent exchange may not fit and cannot be shrunk by prefix compaction. There is no recursive summarization, destructive output pruning or hidden recovery loop. With no CLI history-editing command, restart with less input if automatic compaction cannot free enough space; core callers may explicitly compact with a smaller recent target.
