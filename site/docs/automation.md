# Automation

Everything the interface does to a terminal can be done from a shell, which is how one agent
works with another, and how you script a fleet.

## Look before you write

```
qoro agents                      every terminal: number, state, program, name, space
qoro agents explain 3            why terminal 3 reads the way it does
qoro read 3                      its screen right now
qoro history 3 --last 5          its last turns, from the agent's own transcript
qoro context 3                   all of the above in one call
```

## Write

```
qoro prompt 3 "fix the failing test"        text plus Enter; refused while it is blocked
qoro send 3 "y" --enter                     raw typing, no checks
qoro send 3 --key esc --key enter           named keys
```

`--generation G` on `send`, `prompt` and `close` refuses the write when terminal 3 no longer
runs the process you read, so a message never lands on the agent that replaced it.

## Wait

```
qoro prompt 3 --wait "run the tests"        send, then wait for the turn it caused
qoro wait 3                                 until it stops working
qoro wait 3 --until blocked --timeout 600   a state, or give up
qoro wait 3 --match "All tests passed"      text on its screen
qoro wait                                   every agent terminal but this one
```

`wait` exits 0 when the state is reached, 124 on timeout (like `timeout(1)`), 1 when the
terminal closes. A target already in the state returns at once, so after writing to an
agent use `prompt --wait`, which first waits for the agent to take the prompt: from a busy
agent (which may fold the prompt into its running turn or queue it) it ends once the agent
has stayed stopped for two seconds.

## Start work

```
t=$(qoro new --name tests -- cargo test)    a new terminal running a command
qoro new --space review -- claude           in a space, made if it does not exist
qoro close 7                                end a terminal and what it started
```

## Worktrees

```
qoro worktree new feat/login -- claude      a git worktree beside the repo, an agent in it
qoro worktree ls                            every worktree and the terminals inside it
qoro worktree rm feat/login                 remove it, keeping the branch
```

`worktree new` puts the worktree at `<parent>/<repo>-wt-feat-login`. `worktree rm` refuses
while any process has its directory inside (named by pid, and by qoro terminal where it
knows), and refuses a tree with uncommitted changes unless `--force`.

## Exit codes

Every command exits 0 on success, 1 when it failed, and 2 when it was called wrong. Errors
are one line on stderr.

`qoro --help` lists every command; so does `docs/cli-surface.md` in the repository.
