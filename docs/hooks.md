# Agent hooks

Hook reports are optional. Every agent is already read from its screen, and
that keeps working with no hooks installed: a hook that is missing, broken, or
erroring changes nothing, because every hook script exits zero and fails open.

A hook is worth installing for one reason: latency and certainty. The screen
sees a state after it is drawn; the agent itself knows the moment it changes.
A `blocked` report also carries the visible-blocker flag, which is the one
case where a weaker source may overrule a stronger one under ADR-008.

## Which agents have hooks

| agent    | hook-capable | source                                                                 | script                           |
| -------- | ------------ | ---------------------------------------------------------------------- | -------------------------------- |
| claude   | yes          | https://docs.anthropic.com/en/docs/claude-code/hooks                   | `hooks/claude/qoro-hook.sh`      |
| codex    | yes          | https://developers.openai.com/codex/hooks                              | `hooks/codex/qoro-hook.sh`       |
| gemini   | yes          | https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md | `hooks/gemini/qoro-hook.sh` |
| cursor   | partial      | https://cursor.com/docs/hooks                                          | `hooks/cursor/qoro-hook.sh`      |
| aider    | no           | https://aider.chat/docs/usage/lint-test.html, https://github.com/Aider-AI/aider/issues/2557 | none (screen only) |
| opencode | yes          | opencode plugin events (`session.status`, `permission.asked`, ...)     | `hooks/opencode/qoro-plugin.js`  |
| amp      | yes          | https://ampcode.com/manual/plugin-api                                  | `hooks/amp/qoro-plugin.js`       |
| goose    | yes          | https://goose-docs.ai/docs/guides/context-engineering/hooks/           | `hooks/goose/qoro-hook.sh`       |

Notes:

- cursor: the IDE supports the full hook list; `cursor-agent` on the CLI only
  fires a subset (`sessionStart`, `stop`, `postToolUse` on recent builds).
  Events that never fire never report; the screen covers those.
- aider has no lifecycle hook system, only `--lint-cmd`/`--test-cmd`. A generic
  command-hook feature is an open request (issue #2557). Aider stays on screen
  detection until that ships.
- opencode's event names follow the plugin vocabulary used by its own
  lifecycle; the mapping mirrors the session-status table (`idle` to idle,
  `active`/`busy`/`pending`/`retry`/`running`/`streaming`/`working` to
  working, `permission.asked` to blocked).

## How it works

1. The daemon puts `QORO_TERMINAL` (the terminal's stream number) and
   `QORO_TOKEN` (its capability token) in every terminal child's environment.
   No new variable was added for hooks: the socket is resolved from
   `QORO_RUNTIME_DIR` exactly like any other client.
2. The agent's hook mechanism calls the script in `hooks/<agent>/` on lifecycle
   events. The script maps the event to a state and calls
   `qoro hook-report --state <state>`, which posts one `ReportAgentState`
   control request and waits up to five seconds for the answer.
3. The daemon admits the report through the existing token and generation
   check, assigns its own per-terminal counter as the observation sequence,
   and applies the normal acceptance rule. The hook's wall clock travels as
   `clock` and is advisory only: it is never copied into the observation
   sequence, so one source's nanos can never dwarf another source's counter
   and a later screen blocker always still wins.
4. A report that fails any check gets a `Failed` answer naming the reason
   (unknown state, refused token). The script swallows it: screen only.

`qoro hook-report` usage:

```text
qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown>
    [--terminal N] [--token T]
    [--visible-blocker] [--visible-idle] [--visible-working]
```

`--terminal` and `--token` default to the environment. It prints nothing on
success and the reason on stderr with a non-zero exit on failure.

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
- opencode: load `hooks/opencode/qoro-plugin.js` as a plugin.
- amp: load `hooks/amp/qoro-plugin.js` as a plugin.
- goose: a plugin directory with `hooks/hooks.json` pointing at
  `hooks/goose/qoro-hook.sh` (project `.agents/plugins/` or user
  `~/.agents/plugins/`).

After installing, drive the agent into a question prompt and check the index:
the terminal should read `blocked` while the prompt is up. Uninstall by
removing the entries; the screen reading takes over with no other change.

## Decisions recorded

- Source cap: a per-source sequence map would need a cap.
  qoro keeps no per-source map (each belief holds one sequence number), so
  there is nothing to cap and no cap was added.
- Sequence discipline: per-terminal counter shared with the screen path,
  rather than per-source counters. Cross-source recency is what the ADR-008
  visible-blocker override compares, and per-source counters would make two
  sources' numbers incomparable.
