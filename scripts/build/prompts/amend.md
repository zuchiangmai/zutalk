# Your role: amend the plan for phase {{PHASE}}

The owner has made a decision that phase {{PHASE}} must take in:

{{NOTE}}

The phase is planned in `docs/build/phases/{{PHASE}}.md`, with tasks in
`docs/build/tasks/{{PHASE}}-*.md`. Tasks whose report in `docs/build/reports/` starts with
`状态：完成` are done: do not change them or their numbers. Read the code as it now is.

## Do

1. Add to the phase plan the design this decision needs, decided once: types, names, the
   protocol, the events (principle 10), and what is left to later phases.
2. Add the task or tasks that carry it out. Place them where the order allows: after what they
   depend on, and before the phase's closing task. Renumber the tasks that are not done, so that
   file names still sort in execution order, and fix every cross-reference.
3. Adjust any not-done task the decision touches. Update the exit criteria.

Commit the amended plan and tasks in one commit, with a message in the style of `git log`. Then
print the new task list.
