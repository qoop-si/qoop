# Usage counts

A release build of qoop sends one small set of numbers per day to `https://qoop.si/api/telemetry`:
how many agents and terminals were open, which terminal app and version, how many questions
agents asked you. Never content: no path, name, command, prompt, agent output, host, user,
email or IP. It is on by default, and one command stops it.

## Before anything is counted

The first time you start qoop in a terminal it prints one line and holds it for five seconds:

```
qoop sends daily usage counts (agents, terminals, questions, versions; never content). Stop: qoop telemetry off
```

Nothing is counted before that. An agent in a qoop terminal, a script, or a machine that only
runs the daemon never sees the line, so it never counts.

## See it, stop it

| Command | What it does |
|---|---|
| `qoop telemetry` | whether it is on, and why not when it is not |
| `qoop telemetry show` | every day not yet sent, exactly as it would be sent, with the random id |
| `qoop telemetry off` | stops now, stops a send in flight, deletes every count and the id |
| `qoop telemetry on` | turns it back on and shows the line again; a new id |

The `usage counts` row in `qoop settings` does the same. So does `telemetry = false` at the top
of `config.toml` (anything but `true`, anywhere in the file, keeps it off), or `DO_NOT_TRACK=1`
or `QOOP_TELEMETRY=0` in the environment, which qoop writes into `config.toml` the first time it
sees them. To turn it on again after that, unset the variable, then `qoop telemetry on`.

## What is sent

One JSON per finished UTC day, sent at a random moment within six hours; days that could not be
sent wait, 30 at most.

| Field | What it is |
|---|---|
| `id` | a random id made on this machine, tied to nothing else |
| `day` | the UTC day the numbers are for |
| `v`, `os`, `arch` | qoop's version, `macos`, `linux` or `windows`, `aarch64` or `x86_64` |
| `phone` | whether a phone was connected that day |
| `term` | which terminal app qoop ran in that day: ghostty, iterm, apple_terminal, kitty, wezterm, alacritty, warp, vscode, windows_terminal, konsole, gnome, other |
| `mux` | whether it ran inside tmux, zellij or screen |
| `spaces_max`, `spaces_created` | spaces open at most, spaces you created |
| `terms_open_max`, `terms_open_avg` | terminals open at once, at most and on average |
| `terms_per_space_max` | the most terminals open in one space at once |
| `terms_opened` | terminals you opened |
| `agents_max`, `agents_avg`, `agents_working_max` | agents open at once, on average, and working at the same moment |
| `agents_by_kind_max` | the most of each agent at once: claude, codex, opencode, gemini, cursor, aider, amp, goose, droid, copilot, qwen, other |
| `q_created` | questions created: by an agent waiting on you (`draft`), a question dialog (`dialog`), or qass |
| `q_answered`, `q_dismissed`, `q_closed_qass` | answers that reached the agent, cards you put away, cards qass closed |
| `q_wait_sum_s`, `q_wait_n` | seconds agents waited for your answers, and how many answers |
| `schedule_runs` | schedules and watches that started an agent |

Open counts are read every five minutes, so an agent that lives less than that may not show in
a maximum.

The server keeps each day for 13 months and stores no IP address or location; Cloudflare, in front
of it, sees addresses as it does for every request. To have your days deleted, send the id from
`qoop telemetry show` to hey@qoop.si.
