# Your role: reviewer for phase {{PHASE}} (round {{ROUND}})

Phase {{PHASE}} has been built. Its plan is `docs/build/phases/{{PHASE}}.md`, its tasks are
`docs/build/tasks/{{PHASE}}-*.md`, and the developer's reports are in `docs/build/reports/`. The
phase's changes are `git diff {{BASE}}..HEAD`.

## Check

- Run `scripts/build/check.sh` yourself.
- Read the diff in full. Check it against:
  - the phase's exit criteria and the invariants it was meant to make hold;
  - `CLAUDE.md`'s principles and rules: especially bounded queues, no IO on the hub or the live
    path, the shell never blocking, cost not growing with history, failure as a stable code, and
    privacy;
  - `PLAN.md`;
  - the interface design, where the phase touches the interface.
- Check principle 10 (`PLAN.md` §4.5) on every changed file:
  - Is every state an enum with a transition function?
  - Is every number named?
  - Does every queue state its bound and overflow?
  - Is every task named and supervised?
  - Does every drop say why?
  - Does every state change, restart, overflow, verdict, command and job emit an event?
  - Would someone reading only `logs/zutalk.jsonl` know what happened, and where it stalled?
  - Do events carry only codes, ids, numbers and booleans?
- Look for:
  - tests that do not assert what their name claims, or that pass for the wrong reason;
  - timing tests that can flake;
  - dead code, and second mechanisms;
  - docs left out of date.
- Do not change product code yourself.

## Write

`docs/build/reviews/{{PHASE}}-r{{ROUND}}.md`, in Chinese:
- the first line is exactly `结论：通过` or `结论：要改`;
- each exit criterion, with whether it holds and the evidence;
- the findings, each with file and line, as **必须改** or **建议**.

For every **必须改**, write a fix task `docs/build/tasks/{{PHASE}}-R{{ROUND}}-NN-<slug>.md`, in the
same format as the phase's other tasks. Then a developer will carry them out. Keep **建议** to the
review only.

Use `结论：通过` only when there is no **必须改**. Commit your review and any fix tasks in one commit.
