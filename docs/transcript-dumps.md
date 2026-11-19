# Transcript dumps

`/transcript` (alias `/dump`) exports the **current root model context** as JSON for debugging. It includes system instructions, tool declarations, the retained messages with full tool results, and unchanged assistant blocks—including thinking, signatures and unknown fields—in Anthropic Messages request format.

## Storage

Dumps go only to `<workspace>/.robbie/transcripts/`, where the workspace is the directory Robbie was invoked in (or the explicit core session workspace). Add `/.robbie/` to the project's `.gitignore`; Robbie does not edit it automatically. Dumps are retained until you delete them.

New directories are created with mode `0700`, and uniquely named files with mode `0600`. Existing directory permissions are never changed. Symlinks and non-directories at `.robbie` or `.robbie/transcripts` are refused, even if they point inside the workspace. Directory-relative creation anchors each component to its opened parent; failure never falls back to another directory. Private export currently requires Unix.

## Behavior

The command makes no model request, executes no tools, and does not change history, usage or the journal's capture policy. It works even if journal recording is disabled. Success reports the path with a sensitive-content warning; failure reports an error without ending the session.

The `request` field uses the same encoder as ordinary inference, before provider preparation. It contains no HTTP headers, authentication configuration or request URL. Secrets already present in prompts or tool outputs are **not redacted**. Treat the whole file as sensitive; ignoring it in Git does not encrypt it or prevent a tool from reading it.

## Limits

After compaction, a dump contains the latest summary and retained tail—not retired messages. It excludes independent child-agent histories, earlier HTTP exchanges and the frontend transcript. It is not a session-resumption format or an exact prediction of a subsequent request after new input or automatic compaction.

The session journal still uses its existing external diagnostics storage and opt-in content policy. Other settings/storage have not moved into `.robbie`. This change does **not** confine shell commands, file tools, skill loading or journal writes to the workspace. Whole-agent filesystem confinement, including concurrent workspace-directory replacement, remains separate work.
