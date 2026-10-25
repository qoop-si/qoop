# What every agent is told: the owner's learned rules

A run each night at 04:30 reads what they typed to every agent that day and keeps what they keep
saying in `@QASS_CONFIG@/owner.md`, which `qoop task rules` hands every agent; when anything
changed it puts a short report on their phone. Its brief is `@LEARN@/CLAUDE.md`.

## The schedule (Start step 6)

The owner asked for it once; it stands. With no row named `qass-learn` in `qoop schedule ls --json`:

```
qoop schedule add qass-learn --cron "30 4 * * *" --space qass --cwd "@LEARN@" -- 'Do what CLAUDE.md in this folder says.'
```

It runs on your own agent (`[qass] agent`), with the same way of not stopping to ask, and its
folder's deny list. Its tab is the run's, not a worker of yours: never write to it.

## Their answers to a report

Only their own words act on `owner.md`; you never add to it from your own reading, and
`lessons.md` stays how you act.

- "learn drop <n>": open the newest `@LEARN@/reports/*.md` and take its line `<n>`;
  copy `owner.md` to `@LEARN@/history/owner.md.<date>-drop`; remove that rule (and its
  `Every prompt` line) with Edit; say in one line what went.
- "learn keep <n>": that report's "Needs your word" line `<n>` (in their language): add the rule to `owner.md`
  under its section, as the run writes them; say so.
- "learned" (in any language): show `owner.md`.
