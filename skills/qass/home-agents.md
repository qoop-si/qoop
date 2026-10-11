# qass

This folder is qass's home. qoop always starts qass here, on whichever agent `[qass] agent`
names, and everything qass keeps lives here. qoop wrote this file once; keep your own notes in
the files below, not here.

- Skill: `@QASS_DATA@/.claude/skills/qass/SKILL.md` (Claude) and
  `@QASS_DATA@/.agents/skills/qass/SKILL.md` (Codex, Gemini, opencode), the same files. Its
  scripts (`watch.sh`, `escalate.sh`, `spawn.sh`, `on.sh`) sit beside it: run them from there,
  never from an older path you remember, and quote the path: on a Mac it holds a space.
- The owner's lessons: `@QASS_CONFIG@/lessons.md`. Read it at every start: it is how he wants
  things done. His `policy` is beside it, and is his alone.
- Memory: `@QASS_DATA@/memory.md`, your own notes that should outlive this conversation (what
  you learned about the fleet, the repos, the machines). Read it at every start; keep it short
  and rewrite it whole.
- State: `@QASS_DATA@/goals.md`, as the skill says.
- Board: `@QASS_DATA@/board/`, every space's goal and open tasks, written by `qoop task board`
  on every wake. Read it, never edit it; it is safe to delete.
- Setup: `@QASS_DATA@/setup`, what the person picked in `/qass setup` (`setup.md`), written only
  by `setup.sh`.
- Scratchpad: `@QASS_DATA@/scratch/`, for anything temporary. Nothing else here is scratch.
