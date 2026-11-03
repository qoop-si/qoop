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
an open task that comes to rest reaches it within about half a minute; stopped again in the same
stage with nothing from qass in between, it reaches qass after 10 minutes at rest, and not
if it works again first, though at least once an hour while it keeps stopping. qass answers a question the agent's rules already settle, resumes
after an error or a usage limit, and pushes the step the task's goal still lacks. A question left to you for 5 minutes stays open while the agent does the
parts of its task that do not depend on it; your answer reaches it once it is at rest. It never types into a tab while you are at it (your keys there in the last
minutes), and stops after a few writes to one agent that do not move it, asking you instead.
Agents themselves are told to take the next step their rules allow and to end a turn on you only
for your decision. In a repository whose GitHub
`origin` belongs to the login `gh` is signed in as, every agent is told the standing way
(`qoop task rules`): commit through /commit, push, open a PR, rebase-and-merge its own PR,
install and clean up; elsewhere it asks first, and delete, release, production, money and
anything external always ask. A repository's `AGENTS.md` may ask for more, never allow
more. When you correct it or say how you want things, it keeps the rule in
`$(qoop paths qass-config)/lessons.md` (8,000 characters at most, loaded at every start and after
every compaction, with its own notes in `memory.md`); a lesson can
let it settle a kind of reversible step at once, never anything that always asks. qass keeps
its conversation in the model's full window, so it compacts rarely and keeps what it learned.
Each night at 04:30 a run in `$(qoop paths data)/learn` reads what you typed to every agent that
day (`qoop history --since <when>`, which leaves out what qoop and qass typed for you, other
agents' messages, pasted blocks and runs nobody sat at) and keeps what you said in two or more
sessions in `$(qoop paths qass-config)/owner.md`. Every agent is handed it with `qoop task rules`;
a rule you keep repeating although it is written moves to the line said beside each request. A
learned rule never lets an agent do what it asks about today; one that would waits for your
word. When anything changed, a short report reaches your phone; tell qass "learn drop <n>" to
remove a rule, "learn keep <n>" to accept one that waited, "learned" to see them.
Its folder's `.claude/settings.json` keeps what would fill that window on its own out of
qass: every plugin you turned on in Claude (turned off for qass alone, again at each
`qoop qass`), every MCP tool, the built-in tools qass never calls, a skill list past 3,000
characters and a command's output past 15,000. `qoop paths`
lists every folder qoop writes to and how big each is.
