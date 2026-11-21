# Working in this repository

`docs/DESIGN.md` is the contract: what the app is and its modules.
`docs/LESSONS.md` lists what the previous version learned from real recordings; each item needs a
test here, or a written reason why it no longer applies.

## Principles (these do not bend)

Every change is checked against these first. A change that needs one of them to bend is the
wrong change; stop and ask the owner.

1. **Records belong to the person; the app is a view.** What lasts is the person's signed Zu
   records and their encrypted audio. ZuTalk has no file format of its own and no state worth
   keeping that is not a record.
2. **Two speeds.** Live text is for the eyes: shown, kept as a draft, never the record. The
   record is written once, after the session, from the whole audio and the whole context.
   Nothing on the live path exists to make live text durable or exact (no replay, no
   de-duplication, no cross-stream alignment, no per-line LLM).
3. **One of each.** One fact (a Zu record; a change is one more record at the same path), one
   clock (the capture clock), one container for everything that leaves a device (a room: an
   append-only ciphertext log plus a slot), one recovery (open it again from scratch; the lost
   time is silence or a gap), one state sent whole to the interface. A second mechanism for
   something one of these already does is drift.
4. **Rules are values.** Decisions are pure functions of values with time as a parameter; IO
   lives in thin ports. Differences are data: a language, a model, a guard, an exporter, an
   error class is a row in a table, never a branch in a pipeline.
5. **Failure is a fact.** Unclear is said, not guessed; a gap is marked, not hidden; an error is
   a stable code the interface has words for. Nothing is swallowed.
6. **Privacy by construction.** Audio leaves only through `tape::Pass`. The server sees
   ciphertext, quota and keys, never a word of a conversation. Keys never reach logs, `Debug`
   or the interface. On disk they live in one place: the system key store, or, on macOS while
   releases are ad-hoc signed, one secrets file only the person's account can read.
7. **Small because the structure is right.** Few concepts, each with one home; layers depend one
   way. When a module grows, first ask which principle it is working around.

## Rules

- **No line caps.** The owner removed the per-module line budget on 2026-11-21 (`budget.toml`
  and `scripts/budget.py` are gone). Principle 7 still holds: a module grows only for what it
  must do, and no rule is dropped to keep it short.
- **Tests live in `tests/` directories**, through the public API. Nothing under `src/` is test code.
- **Rules do no IO.** Segmentation, clocks, policies and protocol state are functions of values:
  time is a parameter, never read. IO stays in the few modules whose job it is (`tape`, `zu::pack`,
  network ports).
- **Differences are data.** A language, an exporter, an error class is a row in a table, not a
  branch in the pipeline.
- **Failure is a fact.** No panics on input, no swallowed errors; say what happened with a stable code.
- **Audio never leaves the device** except through `tape::Pass`. Do not add another way to read it.
- **Privacy.** No person's name, handle or email in code, comments, test data, docs or commits.
  Secrets never appear in `Debug`, logs or any file but the one secrets file.
- Comments in English, short, about why. Docs under `docs/` in Simplified Chinese.
- Format with the repository's `rustfmt.toml`; `cargo clippy --all-targets -- -D warnings` is clean.
- Commit only when the owner asks.

## The old repository

The 0.8 app is in the sibling `zulangue` repository. It is the source of hard-won rules and of
recorded test material, not of structure: read it to learn what must hold, then write it small.
