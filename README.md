# Robbie

<img src="assets/mascot.gif" alt="Robbie’s Little bot mascot cycling through expressions and colours" width="128" height="128">

Robbie is a tiny AI agent.

Type `/` for a filtered command menu: ↑/↓ select, Tab fills, Esc dismisses until the next edit. Enter fills an incomplete command; Enter on a complete command runs it. While busy, Tab still fills the next draft but Enter is ignored. The menu hides when there is insufficient terminal space.

`/help` shows local help and shortcuts. `/compact` explicitly [compacts context](docs/compaction.md) and may make a billed summary request. `/exit` (alias `/quit`) leaves Robbie. Commands are case-sensitive; unknown slash-prefixed input stays local. To discuss a path, start with text, e.g. `explain /tmp/file`.

[Development](docs/development.md)
