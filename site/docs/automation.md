# Automation

Everything the interface does to a terminal can be done from a shell, which is how one agent
works with another, and how you script a fleet.

## Look before you write

```
qoop agents                      every terminal: number, state, program, name, space
qoop agents explain 3            why terminal 3 reads the way it does
qoop read 3                      its screen right now
qoop history 3 --last 5          its last turns, from the agent's own transcript
qoop context 3                   all of the above in one call
```

## Write

```
qoop prompt 3 "fix the failing test"        text plus Enter; refused while it is blocked
qoop send 3 "y" --enter                     raw typing, no checks
qoop send 3 --key esc --key enter           named keys
```

`--generation G` on `send`, `prompt` and `close` refuses the write when terminal 3 no longer
runs the process you read, so a message never lands on the agent that replaced it.

## Wait

```
qoop prompt 3 --wait "run the tests"        send, then wait for the turn it caused
qoop wait 3                                 until it stops working
qoop wait 3 --until blocked --timeout 600   a state, or give up
qoop wait 3 --match "All tests passed"      text on its screen
qoop wait                                   every agent terminal but this one
```

`wait` exits 0 when the state is reached, 124 on timeout (like `timeout(1)`), 1 when the
terminal closes. A target already in the state returns at once, so after writing to an
agent use `prompt --wait`, which first waits for the agent to take the prompt: from a busy
agent (which may fold the prompt into its running turn or queue it) it ends once the agent
has stayed stopped for two seconds.

## Start work

```
t=$(qoop new --name tests -- cargo test)    a new terminal running a command
qoop new --space review -- claude           in a space, made if it does not exist
qoop close 7                                end a terminal and what it started
```

## Worktrees

```
qoop worktree new feat/login -- claude      a git worktree beside the repo, an agent in it
qoop worktree setup feat/login              run its space's setup again
qoop worktree finish feat/login --pr        push it and open a pull request, keeping it
qoop worktree finish feat/login             once merged: remove it, its branch and origin's
qoop worktree ls --stale                    the clean ones that are merged or old
qoop worktree ls                            every worktree and the terminals inside it
qoop worktree rm feat/login                 remove it, keeping the branch
```

`worktree new` puts the worktree at `<parent>/<repo>-wt-feat-login` and opens its terminal in
the space whose folder is the repository (`qoop space dir NAME <repo>` sets one), else in the
space whose folder holds it, else in the default space; `--space` picks one. Before the
terminal opens, that space's `[spaces.<name>.worktree]` in `config.toml` copies the ignored
files it lists and runs its `setup` (see [Configuration](#configuration)); stdout says
`setup=done`, `none`, `skipped` (`--no-setup`) or `failed`. A failed setup keeps the worktree,
opens no terminal and exits 1; `worktree setup <branch>` runs it again. `worktree rm` refuses
while any process has its directory inside (named by pid, and by qoop terminal where it
knows), and refuses a tree with uncommitted changes unless `--force`.

`worktree finish` deletes only work it can prove is kept: plain `finish` removes the worktree,
its branch and `origin/<branch>` once the branch is merged (fast-forward, rebase, or a squash
merge gh knows of), and refuses otherwise. `--pr` pushes and opens a pull request, keeping the
worktree for the review's fixes; `--merge` fast-forwards the main checkout's branch, locally;
`--discard` drops the work. It never force pushes, every step is safe to run again, and like
`rm` it runs from outside the worktree: every mode, `--discard` included, refuses while a
process is inside. `ls --stale` only lists, from the refs here.

## Race

```
qoop race new login --agents claude,codex -- add a login page    one prompt, two agents
qoop race new login --agents claude,codex --wait -- add a login page
qoop race ls login --test 'cargo test'      each racer's files, lines and tests
qoop race rm login --keep codex --force     drop every racer but the winner
```

`race new` gives each agent (2 to 5, a name from `[agents]` or `claude`, `codex`, `gemini`,
`opencode`; a second of one agent is `claude-2`) a branch `race/<name>/<agent>`, a worktree
where `worktree new` puts one, set up as its space says, and a tab named after the branch with
the agent started on the prompt. Under the prompt each racer is told to change files in its
worktree only, never commit, push, open a PR, merge, close its tab or start agents, and to end
with a short self-report. Every racer starts from one commit: `HEAD` of the checkout you run it
in, or `--base`; uncommitted changes there are not in the racers. A worker agent's terminal
cannot start a race; your own shell and qass can. `--wait` returns once every racer finished,
failed, is blocked, asleep, closed, quit to its shell or never started, a line each, and exits
0 only when every racer finished. A blocked racer waits for you: its line names the `qoop wait`
that keeps waiting on it.

`race ls` counts what each worktree changed since that commit, committed or not; `--test` runs
your command in each worktree, which runs the racer's code. To end a race, judge with `race ls`,
then `race rm <name> --keep <winner> --force`: the racers were told not to commit, so every
loser that did anything has uncommitted changes, and `--force` is what drops them. A racer
still working is never dropped, `--force` or not. The winner keeps its branch, worktree and tab;
once it has landed, `race rm <name> --force` drops it and the race too. Like `worktree rm`, it
never removes a folder a process still stands in: a tool server an agent's own background
service started there (Codex's app server keeps them) is named, and `race rm` stops; once
those end, run it again and it finishes.

Each agent must be installed and signed in, and cost and memory grow with each racer. qoop adds
no permission flag: without an `[agents]` line that carries yours, each racer stops at its first
permission prompt, and Claude asks once to trust each new worktree folder. The phone's Changes
view diffs a racer against the default branch, so with another base it shows that base's own
commits too; `race ls` is exact.

## Spaces

```
qoop space rm review                        remove a space that has no tabs in it
```

`space rm` refuses a space that still holds a tab, awake or asleep, and says how many.
It never closes tabs for you: close them first.

## Exit codes

Every command exits 0 on success, 1 when it failed, and 2 when it was called wrong. Errors
are one line on stderr.

`qoop --help` lists every command.
