# Schedules: a worker at a time the owner named

Read this before any `qoop schedule` command.

Only when the owner asks for a time ("every morning", "at 9", "tomorrow", "weekdays"); never your own idea.

```
qoop schedule add <name> (--cron "<m h dom mon dow>" | --at <HH:MM | YYYY-MM-DD HH:MM | +30m>) \
  [--agent NAME] [--cwd DIR] [--space NAME] -- '<the prompt>'
```

- Their words become the time, in this Mac's local time: a repeat is `--cron` (weekdays 9:00 is
  `0 9 * * 1-5`, every 2 hours `0 */2 * * *`), a single run is `--at` (a bare `HH:MM` already past
  means tomorrow). `--agent`, `--cwd` and `--space` as for Spawning; `--cwd` the repo they mean.
- The prompt is a full brief for a worker no one watches: their words, the repo, what done is, what
  to leave alone. Apostrophes as ’, since `'` is the quote.
- Then read `qoop schedule ls --json` and tell them the name, its `when` and its next run (`next_at`).
- Pause, resume, remove or run now, on their request only: `qoop schedule pause|resume|rm|run <id|name>`.
- A terminal is a scheduled run when its number is some row's `last_terminal` in `qoop schedule ls
  --json` (its tab is named `<name> HH:MM`). It is the owner's worker, not yours: no task record, no
  `goals.md` line, no count against the cap of Spawning. Never write to it unless they ask; a
  question from it is a card like any other.
- A remote: `"<this skill's dir>/on.sh" ubox schedule add ...` sets it on that machine's clock.
- A schedule fires only while qoop runs on that Mac. A missed run gets one catch-up when qoop is
  back: within 7 days for a repeat, within 24 hours for a single run.
