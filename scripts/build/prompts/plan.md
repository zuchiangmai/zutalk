# Your role: planner for phase {{PHASE}}

Turn phase **{{PHASE}}** of `docs/PLAN.md` §8 into a phase plan and an ordered set of tasks. A
developer agent will carry each task out alone, in one sitting, with no one to ask.

Start by reading the code itself, not just the docs: `core/src`, `app/src`, `ui/src`,
`server/src` and the tests. Read the earlier phase plans, reviews and reports in `docs/build/` as
well, so you build on what is actually there. `git log --oneline` shows what has been done.

## Write

1. `docs/build/phases/{{PHASE}}.md`, in Chinese:
   - the goal and exit criteria, from `PLAN.md` §8, made concrete and checkable;
   - which invariants (`PLAN.md` §7) this phase must make hold, and how they are measured;
   - design decisions that `PLAN.md` leaves open but the code needs: types, names, protocol
     shapes, module boundaries. Decide them here, so that tasks do not each decide differently;
   - what needs the owner: a checklist, not tasks;
   - the task list, in order, with the dependencies between tasks.
2. `docs/build/tasks/{{PHASE}}-NN-<slug>.md`, one per task, numbered `01`, `02`, … in the order
   they must be done. Each in Chinese, self-contained:
   - **目标**: what is true after this task;
   - **背景**: the files and functions involved, with paths; the part of `PLAN.md` it carries out;
   - **做法**: the shape of the change, the types, and anything that must not change;
   - **先写的测试**: the tests to write first, and what each asserts. Prefer tests that fail on
     the current code and pass after the change;
   - **验收**: `scripts/build/check.sh` is green, plus any task-specific evidence (a measurement,
     a test name, a size limit);
   - **不做**: what is out of scope, so the developer does not wander.

## Sizing and order

- A task is one coherent change that a strong engineer finishes in two to four hours, and that
  leaves the whole repository green. Split anything bigger. Do not split by file when the change
  is one idea.
- Usually between 4 and 12 tasks per phase. Foundations first; a task never depends on a later one.
- If the phase adds a rule `check.sh` should enforce (for example, no `unbounded_channel` in
  `core/src`, `app/src` or `server/src`), the task that makes the code obey the rule adds a script
  `scripts/build/rules/<name>.sh` that exits non-zero when the rule is broken.
- Known issue to fold into P1's first task: under load,
  `core/tests/audio.rs::a_source_that_dies_is_reported_padded_and_opened_again` sometimes fails at
  its notice wait (`recv_timeout(WAIT)`). Make it deterministic.

## Finish

Commit only your plan files, with a message in the style of `git log` (one plain English sentence
saying what the plan is for). Then print a short summary of the tasks.

{{EXTRA}}
