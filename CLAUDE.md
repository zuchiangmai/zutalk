# Working in this repository

`docs/DESIGN.md` is the contract: what the app is and its modules. `docs/PLAN.md` is the
architecture and roadmap adopted on 2026-11-21; where the two differ, `PLAN.md` wins until it is
folded into `DESIGN.md`. `docs/build/` is how the build runs, and `docs/build/DECISIONS.md` holds
the provisional answers to the owner's open questions.
`docs/LESSONS.md` lists what the previous version learned from real recordings; each item needs a
test here, or a written reason why it no longer applies.

## Principles (these do not bend)

Every change is checked against these first. A change that needs one of them to bend is the
wrong change; stop and ask the owner.

1. **Records belong to the person; the app is a view.** What lasts is the person's signed Zu
   records and their encrypted audio. ZuTalk has no file format of its own and no state worth
   keeping that is not a record.
2. **Every path has its own time budget.** Live text is for the eyes: shown, kept as a draft,
   never the record. The record is written once, after the session, from the whole audio and
   the whole context. Nothing on the live path exists to make live text durable or exact (no
   replay, no de-duplication, no cross-stream alignment, no per-line LLM). Capture, live,
   record, query, background and interface each have a budget, and a faster path never waits
   on a slower one.
3. **One of each.** One fact (a Zu record; a change is one more record at the same path), one
   clock (the capture clock), one container for everything that leaves a device (a room: an
   append-only ciphertext log plus a slot), one recovery (open it again from scratch; the lost
   time is silence or a gap). Toward the interface there are three kinds of data, each with one
   way across: a **bounded** state sent whole; **signals** (live captions, progress), of which
   only the latest is kept; and **views** (library, transcript, search), computed from the
   records, fetched by range and invalidated by version. Whatever grows with recordings is a
   view, never part of the state. A second mechanism for something one of these already does
   is drift.
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
8. **Liveness is a fact.** Every path has a budget, every queue a bound, every part reports how
   far it has got. A stall is seen and acted on, never waited out.
9. **Cost does not grow with history.** What any one update costs depends on what changed and
   on the size of the screen, never on how long the recording runs or how much is kept.
10. **Explicit and observable.** Reading the code tells what will happen: states are enums with
   transition functions, numbers are named with their unit and their reason, every queue says
   its bound and what happens when full, every task has a name and an owner, nothing is dropped
   without saying why. Running code tells what it is doing: every state change, restart, full
   queue, verdict, command and job is a structured event of codes, ids and numbers (never a word
   of a conversation, a name or a key); counters, high-water marks and latencies are readable at
   any moment; a stall leaves a snapshot. A fault a test injects must be both recovered from
   and visible. (`docs/PLAN.md` §4.5.)

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
- **No unbounded queue.** Every channel has a capacity and says what happens when it is full
  (drop, coalesce, or end with a code). `unbounded_channel` is not used.
- **The shell never blocks.** No synchronous Tauri command; no lock held across an `await` or a
  window call; the main thread is never waited on.
- **The hub and the live path do no IO.** Disk and network belong to `capture`, `record`, `view`,
  `out` and `id` ports, run off the hub.
- **Lists are virtualized.** Nothing in the interface does work in proportion to a recording's
  length or the library's size.
- **Every long-running task is supervised and reports a watermark.** Tasks are started only
  through the supervision tree, with a name.
- **Named numbers.** A timeout, capacity, budget or retry count is a named constant with its unit
  in the name (`_MS`, `_BLOCKS`, `_BYTES`) and a comment saying why it is that number.
- **Nothing dropped silently.** A discarded `Result` or failed send carries `// dropped: <why>`
  on the same line; if it matters, it is a failure with a code.
- **Events, not prints.** Diagnostics go through `tracing` as structured events (`event` code,
  ids, numbers) into `logs/zutalk.jsonl`; no `println!`/`eprintln!` outside `main` before the log
  exists. The fields of an event are codes, ids, numbers and booleans only.
- Commit only when the owner asks. The owner asked on 2026-11-21 for the whole build: during it,
  each finished task is committed locally on `main`; nothing is pushed, tagged or released.

## The old repository

The 0.8 app is in the sibling `zulangue` repository. It is the source of hard-won rules and of
recorded test material, not of structure: read it to learn what must hold, then write it small.
