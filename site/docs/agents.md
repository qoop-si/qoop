# Supported agents

qoro recognises an agent by what is running in the terminal, even when it runs through a
runtime (`node`, `bun`, `python`): an agent installed as a node package reads as itself, not
as `node`. It then reads what the agent is doing from its screen, and from its hook when one
is installed.

States: `starting`, `working`, `blocked` (waiting for you), `done`, `failed`, `idle`,
`uncertain` (two sources disagree, which never pages you) and `sleeping` (closed by the idle
timeout, conversation kept).

## Detected agents

How well each is known is said plainly: rules checked against real screens of the agent are
more trustworthy than rules adapted from another project's descriptions.

| Agent | Screen rules | Hook | Checked against |
|---|---|---|---|
| Claude Code | yes | full | recorded sessions, real captures |
| Codex | yes | full | recorded sessions |
| Gemini CLI | yes | full | recorded sessions |
| OpenCode | yes | full (plugin) | recorded sessions |
| Cursor agent | yes | partial | recorded sessions |
| Amp | yes | by hand | recorded sessions |
| Devin | yes | none | real captures |
| Muse | yes | none | real captures |
| GitHub Copilot CLI | yes | conversation id | adapted rules |
| Factory Droid | yes | conversation id | adapted rules |
| Qwen Code | yes | conversation id | adapted rules |
| Qoder CLI | yes | conversation id | adapted rules |
| Grok CLI | yes | none | adapted rules |
| Kimi, Kiro, Kilo, Hermes, Cline, Antigravity, Maki | yes | none | adapted rules |
| Pi | yes | none | identity checked live, rules adapted |
| Aider, Goose | generic | Goose by hand | generic rules |

Anything else is read by the generic rules, and a shell reads as a shell.

## Names that other programs use

`pi`, `hermes`, `kilo`, `muse` and `maki` are also the names of unrelated programs, so they
count as agents only when the process shows it: the package it came from, a runtime running
it, or muse's versioned binary. A plain `kilo` editor is never treated as an agent, so the
idle timeout and restore never act on it.

## Overriding a rule

Rules live in manifests you can override without a release: put a manifest with the same id
in `~/.config/qoro/agent-detection/` and run `qoro reload-manifests`. `qoro agents explain
<terminal>` says which rule last matched and which source holds the state.
