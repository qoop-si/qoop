# Supported agents

qoop recognises an agent by what is running in the terminal, even when it runs through a
runtime (`node`, `bun`, `python`): an agent installed as a node package reads as itself, not
as `node`. It then reads what the agent is doing from its screen, and from its hook when one
is installed.

States: `starting`, `working`, `blocked` (waiting for you), `done`, `failed`, `idle`,
`uncertain` (two sources disagree, which never pages you) and `sleeping` (put to sleep by
hand or after it sat idle, conversation kept; see [Sleep](#sleep)).

## Detected agents

Every agent's rules are written from its own sources, and the table says which. Where those
sources give the words but not where they are drawn, the screen is not read and the generic
rules or the hook carry the state.

| Agent | Screen rules | Hook | Written from |
|---|---|---|---|
| Claude Code | yes | full | its package's strings, plus screens from real sessions |
| Codex | yes | full | its open source, plus screens from real sessions |
| OpenCode | yes | full (plugin) | its open source, plus screens from real sessions |
| Gemini CLI | yes | full | its open source |
| Cursor agent | yes | partial | its package |
| Amp | resume only | not set up by `qoop hooks install` | its package (no screen layout in it) |
| Qwen Code, Kilo, Cline, Kimi, Maki, Pi, Hermes, Grok | yes | none or conversation id | their open source |
| GitHub Copilot CLI, Factory Droid, Qoder CLI, Kiro | yes | none or conversation id | their packages |
| Devin | working and idle only | none | its binary and documentation |
| Muse | startup gates only | none | its binary and documentation |
| Antigravity | resume only | none | its binary and documentation |
| Aider, Goose | generic | not set up by `qoop hooks install` | generic rules |

Anything else is read by the generic rules, and a shell reads as a shell.

## Names that other programs use

`pi`, `hermes`, `kilo`, `muse` and `maki` are also the names of unrelated programs, so they
count as agents only when the process shows it: the package it came from or a runtime
running it. A plain `kilo` editor is never treated as an agent, so idle sleep and
restore never act on it. Muse installs only a launcher named `muse`, so it is not
recognised yet.

## Overriding a rule

Rules live in manifests you can override without a release: put a manifest with the same id
in `~/.config/qoop/agent-detection/` and run `qoop reload-manifests`. `qoop agents explain
<terminal>` says which rule last matched and which source holds the state.
