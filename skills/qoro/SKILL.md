---
name: qoro
description: "Work with the coding agents running in qoro terminals: see who is there, understand one before writing to it, send it text, read its screen and transcript. Use when the user mentions qoro, tabs, terminals, or another agent's session."
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
qoro send <terminal> <text> [--enter]  # raw typing; enforces nothing, check context first
```

Only `prompt` refuses. `send` checks that the terminal exists and nothing
else. Empty text is refused by both.

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
```

Returns as soon as a target is in one of the states (already there counts),
printing the matching rows. Default states `idle,done,blocked,failed`; no
target means every agent terminal except your own. Exit 124 is a timeout,
exit 1 means a target closed. Wait on the terminals you know are busy, not
on ones already idle. It reads the state when something changes, so a state
that comes and goes in a blink can be missed; the ones that wait on you
(`idle`, `blocked`) last until someone answers.

## Start and end terminals

```bash
t=$(qoro new --name worker --cwd ~/repo -- claude)   # prints the number
qoro close <terminal>                                  # never your own
```

## Supervising other agents

1. `qoro agents --json`: note each worker's `terminal` and `generation`.
2. `qoro wait <busy terminals> --timeout 600`: sleep until one needs you.
3. `qoro context <n>`: read what it did or what it asks.
4. Answer: blocked on a menu -> `send <n> --key ... --generation G`;
   finished -> `prompt <n> "next task" --generation G`.
5. Back to 2.

## Sessions

```bash
qoro --session work            # which session to work in (or QORO_SESSION)
qoro ls                        # sessions and their tab counts
qoro stop [name]               # end a session; tabs come back on next open
```

Sessions do not see each other. A shell inside a session reaches its own
daemon, so plain `qoro ...` targets the right place.
