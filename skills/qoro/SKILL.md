---
name: qoro
description: "Work with the coding agents running in qoro terminals: see who is there, understand one before writing to it, send it text, wait for it to finish, read its screen and transcript, and start new agents in terminals or git worktrees. Use when the user mentions qoro, tabs, terminals, worktrees, or another agent's session."
license: AGPL-3.0-or-later
metadata:
  author: us
  repository: https://github.com/pat/qoro
allowed-tools: Bash(qoro:*) Read
---

# qoro: agents in terminals

qoro holds shells and coding agents in terminals (tabs). Rule one:
**understand before writing.** Look first (`agents`, `context`),
then write (`send`, `prompt`).

## Who is there

```bash
qoro agents [--json]              # every terminal: number, state, program, session
qoro agents explain <target>      # why one terminal reads the way it does
```

`<target>` is a terminal number, an exact tab name, or an exact program
name. Two matches means `ambiguous`; none means `no such agent or terminal`.

## Understand one

```bash
qoro context <target> [--last N] [--role user|assistant] [--lines N] [--json]
```

One call: who it is, whether it is writable, its live screen and its
recent transcript window. Defaults: last 20 messages, full trimmed screen.

`writable=no` means write to **neither** `send` nor `prompt`:

* `trust_required`: a trust dialog is up; answer it in the terminal first.
* `blocked`: the agent asks a question; do not type over it.

## Write to one

```bash
qoro prompt <target> <text>   # text plus Enter; refused when blocked or trust_required
qoro prompt <target> --wait [--timeout SECS] <text>  # the same, then wait for that turn
qoro send <terminal> <text> [--enter]  # raw typing; enforces nothing, check context first
```

Only `prompt` refuses. `send` checks that the terminal exists and nothing
else. Empty text is refused by both. `prompt` flags go right after the
target; `prompt X "text" --wait` is refused rather than typed.

`prompt --wait` waits for the turn the prompt caused, with `wait`'s exit
codes. An agent that was busy when the prompt arrived must stay stopped for
2 s before it returns, because it may have folded the prompt into its
running turn or queued it for the next one.

```bash
qoro send <terminal> --key esc              # interrupt an agent
qoro send <terminal> --key down --key enter  # pick the next menu option
```

Keys: `enter esc tab shift-tab backspace space up down left right home end
pageup pagedown ctrl-a..ctrl-z`. Text and `--key` do not mix in one call.

`send`, `prompt` and `close` take `--generation G` (from `agents`): refused
when the process in that terminal is no longer the one you read. Pass it
whenever you write based on something you read earlier.

## Read from one

```bash
qoro read <terminal> [--lines N]                          # live screen
qoro history <target> [--last N] [--role user|assistant] [--json]  # transcript window
```

`history` reads the agent product's own store, local files only
(file transcripts tail at 8 MiB; the opencode database reads whole).
`--last` max 200, default 20. `no session id reported` means the agent's
hook never reported its conversation: run `qoro hooks install`.

## Wait instead of polling

```bash
qoro wait [target...] [--until idle,blocked] [--timeout 600] [--json]
qoro wait <target> --match "TEXT" [--until STATE] [--timeout 600]
```

Returns as soon as a target is in one of the states (already there counts),
printing the matching rows. Default states `idle,done,blocked,failed`; no
target means every agent terminal except your own. Exit 124 is a timeout,
exit 1 means a target closed. Wait on the terminals you know are busy, not
on ones already idle. It reads the state when something changes, so a state
that comes and goes in a blink can be missed; the ones that wait on you
(`idle`, `blocked`) last until someone answers. Right after writing to an
agent, use `prompt --wait` instead: a bare `wait` on an agent that has not
started yet returns at once. `--match` waits for text on the live screen
only, not the scrollback; with `--until` both must hold.

## Start and end terminals

```bash
t=$(qoro new --name worker --cwd ~/repo -- claude)   # prints the number
qoro new --space review -- claude                      # a space by id or name
qoro close <terminal>                                  # never your own
qoro worktree new <branch> [--base REF] [-- claude]    # a worktree and a terminal in it
qoro worktree ls [--json]
qoro worktree rm <branch|path> [--force]
```

`new` never starts qoro, and a new `--space` name is made once.
`worktree new` adds `<repo>-wt-<slug>` beside the repository (the slug is
the branch lowercased, other characters as dashes) and opens a terminal
there named after the branch; a branch only on a remote is checked out
tracking it. `worktree rm` refuses while any process is inside it, even
with `--force`, which only overrides a dirty tree; run it from outside the
worktree. The branch is kept.

## Supervising other agents

1. `qoro agents --json`: note each worker's `terminal` and `generation`.
2. `qoro wait <busy terminals> --timeout 600`: sleep until one needs you
   (right after a prompt, `prompt --wait` covers this step).
3. `qoro context <n>`: read what it did or what it asks.
4. Answer: blocked on a menu -> `send <n> --key ... --generation G`;
   finished -> `prompt <n> "next task" --generation G`.
5. Back to 2.

When you supervise for a person:

* Read only until they approve: `agents`, `context`, `history`, `read`,
  `wait`. Report each worker that finished with its answer, and each one
  that is blocked with exactly what it asks and every option it offers.
* Propose the exact command, with `--generation`, and write nothing to a
  worker until the person approves that action.
* Before writing, read that worker again; after writing, confirm with
  `wait` and `context` and report what you saw, not what you expected.
* Never `stop` qoro or `sleep` a space, and `close` a worker only when
  asked. Sit in a space of your own, so putting the workers' to sleep
  cannot end you.
* This skill pre-approves `qoro` for one turn only; a supervisor runs for
  many. Start it with `claude --allowedTools "Bash(qoro *)"` and run each
  `qoro` command on its own, without pipes, so that rule covers it.

## Spaces and sleep

```bash
qoro ls [--json]               # spaces: id, name, awake/asleep/stopped, tabs
qoro sleep <space>             # end its tabs, keep them; refused with reasons
qoro wake [space]              # bring one back, or the one that slept last
qoro stop                      # end qoro; what was awake comes back next open
```

One qoro per machine; spaces are how tabs are grouped. A sleeping space
runs nothing; its agents resume into their own conversations on wake. A
sleep is refused while a tab runs a command, an agent is busy or has not
reported its conversation, or a job runs in the background. `qoro agents`
lists only what is awake, and with nothing awake answers an empty list.
