---
name: qoop
description: "Work with the coding agents running in qoop terminals: see who is there, understand one before writing to it, send it text, wait for it to finish, read its screen and transcript, and start new agents in terminals or git worktrees. Use when the user mentions qoop, tabs, terminals, worktrees, or another agent's session."
license: AGPL-3.0-or-later
metadata:
  author: qoopsy
  repository: https://github.com/qoop-si/qoop
allowed-tools: Bash(qoop:*) Read
---

# qoop: agents in terminals

qoop holds shells and coding agents in terminals (tabs). Rule one:
**understand before writing.** Look first (`agents`, `context`),
then write (`send`, `prompt`).

## Who is there

```bash
qoop agents [--json]              # every terminal: number, state, program, session
qoop agents explain <target>      # why one terminal reads the way it does
```

`<target>` is a terminal number, an exact tab name, or an exact program
name. Two matches means `ambiguous`; none means `no such agent or terminal`.

## Understand one

```bash
qoop context <target> [--last N] [--role user|assistant] [--lines N] [--json]
```

One call: who it is, whether it is writable, its live screen and its
recent transcript window. Defaults: last 20 messages, full trimmed screen.

`writable=no` means write to **neither** `send` nor `prompt`:

* `trust_required`: a trust dialog is up; answer it in the terminal first.
* `blocked`: the agent asks a question; do not type over it.

## Write to one

```bash
qoop prompt <target> <text>   # text plus Enter; refused when blocked or trust_required
qoop prompt <target> --wait [--timeout SECS] <text>  # the same, then wait for that turn
qoop send <terminal> <text> [--enter]  # raw typing; enforces nothing, check context first
```

Only `prompt` refuses; `send` checks only that the terminal exists. Both
refuse empty text. Put `prompt` flags right after the target
(`prompt X "text" --wait` is refused, not typed).

`prompt --wait` waits for the turn the prompt caused, with `wait`'s exit
codes. An agent busy when it arrived must stay stopped for 2 s first: it
may have folded the prompt into its turn or queued it.

```bash
qoop send <terminal> --key esc              # interrupt an agent
qoop send <terminal> --key down --key enter  # pick the next menu option
```

Keys: `enter esc tab shift-tab backspace space up down left right home end
pageup pagedown ctrl-a..ctrl-z`. Text and `--key` do not mix in one call.

`send`, `prompt` and `close` take `--generation G` (from `agents`): refused
when the process in that terminal is no longer the one you read. Pass it
whenever you write based on something you read earlier.

## Read from one

```bash
qoop read <terminal> [--lines N]                          # live screen
qoop history <target> [--last N] [--role user|assistant] [--json]  # transcript window
```

`history` reads the agent product's own store, local files only
(file transcripts tail at 8 MiB; the opencode database reads whole).
`--last` max 200, default 20. `no session id reported` means the agent's
hook never reported its conversation: run `qoop hooks install`.

## Wait instead of polling

```bash
qoop wait [target...] [--until idle,blocked] [--timeout 600] [--json]
qoop wait <target> --match "TEXT" [--until STATE] [--timeout 600]
```

Returns once a target is in one of the states (already there counts) and
prints the matching rows. Default states `idle,done,blocked,failed`; no
target means every agent terminal but yours. Exit 124: timeout; exit 1: a
target closed. Wait on busy terminals, not idle ones. A state that comes
and goes in a blink can be missed; `idle` and `blocked` last until someone
answers. Right after writing, use `prompt --wait`: a bare `wait` on an
agent that has not started returns at once. `--match` reads the live
screen only, not the scrollback; with `--until` both must hold.

## Start and end terminals

```bash
t=$(qoop new --name worker --cwd ~/repo -- claude)   # prints the number
qoop new --space review -- claude                      # a space by id or name
qoop close <terminal>                                  # never your own
qoop worktree new <branch> [--base REF] [-- claude]    # a worktree and a terminal in it
qoop worktree ls [--json]
qoop worktree rm <branch|path> [--force]
```

`new` never starts qoop; a new `--space` name is made once. `worktree new`
adds `<repo>-wt-<slug>` beside the repository (the branch lowercased, other
characters as dashes) with a terminal named after the branch; a branch only
on a remote is checked out tracking it. `worktree rm` refuses while any
process is inside it (`--force` only overrides a dirty tree); run it from
outside. The branch is kept.

## Supervising other agents

1. `qoop agents --json`: note each worker's `terminal` and `generation`.
2. `qoop wait <busy terminals> --timeout 600`: sleep until one needs you
   (right after a prompt, `prompt --wait` covers this step).
3. `qoop context <n>`: read what it did or what it asks.
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
* Never `stop` qoop or `sleep` a space, and `close` a worker only when
  asked. Sit in a space of your own, so putting the workers' to sleep
  cannot end you.
* This skill pre-approves `qoop` for one turn only; a supervisor runs for
  many. Start it with `claude --allowedTools "Bash(qoop *)"` and run each
  `qoop` command on its own, without pipes, so that rule covers it.

## Spaces and sleep

```bash
qoop ls [--json]               # spaces: id, name, state, tabs, which sleep
qoop sleep --tab <terminal>    # end one tab, keep it; refused with reasons
qoop sleep <space>             # every tab of a space that can sleep
qoop wake --tab <terminal>     # bring one tab back; `wake <space>` all of it
qoop wake                      # the tab that slept last
qoop space rm <id|name>        # drop a space with no tabs; refused if it has any
qoop restart                   # daemon onto the qoop on disk, every tab still running
qoop stop                      # end qoop; what was awake comes back next open
```

One qoop per machine; spaces group tabs. A sleeping tab runs nothing; on
wake its agent resumes its own conversation. A sleep is refused while a tab
runs a command, an agent is busy or has not reported its conversation, or a
job runs in the background. `qoop agents` lists only what is awake (an
empty list when nothing is).

## Testing what you build

Run your own browser and simulator tests headless and read the screenshots yourself (headless
Chrome or Playwright `headless: true`; `xcrun simctl boot` and `xcrun simctl io <udid>
screenshot`, never `open -a Simulator`; Android `-no-window` and `adb exec-out screencap`). Open
a window only when the owner asks to see something, and close every browser, simulator and
emulator you started.

## Show the result

When the work ends with something to look at, put it on the owner's phone before you say it is
done: `qoop show <file> --note "<what it shows>"`. The phone lists it and opens it in its own
viewer; `qoop show <url>` does the same for a public url (a deploy, a PR). Write captures to
`"${TMPDIR:-/tmp}"`, never into the repo:

- the screen: `screencapture -x "${TMPDIR:-/tmp}/shot.png"`
- a clip of 15 s or less: `screencapture -v -V 15 "${TMPDIR:-/tmp}/clip.mov"`, then shrink it with
  `avconvert --preset Preset1280x720 --source "${TMPDIR:-/tmp}/clip.mov" --output "${TMPDIR:-/tmp}/small.mov"`
- a simulator: `xcrun simctl io booted screenshot "${TMPDIR:-/tmp}/sim.png"` or
  `xcrun simctl io booted recordVideo "${TMPDIR:-/tmp}/sim.mov"`
- a browser: a Playwright screenshot or video
- a long report: write it to a file and show the file

A file is at most 25 MiB, and a url is a public one: the phone cannot open `localhost`. The
path is read again when the owner opens it, so a file changed or deleted since must be shown again.

## Tasks

```bash
qoop task new [--replace] "<outcome>"  # open this terminal's task (one at a time)
qoop task plan -                       # stdin: acceptance, plan, how it is verified, risks
qoop task stage <stage>                # investigating planning implementing reviewing verifying waiting-human
qoop task evidence "<check: result>"   # a check that proves an acceptance point
qoop task done "<summary>"             # a claim; checked outside the agent
qoop task close <id> --verified|--failed [reason]   # qass or the person ends it
qoop task ls [--json]                  # open tasks, every terminal
qoop task show [id] [--before E] [--json]           # one task and what was recorded
```

The record lives in qoop, not the repo, and outlives a crash, a restart or
a compaction. An agent inside qoop gets the short rules at session start
(`qoop task rules`). `qoop agents --json` rows carry `task: {id, goal,
stage, claimed}`. A task named by id needs no terminal; without one the
command speaks for its own terminal (`QOOP_TERMINAL`, `QOOP_TOKEN`). What
an agent wrote there is its own account: check it before acting on it.
