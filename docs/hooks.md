# Agent hooks

Hook reports are optional. Every agent is already read from its screen, and
that keeps working with no hooks installed: a hook that is missing, broken, or
erroring changes nothing, because every hook script exits zero and fails open.

A hook is worth installing for one reason: latency and certainty. The screen
sees a state after it is drawn; the agent itself knows the moment it changes.
A `blocked` report also carries the visible-blocker flag, which is the one
case where a weaker source may overrule a stronger one under ADR-008.


## Installing them

```text
qoop hooks status               which are installed, and which agents are not present
qoop hooks install              every agent on this machine it can install for (a config entry, or opencode's plugin file)
qoop hooks install claude       one of them
qoop hooks uninstall claude     exactly what was installed, and nothing else
```

Every daemon also looks every 10 s for an agent on its machine with no qoop hook or skill yet
(one installed after qoop, in `PATH` or a usual install folder) and sets it up
(`crates/qoop-cli/src/keep.rs`). An agent removed with `uninstall` is listed in
`no-auto-hooks` / `no-auto-skills` in the config directory and left alone until it is installed
by name.

amp and goose ship a hook too, but are installed by hand: see "Installing" below.
A hooks directory whose path holds whitespace or shell syntax is refused rather than
quoted, because not every agent runs its hook command through a shell.

The script is written to `<config>/qoop/hooks/`, and one entry is added to that agent's
own configuration file. Three things this will not do to a person's configuration: it adds
beside what is already there rather than replacing it, installing twice changes nothing the
second time, and a file it cannot parse is reported rather than overwritten.

An install that names no agent installs only for the agents on this machine: an agent
counts as present when its binary is on `PATH` or its configuration directory exists, and
the others are listed as `not present, skipped` rather than having a directory created for
them. Naming an agent installs it whether or not it is here yet.

Every write goes to a temporary file beside the configuration and is renamed over it, so an
interrupted write never leaves half a settings file; the temporary file is created with the
configuration's own permissions, so a 0600 file is never briefly readable by others, and a
configuration the install creates gets the mode your umask gives any new file. A
configuration you made read-only is reported as not edited and left alone. A
symlinked configuration is followed, even a link whose target does not exist yet, so the
link stays and the file it points at is changed or created. A copy is
kept before every write, next to the file as `<name>.qoop-backup-<stamp>`; the oldest copy
(the file before qoop first touched it) and the newest three are kept, the rest pruned.

Uninstalling right after installing gives back the file byte for byte in the common cases:
when what is left is exactly what a kept copy holds, that copy's own bytes are written back
rather than a reformatted rendering of the same content (an empty or blank file included),
and a file that did not exist before the install is removed rather than left as `{}` (or,
for Cursor, as `{"version": 1}`). Two cases come back equal in content
but not in bytes: an empty list the person had under the very event the hook registers
(`"SessionStart": []`) is removed with the entry, and a file edited between install and
uninstall is written back in qoop's own formatting. Uninstall looks in every file the agent
could read the entry from, so an entry is found even after the agent's choice of file
changed (Droid's, below).

An agent that is already running does not read its configuration again, so it has to be
started afresh.

A hook is also the only way an agent's own id for its conversation reaches qoop, and that
id is what makes reopening a stopped session come back to that conversation rather than to
whichever one in the directory is newest. Without it a restored tab runs the agent's
continue-the-latest command, which is right until two agents share a repository.

Each hook can also be installed by hand: every file under `hooks/` carries the exact
configuration entry it needs in its own header.

## Which agents have hooks

| agent    | hook-capable | source                                                                 | script                           |
| -------- | ------------ | ---------------------------------------------------------------------- | -------------------------------- |
| claude   | yes          | https://docs.anthropic.com/en/docs/claude-code/hooks                   | `hooks/claude/qoop-hook.sh`      |
| codex    | yes          | https://developers.openai.com/codex/hooks                              | `hooks/codex/qoop-hook.sh`       |
| gemini   | yes          | https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md | `hooks/gemini/qoop-hook.sh` |
| cursor   | partial      | https://cursor.com/docs/hooks                                          | `hooks/cursor/qoop-hook.sh`      |
| aider    | no           | https://aider.chat/docs/usage/lint-test.html, https://github.com/Aider-AI/aider/issues/2557 | none (screen only) |
| opencode | yes          | opencode 2 plugin events (`session.execution.*`, `permission.asked`, ...)| `hooks/opencode/qoop-plugin.js`  |
| amp      | yes          | https://ampcode.com/manual/plugin-api                                  | `hooks/amp/qoop-plugin.js`       |
| goose    | yes          | https://goose-docs.ai/docs/guides/context-engineering/hooks/           | `hooks/goose/qoop-hook.sh`       |
| copilot  | session only | https://docs.github.com/en/copilot/reference/hooks-reference           | `hooks/session/qoop-session-hook.sh` |
| droid    | session only | https://docs.factory.ai/harness/hooks                                  | `hooks/session/qoop-session-hook.sh` |
| qwen     | session only | https://qwenlm.github.io/qwen-code-docs/en/users/features/hooks/       | `hooks/session/qoop-session-hook.sh` |
| qodercli | session only | https://docs.qoder.com/cli/hooks                                       | `hooks/session/qoop-session-hook.sh` |

Notes:

- copilot, droid, qwen, qodercli: one shared script, installed once per agent
  under that agent's name, registered for `SessionStart` only. It reports the
  agent's own conversation id (`session_id` in the payload, `sessionId` for
  Copilot's camelCase events) with `qoop hook-report --session-id` and no
  state, so these agents are resumed by id while their state stays with the
  screen.
- cursor: the IDE supports the full hook list; `cursor-agent` on the CLI only
  fires a subset (`sessionStart`, `stop`, `postToolUse` on recent builds).
  Events that never fire never report; the screen covers those.
- aider has no lifecycle hook system, only `--lint-cmd`/`--test-cmd`. A generic
  command-hook feature is an open request (issue #2557). Aider stays on screen
  detection until that ships.
- opencode (version 2 or later; its plugin shape is `export default { id, setup }`, and the
  version 1 shape is refused at load): `session.execution.started` reports working,
  `succeeded` and `interrupted` idle, `failed` failed, `session.tool.called` a tool event,
  `permission.asked` and `question.asked` blocked, their replies working again. A
  subagent session (one with a parent) never sends its id and never reports idle, because
  it is not the conversation a person resumes. Reports go out one at a time, in event
  order. Only the server a qoop tab starts for itself reports (`opencode --standalone`,
  which qoop uses for qass and for restores): the shared background service keeps the
  environment of the first tab that started it, so a plugin loaded there stays silent. A
  tab started by hand on the shared service has the screen reading and no hook.
- claude's `Notification` is blocked only for the kinds that wait on the
  person: `permission_prompt`, `elicitation_dialog`,
  `elicitation_url_dialog`, `agent_needs_input` and `quota_auto_resume_stale`
  (a resume that waits for Enter), plus a payload with no `notification_type`
  (an older build). Every other kind, `idle_prompt` a minute after the agent
  finished included, reports nothing; `Stop` already said idle. An installed
  hook picks this up on the next `qoop hooks install`.
- a question dialog (`elicitation_dialog` or `elicitation_url_dialog`) is the
  one `blocked` report that carries a label: its type. When it is still the
  agent's newest event 120 seconds later (qass often answers first), the daemon
  drafts a `W<n>` inbox card for it, "a question on its screen", with no
  options, whether or not the agent has an open task; never for a permission
  menu, a quota notice or qass's own tab. His words for it go through qass,
  since a blocked agent takes none, typed as the draft's handle (the agent's
  name word, then its task number, or `-t` and its terminal without a task)
  and his words. The draft closes when the agent reports a tool or a prompt,
  or its task moves. A hook installed before this sends no type, and its
  dialogs get no draft.
- focus events (claude, codex, gemini, opencode): the scripts add `--event
  user|tool|blocked|done` with a one-line `--label` beside the unchanged
  state word. The daemon keeps the last 50 per terminal in memory for the
  focus view: no journal, no push payload, nothing written to the store. Labels are
  best-effort (tool name plus its command, path or pattern, cut to 120
  characters, control characters removed); a payload without the field
  reports label-less. Claude sends the prompt head as the `user` label for
  the focus view; codex and gemini send `user` with no label at all.
- activity: while an agent works, waits or has failed, its row carries
  `activity`, the newest non-empty tool or blocked label (or `thinking`
  after a prompt), which the sidebar shows under the agent and
  `qoop agents --json` prints. The prompt text is never an activity. It is
  cleared when the turn ends. Codex needs `UserPromptSubmit` and gemini
  `BeforeTool` registered, which `qoop hooks install` now does.

## How it works

1. The daemon puts `QOOP_TERMINAL` (the terminal's stream number) and
   `QOOP_TOKEN` (its capability token) in every terminal child's environment.
   No new variable was added for hooks: the socket is resolved from
   `QOOP_RUNTIME_DIR` exactly like any other client.
2. The agent's hook mechanism calls the script in `hooks/<agent>/` on lifecycle
   events. The script maps the event to a state and calls
   `qoop hook-report --state <state> [--event <kind>] [--label ...]`, which posts one `ReportAgentState`
   control request and gives the whole exchange five seconds: connecting, the
   handshake and the answer, so a daemon that is stuck costs a hook five
   seconds and never the agent's turn. Each script
   also passes the agent's own id for the conversation as
   `--session-id`, read out of that agent's hook payload on stdin (claude's
   `session_id`, codex's `session_id`, gemini's `session_id` or
   `GEMINI_SESSION_ID`, cursor's `session_id`/`conversation_id`, goose's
   `session_id`) or out of the plugin event (`data.sessionID` for
   opencode, `thread.id` for amp). A payload with no id reports with no flag,
   same as before.
3. The daemon admits the report through the existing token and generation
   check, assigns its own per-terminal counter as the observation sequence,
   and applies the normal acceptance rule. The hook's wall clock travels as
   `clock` and is advisory only: it is never copied into the observation
   sequence, so one source's nanos can never dwarf another source's counter
   and a later screen blocker always still wins.
4. A report that fails any check gets a `Failed` answer naming the reason
   (unknown state, refused token). The script swallows it: screen only.

`qoop hook-report` usage:

```text
qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown>
    [--terminal N] [--token T] [--session-id S]
    [--event user|tool|blocked|done] [--label ...]
    [--visible-blocker] [--visible-idle] [--visible-working]
qoop hook-report --session-id S [--terminal N] [--token T]
```

`--event` needs `--state`, and `--label` needs `--event`; anything else is
a usage error before anything reaches the daemon. An out-of-set `--event`
is rejected the same way, while the daemon itself stays tolerant: a report
with no (or an unknown) event still applies its state and records no row.

`--terminal` and `--token` default to the environment. It prints nothing on
success and the reason on stderr with a non-zero exit on failure.

A session id may also travel alone, with no `--state`: an agent that knows
its own conversation id but has no lifecycle event to map reports where a
restart should return without claiming anything about what it is doing. The
report goes through the same token check and lands in the same store; the
belief is untouched, so the index never moves on it. This is the path for
third-party agents with no first-party hook: wrap their launch, read
`QOOP_TERMINAL` and `QOOP_TOKEN` from the environment, and report the
conversation id.

## Installing

Each script's header carries the exact config snippet for its agent; the
shape in each case:

- claude: entries under `"hooks"` in `.claude/settings.json` (project) or
  `~/.claude/settings.json` (user), one per event name.
- codex: entries under `"hooks"` in `~/.codex/hooks.json` (user) or
  `.codex/hooks.json` (project, trusted), with the state as the script's
  argument per event.
- gemini: entries under `"hooks"` in `~/.gemini/settings.json` (user) or
  `.gemini/settings.json` (project), with the state as the script's argument.
- cursor: entries in `.cursor/hooks.json` (project) or `~/.cursor/hooks.json`
  (user) with `"version": 1`, state as the script's argument.
- opencode: `qoop hooks install opencode` (run it again after an update: an older file is
  replaced), or copy `hooks/opencode/qoop-plugin.js` into `~/.config/opencode/plugin/`; do not also list it in `opencode.json`, which loads it twice.
- amp: load `hooks/amp/qoop-plugin.js` as a plugin.
- copilot: flat entries (`{ "command": ... }`, no wrapper) under `"hooks"`
  in `~/.copilot/settings.json` (`$COPILOT_HOME/settings.json` when set; a
  leading `~` in any of these variables is the home directory).
- droid: nested entries under `"hooks"` in `~/.factory/settings.json`, or at
  the root of `~/.factory/hooks.json` when that file exists, because Droid
  reads `hooks.json` instead of the settings whenever there is one.
- qwen, qodercli: nested entries with `"matcher": "*"` under `"hooks"` in
  `~/.qwen/settings.json` and `~/.qoder/settings.json` (under `$QWEN_HOME` and
  `$QODER_CONFIG_DIR` when set).
- goose: a plugin directory with `hooks/hooks.json` pointing at
  `hooks/goose/qoop-hook.sh` (project `.agents/plugins/` or user
  `~/.agents/plugins/`).

After installing, drive the agent into a question prompt and check the index:
the terminal should read `blocked` while the prompt is up. Uninstall by
removing the entries; the screen reading takes over with no other change.

## Decisions recorded

- Source cap: a per-source sequence map would need a cap.
  qoop keeps no per-source map (each belief holds one sequence number), so
  there is nothing to cap and no cap was added.
- Sequence discipline: per-terminal counter shared with the screen path,
  rather than per-source counters. Cross-source recency is what the ADR-008
  visible-blocker override compares, and per-source counters would make two
  sources' numbers incomparable.
