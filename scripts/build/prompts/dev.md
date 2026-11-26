# Your role: developer for task {{TASK}}

Carry out the task in `docs/build/tasks/{{TASK}}.md`. Before you start, read the phase plan
`docs/build/phases/{{PHASE}}.md` and the reports of the tasks before this one, in
`docs/build/reports/`.

## How

1. Read the code you will change, and the tests around it.
2. Write the tests the task asks for first. Where the task says so, see them fail.
3. Make the change. Keep to the task's scope, and to `CLAUDE.md`. Something you see that is wrong
   but out of scope goes in your report, not in the code.
4. Run `scripts/build/check.sh` until it is green. If something that has nothing to do with your
   change fails, fix it only if the fix is small and plainly right; otherwise say so in your report.
5. If the change alters what `docs/DESIGN.md` or `docs/PLAN.md` describes, update the doc in the
   same change, in Chinese.
6. Write `docs/build/reports/{{TASK}}.md` in Chinese:
   - the first line is exactly `状态：完成` or `状态：受阻`;
   - what changed, and the files;
   - the evidence: the new tests by name, and any measurements;
   - what is left, or needs the owner;
   - for `受阻`: what blocks it, and what you tried.
7. Commit everything for this task in one commit. The message is one plain English sentence, in
   the style of `git log`, saying what changed and why.

If the task cannot be done as written (it rests on something false, or it would break a
principle), do the closest thing that keeps the principles, and explain why in the report. If
nothing sensible can be done, report `状态：受阻`, commit the report, and stop.

{{EXTRA}}
