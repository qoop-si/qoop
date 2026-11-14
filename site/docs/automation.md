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
qoop worktree ls --stale                    the clean ones that are empty, merged or old
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
`rm` it runs from outside the worktree. `ls --stale` only lists, from the refs here.

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
