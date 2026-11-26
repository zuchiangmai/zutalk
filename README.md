# ZuTalk

Records a conversation, shows live multilingual captions, and leaves an accurate
transcript that belongs to the person who recorded it. An application of the
[Zu protocol](https://github.com/zuchiangmai/zu) for Windows, macOS and Linux.

This repository is the rewrite that replaces the 0.8 line. The whole design is in
[docs/DESIGN.md](docs/DESIGN.md).

```text
zu/      the Zu protocol kernel (shared by every Zu application; not ZuTalk-specific)
core/    everything ZuTalk knows and does; no UI
app/     the Tauri shell
ui/      the interface, and the page a shared link opens
server/  short-lived keys and rooms
```

```bash
cargo test                  # every rule has a test
```

GPL-3.0-or-later.
