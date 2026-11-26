# Your role: revise the plan for phase {{PHASE}}

On 2026-11-21 the owner added a standard: **the code must be explicit and observable.** It is now
principle 10 in `CLAUDE.md` and `docs/PLAN.md` §4.5. Read both closely. The new core has no
logging at all today. 0.8's failures were only ever understood through its `rust.log` and
main-thread samples.

Phase {{PHASE}} is planned in `docs/build/phases/{{PHASE}}.md`, with tasks in
`docs/build/tasks/{{PHASE}}-*.md`. Tasks that already have a report starting with `状态：完成` in
`docs/build/reports/` are done. Do not change them or their numbers. Read their reports, and
the code as it now is.

## Do

1. Add to the phase plan a design section for the observability foundation, decided once for all
   tasks:
   - the `tracing` setup: the JSON-lines file under `<data>/logs/`, rotation, who installs it
     (the shell and the server; tests install a capturing one);
   - the event naming scheme, and the allowed field types (codes, ids, numbers, booleans), and
     how that is enforced;
   - `rt::stats`: counters, high-water marks, fixed-bucket latency histograms; the once-a-minute
     summary; the `Stats` signal;
   - the `stall.snapshot` event;
   - `Command::Diagnose` and what goes in the bundle;
   - the shell's non-blocking main-thread heartbeat;
   - the interface's frame-gap report;
   - the named-queue constructor `rt::queue::bounded(name, capacity, Full::…)`;
   - task naming through the supervision tree;
   - the rule scripts `numbers.sh`, `spawn.sh` and `dropped.sh` (and how `bounded.sh` changes);
   - how the bench captures events, so tests can assert on them;
   - the privacy test: no word of the bench's speech ever appears in the log.
2. Insert a task for the foundation, as early as the order allows. It should come right after
   the layout task, before the bench, so the bench can assert on events from the start.
   Renumber the tasks that are not done, so that file names still sort in execution order.
   Two-digit numbers; no gaps needed. Fix every cross-reference: in the plan, in the tasks, and
   in `scripts/build/rules/` mentions.
3. Revise every task that is not done:
   - add which events it must emit, and which stats it must count;
   - make each fault test also assert visibility (the event and the verdict);
   - name its numbers; state its queues;
   - spawn only through the tree.
   Split a task if it grows past what one sitting can do.
4. Update the exit criteria and the invariant table: add one for "explicit and observable", with
   how it is checked.
5. If the rule scripts would fail on code that later tasks fix, say in the task that adds each
   rule which code it brings into line.

Commit the revised plan and tasks in one commit, with a message in the style of `git log`. Then
print the new task list.
