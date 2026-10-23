# qass

qass comes with the skill install: an assistant that sits above every
other agent: it is told when one stops, reads where that work stands (its task, its plan
file, the repo's rules), answers what it can the way you would, and sends you only what
needs your word. You answer it in its own tab, and it passes your answer on. Start it in
its own space:

```
qoop qass
```

It opens qass in its own space on the agent `qoop settings` names (`qass runs on`: claude by
default, codex, gemini, opencode, or one of your own `[agents]` lines, such as
`claudes = "claude --model sonnet"`), or shows or wakes the one you have. `ctrl-a Q` goes there and back.
Close it as any tab: `ctrl-a x` in it, or `qoop close`.
Its rules live in the skill itself: what it answers alone, what it asks you, and what it
never does. It writes to an agent within those rules without asking you first; tell it "ask me
first" and it keeps that until you say otherwise. Work that stops is its to move on: an agent on
an open task that comes to rest reaches it within half a minute, and it answers a question the
agent's rules already settle, resumes after an error or a usage limit, and pushes the step the
task's goal still lacks. It never types into a tab while you are at it (your keys there in the last
minutes), and stops after a few writes to one agent that do not move it, asking you instead.
Agents themselves are told to take the next step their rules allow and to end a turn on you only
for your decision. In a repository whose GitHub
`origin` belongs to the login `gh` is signed in as, every agent is told the standing way
(`qoop task rules`): commit through /commit, push, open a PR, rebase-and-merge its own PR,
install and clean up; elsewhere it asks first, and delete, release, production, money and
anything external always ask. A repository's `AGENTS.md` may ask for more, never allow
more. When you correct it or say how you want things, it keeps the rule in
`$(qoop paths qass-config)/lessons.md` (3,000 characters at most, loaded at every start); a lesson can
let it settle a kind of reversible step at once, never anything that always asks. qass alone
compacts its conversation before it passes about 100k tokens (its folder's `.claude/settings.json`
sets `CLAUDE_CODE_AUTO_COMPACT_WINDOW`), since every wake reads it again. `qoop paths`
lists every folder qoop writes to and how big each is.
