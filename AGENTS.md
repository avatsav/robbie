# Project context

When starting or resuming work, read `.agents/docs/README.md` if present. It indexes local learning notes, current decisions, and deferred work; follow its topic links as needed. These notes are intentionally gitignored and are absent in a fresh clone.

## Conciseness

- Keep responses and documentation brief, direct, and actionable. Cut filler and repetition.

## Documentation

- Order feature docs by supported scope, defaults/configuration, runtime behavior, then actionable limits. Omit sections that add no useful information.
- Describe implemented behavior verified against code and tests. Keep tutorials, hypothetical cases, and decision histories out of practical docs unless explicitly requested.
- `docs/development.md` contains only concise setup instructions, build/run/check commands, and an environment-variable table (values, defaults, purpose).
- Keep implementation details in code and tests. Link to existing configuration references rather than duplicating their tables.
- Create separate, focused documents only when requested or necessary; keep each concise. Use lowercase, kebab-case filenames.
- When adding a model provider or API protocol, update `docs/prompt-caching.md` with that pairing's behavior; keep provider/protocol scope explicit.
