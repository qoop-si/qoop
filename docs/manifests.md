# Agent-detection manifests

qoro reads what an agent is doing from its screen (and its OSC title) through
TOML rule files called manifests: one per agent, compiled into the binary,
matched highest-priority-first. This page covers the file format, the bundled
set, and the override directory that lets you fix a misreading without
rebuilding.

## The bundled set

`crates/qoop-detect/manifests/`: `claude`, `codex`, `opencode`, `cursor`,
`gemini`, `amp`, `copilot`, `droid`, `grok`, `qwen`, `devin`, `kiro`, `kimi`,
`kilo`, `hermes`, `cline`, `antigravity` (run as `agy`), `muse`, `pi`, `maki`,
`qodercli`, plus `generic` for everything without its own manifest (a bare
shell prompt reads as idle; anything else reads as nothing).

Every rule is written from the agent's own sources: its open repository where it has
one, otherwise the strings in its published package or binary, otherwise its
documentation. Each file's header names the source and version it was read from, and
each rule says which screen it reads. Where a source gives the words but not where they
are drawn, there is no rule for that screen: `amp` and `antigravity` carry only their
`[resume]` tables, `muse` reads only its startup gates, and `devin` reads no dialog.
Every blocker rule, startup gates included, is tested against the same words quoted in
an answer above the agent's idle prompt: a blocker reads the live bottom of the screen
or refuses while the agent's composer is showing, so a quoted question cannot pin an
idle agent as waiting. Screens copied off real sessions of `claude`, `codex` and
`opencode` are kept as tests of the engine as well.

`aider` and `goose` ship a manifest that carries only a `[resume]` table and no
rules. How to carry a conversation across a restart is a different question
from how to read a screen: the first is answered by an agent's own published
flags, the second needs a capture. A manifest with no rules does not shadow the
generic ones, so they are read exactly as they were before.

`pi`, `hermes`, `kilo`, `muse` and `maki` are also ordinary words and other
programs' names, so a program with one of those names counts as an agent only on
evidence the agent alone leaves: its package (`pi-coding-agent`,
`@kilocode/cli`, `hermes-agent`) or the name as the script a JavaScript or Python
runtime is running. A bare executable called `pi` is somebody else's tool, and the
idle reaper and a restore leave it alone. Muse installs a launcher named `muse` and
nothing else of its own name, so it is not recognised yet.

## The rule schema

```toml
id = "amp"
version = "1"
min_engine_version = 1   # optional; refused by older engines

[[rules]]
id = "esc_cancel_working"
state = "working"        # starting|idle|working|blocked|done|failed|unknown
priority = 100           # highest matching rule wins; ties keep file order
region = "bottom_lines(40)"
visible_working = true
contains = "esc to cancel"
```

- One string per leaf: `contains` (case insensitive), `regex` (whole region),
  `line_regex` (any single line). Several strings are several `any` branches.
- Nestable `all` / `any` / `not` gates; every part present must hold.
- Regions: `whole_screen`, `bottom_lines(N)`, `bottom_non_empty_lines(N)`,
  `top_non_empty_lines(N)`, `after_last_prompt_marker`, `osc_title`, and from
  engine version 2: `whole_recent` (another name for the
  visible screen, the same as `whole_screen`), `prompt_box_body` (between the
  last two horizontal rules, where a composer drawn between rules sits),
  `last_non_empty_above_prompt_box` (the one line with content above that box,
  where a status line sits) and `after_last_horizontal_rule` (a dialog drawn
  under its own rule, without the transcript above it; the whole screen when
  there is no rule). A horizontal rule is a line starting with `─`, alone or
  with a label after at least three of them. There is no `osc_progress`:
  nothing produces OSC 9, and a region that can never match stays a parse
  error until a producer exists.
- Unknown fields are rejected, so a misspelled key fails loudly instead of
  loading a rule that never fires. `updated_at` and `aliases` are not
  accepted: a header holds only `id`, `version`, and optionally
  `min_engine_version`.
- `min_engine_version` is enforced: a file needing a newer engine is refused
  naming itself, rather than misread under older semantics. This engine is
  version 2; a file using a version 2 region declares `min_engine_version = 2`.

## Known behaviour

- A rule with `skip_state_update` that wins (a transcript viewer, a model picker) leaves the last reading in place for as long as it stays on
  screen, with no expiry: closing the pager redraws the live prompt, and the next look
  reads that. A working agent whose pager is left open reads working until it is closed.
- Claude draws every dialog in the composer's place, under one rule of its own, so its
  blockers read below the last rule, or refuse while the composer box is on screen, and an
  answer above the composer never raises one.

## Carrying a conversation across a restart

A manifest may also say how to put a person back into the conversation a tab was having,
which is what a restored tab runs:

```toml
[resume]
# Carry on one exact conversation. `{session}` is the agent's own id for it.
session = ["claude", "--resume", "{session}"]
# Carry on the newest conversation in the tab's directory, for when no id is known.
latest = ["claude", "--continue"]
```

The whole command line is here, flags and all, because it differs per agent and changes
between releases. An agent that renames its flag, or one you run through a wrapper of your
own, is an edit to this file rather than a release to wait for. Both lines are optional:
with neither, a tab running that agent comes back as a plain shell.

Checked when the file is read, not when it is used. A `session` line that never mentions
`{session}` would resume whichever conversation the agent picks rather than the one the tab
was having, and a `latest` line that does mention it would pass the placeholder through as
an argument. Both are parse errors naming the manifest.

Only `claude`, `codex`, `opencode`, `gemini`, `cursor`, `amp`, `droid`, `qwen`, `grok` and
`copilot` carry a `latest` line. Every other agent is resumed only into a conversation whose
id was reported, never into whichever one is newest. Each line comes from the agent's own
flag definitions or documentation, named in the file's header. `aider` has no `session`
line, because it has no session ids at all: its conversation is a file in the project.

Charm Crush has no table and no manifest. Its own complete flag reference has nothing that
resumes a conversation from the command line: switching is done in its own interface. There
is nothing to write down, so nothing is written down.

Nothing else about the tab is re-run. The command a tab was running is recorded and
deliberately not repeated on restore: a tab that was halfway through a deploy, a migration
or an `rm` would run it again on every start.

## Overrides

Put `<agent>.toml` files in `<config>/qoro/agent-detection/` (`$XDG_CONFIG_HOME`
or `~/.config`), one per agent, with the filename matching the `id` inside.
An override replaces the bundled manifest for its id entirely, including ids
no bundled manifest has (a verified `aider.toml` written from a real capture
works the same way). Then tell the running daemon:

```text
qoro reload-manifests
```

It prints `loaded <id>` per applied file and `error <file>: <reason>` per
skipped one. A bad file keeps the previous rules for its agent: the table is
never half loaded, so one typo cannot unload every override. A missing
directory is an empty success.
