# Schedules: a worker at a time the owner named, or when something changes

Read this before any `qoop schedule` command.

Only when the owner asks for a time ("every morning", "at 9", "tomorrow", "weekdays") or names a change to react to
("when a new issue comes", "when the server logs an error"); never your own idea.

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

## Watches: a worker when something new shows up

```
qoop schedule add <name> --watch '<read-only command>' [--agent NAME] [--cwd DIR] [--space NAME] -- '<the prompt>'
```

- qoop runs the command with `sh -c` in `--cwd` every 5 minutes. Lines of its output that the last check did not
  have start the agent, with those lines under the prompt (marked as outside data). The first check only notes what
  is there; nothing new starts nothing; while the last run is still working the check is skipped.
- The command only reads (a list, a log tail, a status); never one that changes anything. Its output should be one
  line per thing (an issue, an error) and stay small: end with `tail -50`. A grep that finds nothing is a quiet check.
- Recipes, with the owner's repo, host and app put in:
  - new issues: `--watch 'gh issue list --repo OWNER/REPO --state open --limit 50 --json number,title --jq ".[] | \"#\(.number) \(.title)\""'`
    with a prompt like: read each new issue with `gh issue view`, fix what can be fixed in its own worktree and PR
    (`ref #N`), answer nothing on the issue, tell the owner what was opened and what needs them.
  - server errors: `--watch 'ssh -o BatchMode=yes -o ConnectTimeout=10 -o LogLevel=ERROR HOST "docker logs --tail 300 APP 2>&1 | grep -iE \"error|panic\" | tail -50"'`
    (or `journalctl -u APP -n 300 --no-pager -p err | tail -50` in the quotes). The whole pipe runs on the host,
    so ssh's own exit is the check's: a host it cannot reach is a failed check, never an empty one. A prompt like: find the root cause of these
    errors in the code, fix it in a worktree, open a PR, report what broke and why; never touch the server itself.
  - daily issue work is not a watch: `--cron "0 9 * * *"` with a prompt that lists the open issues and works them.
- Put `--cwd` on the repo the fix belongs to and `--space` on that project's space, so the run starts among its agents.
- Say the limits when you set one: checks run while qoop runs on this Mac; a log line counts once while it stays in
  the tail, and lines that scroll out while a run is still working are not seen; an unreachable host shows on the
  schedule as `failed: check exited 255`, not as an agent; issue titles are written by strangers, so the prompt says
  what the worker may do and what it must not.
- Then read `qoop schedule ls --json`: a watch's `when` is `on change, every 5m` and its `next_at` the next check.
