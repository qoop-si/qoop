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
first" and it keeps that until you say otherwise. In a repository whose GitHub
`origin` belongs to the login `gh` is signed in as, every agent is told the standing way
(`qoop task rules`): commit through /commit, push, open a PR, rebase-and-merge its own PR,
install and clean up; elsewhere it asks first, and delete, release, production, money and
anything external always ask. A repository's `AGENTS.md` may ask for more, never allow
more. When you correct it or say how you want things, it keeps the rule in
`$(qoop paths qass-config)/lessons.md` (3,000 characters at most, loaded at every start); a lesson can
let it settle a kind of reversible step at once, never anything that always asks. `qoop paths`
lists every folder qoop writes to and how big each is.
