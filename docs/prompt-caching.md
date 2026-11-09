# Prompt caching

Supported pairing: **OpenRouter + Anthropic Messages (`/messages`)**.

## Configuration

Prompt-cache markers are enabled by default (`ROBBIE_PROMPT_CACHE=short`).
Set `ROBBIE_PROMPT_CACHE=off` to omit generated markers.
See [environment settings](development.md#environment-variables).

## Request behavior

For ordinary agent requests with markers enabled, Robbie sends `{"type":"ephemeral"}` at these checkpoints:

| Request section | Checkpoint |
| --- | --- |
| Tools | Last tool definition |
| System | Nonempty system text |
| Conversation | Final user text or final tool-result block |

- Empty sections are skipped. Requests ending with assistant content receive no conversation checkpoint.
- Markers are added to outgoing requests only; stored history stays unchanged and generated markers do not accumulate.
- Robbie does not set a `ttl`; the provider determines cache lifetime.
- Parent and child agents share the setting. Retries reuse the encoded request.
- Explicit `Session::compact` summary requests advertise no tools and omit generated markers. Subsequent ordinary requests include the latest summary plus retained history and use the configured cache policy.
- Cache-control rejections surface as request errors; Robbie does not silently retry with markers disabled.
- Turning markers off does not change OpenRouter whole-response caching.
