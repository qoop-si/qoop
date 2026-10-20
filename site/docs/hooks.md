# Hooks and skills

## Hooks

Hooks are optional. Every agent is read from its screen without one. A hook buys two things:
a state known the moment it changes rather than after it is drawn, and the agent's own id
for its conversation, which is what lets a stopped or sleeping tab come back to that conversation.

qoop sets them up by itself: when it is installed or updated, on a computer added over ssh, and
for an agent installed after qoop, which qoop notices within seconds on this machine and on
every ssh computer. Removing one with `qoop hooks uninstall <agent>` keeps it off until
`qoop hooks install <agent>`; the same holds for skills.

```
qoop hooks status               which are installed, which agents are not on this machine
qoop hooks install              every agent qoop ships a hook for that is on this machine
qoop hooks install claude       one of them
qoop hooks uninstall claude     exactly what was installed, and nothing else
```

| Agent | What the hook reports |
|---|---|
| Claude Code, Codex, Gemini CLI, OpenCode | state and conversation id |
| Cursor | partial state |
| Copilot CLI, Droid, Qwen Code, Qoder CLI | conversation id at session start |
| Amp, Goose | installed by hand, see `hooks/` in the repository |

What the installer promises about your configuration files:

- It adds beside what is there, and installing twice changes nothing.
- A file it cannot parse is reported, never overwritten.
- Writes go to a temporary file and are renamed over the real one; a 0600 file stays 0600,
  a symlinked file stays a symlink, and a read-only file is left alone and reported.
- A copy is kept before every write; the first copy and the newest three are kept.
- Uninstalling right after installing gives the file back byte for byte in the common cases,
  and removes a file the install created.

`COPILOT_HOME`, `QWEN_HOME` and `QODER_CONFIG_DIR` are honoured. An agent that is already
running reads its configuration only at start, so start it afresh after installing.

## Skills

```
qoop skills install
```

teaches each agent what qoop is for, as a skill placed beside the agent's own skills: how to
look at another terminal before writing to it, how to prompt another agent and wait for it,
how to start work in a new terminal or git worktree. `qoop skills status` and
`qoop skills uninstall` work like the hook commands.
