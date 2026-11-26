# ZuTalk 0.8.7

Prepares your local recording library for a seamless upgrade to ZuTalk 0.9.0
without modifying or deleting any existing recordings.

ZuTalk requires macOS 12.5 or later.

## Library handover for ZuTalk 0.9.0

- **Automatic, non-destructive handover.** On launch and again right before an
  update installs, ZuTalk exports a read-only handover snapshot of your live
  transcripts (including manual edits), whole-session transcripts, speaker
  names, session notes, mark notes, and sealed audio references into
  `handover/` alongside your existing library. Your 0.8 library files are never
  altered.
- **Safe staged upgrade to 0.9.0.** ZuTalk 0.9.0 is offered through the updater
  only after the local handover snapshot has been written, so your recordings
  are ready to appear immediately on first launch of 0.9.0 — even without
  signing in.
