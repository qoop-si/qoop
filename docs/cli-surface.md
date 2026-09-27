# The command line surface

Owned by `qoro-cli` and grown with it from milestone 1 onward, so that the enumeration
exists before a gate needs it rather than being invented when one does.

Gate G9.1 reads this file: with the control-plane host blackholed, every subcommand here
produces the same result as when it is reachable. Four rows are listed but not run by it:
`qoro`, `qoro attach` and `qoro daemon` do not return, and `qoro relay` starts a daemon and then prints
whatever that daemon says to a client rather than a result of its own. They are still
required to be here, and the gate asserts that this table and `qoro --help` name the same
set, which is what stopped `relay` from staying undocumented after milestone 6 added it.

An optional control plane is the whole claim here, and a claim that is only made in prose is
a claim nobody has to keep.

## Subcommands

| Command | What it does | Needs the network |
|---|---|---|
| `qoro` | open the interface, starting the daemon if there is not one | no |
| `qoro attach` | attach to one terminal, with no interface around it | no |
| `qoro daemon` | run the daemon in the foreground | no |
| `qoro reset` | restore a terminal left in raw mode by a killed client | no |
| `qoro relay` | pipe stdin and stdout to this machine's daemon, which is what `ssh` starts on the far side | no |
| `qoro --remote <remote>` | the interface for another machine's qoro alone, over ssh, by an added name or an ssh target; a dropped connection comes back on its own | yes |
| `qoro remote add <name> <ssh-target> [--session S]` | reach a machine over ssh in the foreground, check its qoro speaks this codec, and list its spaces in `qoro` as `<name>:<space>` beside this machine's | yes |
| `qoro remote rm <name>` | stop listing a machine's spaces | no |
| `qoro remote ls` | the machines added, their ssh targets and sessions | no |
| `qoro setup show <file>` | what a Setup will run, its content hash and its reproducibility level | no |
| `qoro setup trust <file>` | confirm a Setup on this machine | no |
| `qoro setup check <file>` | whether a Setup may run here | no |
| `qoro hook-report --state <state>` | what an agent's own hook script calls to report a state change, from inside a qoro terminal | no |
| `qoro hook-report --session-id <id>` | report only the agent's conversation id, without claiming a state | no |
| `qoro send <terminal> <text>` | type into another terminal's session, over the addressed terminal channel | no |
| `qoro send <terminal> --key <name>` | press named keys in another terminal, one frame each: `enter esc tab shift-tab backspace space up down left right home end pageup pagedown ctrl-a..ctrl-z`; arrows follow the program's cursor key mode | no |
| `qoro read <terminal>` | print another terminal's current screen | no |
| `qoro prompt <target> [--wait] [--timeout SECS] [--] <text>` | send text plus Enter to another session, refused while it is blocked or waiting on a trust prompt. Flags go right after the target; the first other word starts the text, `--` lets the text start with a dash, and a text ending in `--wait` or `--timeout N` is refused as a misplaced flag unless it follows `--`. `--wait` then waits for the turn the prompt caused, with the exit status of `qoro wait`: from a stopped agent it ends on the first stop after the agent took the prompt; from a busy one (which may fold the prompt into its running turn or queue it) it ends once the agent has stayed stopped for 2 s. Blocked counts as stopped, since it needs a person | no |
| `qoro wait [target...] [--until STATE,...] [--match TEXT] [--timeout SECS] [--json]` | block until a target reaches a state (default `idle,done,blocked,failed`; no target means every agent terminal but the caller's), print the matching rows; exits 124 on timeout and 1 when a target closes. A target already there returns at once, so after writing to one use `prompt --wait`. `--match` names one target and waits for TEXT on its live screen (never the scrollback); with `--until` both must hold | no |
| `qoro new [--name N] [--cwd DIR] [--space ID\|NAME] [-- command...]` | open a terminal in the caller's directory, print its number, and type the command into its shell once it has drawn a prompt, quoted so each argument stays one word; nothing is typed after 10 s. `--space` takes an id or a name, and a new name is made once however many callers race for it. Refused when no session runs: it never starts one | no |
| `qoro close <terminal>` | end a terminal and everything it started, as `ctrl-a x` does; refused for the terminal the command runs in | no |
| `qoro worktree new <branch> [--base REF] [--path DIR] [--space ID\|NAME] [--name NAME] [-- CMD...]` | add a git worktree at `<parent>/<repo>-wt-<slug>`, where the slug is the branch lowercased with every run of other characters one dash (`feat/Login` becomes `feat-login`), and open a terminal in it named after the branch. A new branch starts from `--base` or HEAD; an existing one is checked out as is, and one that exists only on a remote is checked out tracking it. A branch checked out elsewhere is refused with where | no |
| `qoro worktree ls [--json]` | every worktree of the current repository, and this session's terminals inside each | no |
| `qoro worktree rm <branch\|path> [--force]` | remove a linked worktree, keeping its branch. Refused while any process of this user has its directory inside, whatever started it, named by pid and by qoro terminal where known; `--force` never overrides that. qoro's own clients do not count; a qoro daemon running from inside is named and refused, because new terminals start in its directory; a caller whose shell is inside is told to run it from outside; if the processes cannot be listed it refuses. A dirty tree is refused without `--force`, and a worktree whose directory is already gone says to run `git worktree prune` | no |
| `qoro history <target> [--last N] [--role user\|assistant] [--json]` | print another agent's recent transcript window from its own product store, local files only | no |
| `qoro context <target> [--last N] [--role user\|assistant] [--lines N] [--json]` | understand another agent before writing to it: who it is, whether it is writable, its live screen and its recent transcript window, local only | no |
| `qoro agents [--json]` | every terminal: number, state, program, name, workspace, generation | no |
| `qoro agents explain <target>` | one terminal: state, the source holding it, the screen rule that last matched and its manifest | no |
| `qoro reload-manifests` | re-read the agent-detection overrides without restarting the daemon | no |
| `qoro hooks status` | which agents' hooks are installed, and whether they are current | no |
| `qoro hooks install [agent...]` | write an agent's hook and register it in that agent's own configuration, beside whatever is already there | no |
| `qoro hooks uninstall [agent...]` | take out exactly what was installed | no |
| `qoro skills status` | which agents' skills are installed, and whether they are current | no |
| `qoro skills install [agent...]` | teach an agent what qoro is for, beside whatever skills it already has | no |
| `qoro skills uninstall [agent...]` | take out exactly what was installed | no |
| `qoro avatar` | show the picked avatar, or pick one when on a terminal | no |
| `qoro avatar pick` | choose a terminal pet from a batch: a number keeps one, `r` deals again | no |
| `qoro avatar show` | print the picked avatar | no |
| `qoro ls` | every session, whether it is running, and how many tabs it holds | no |
| `qoro stop [name]` | end a session: its daemon and every terminal in it. The tabs come back on the next start | no |
| `qoro update [--check]` | ask GitHub Releases for the newest binary for this machine (macOS or Linux, arm64 or x86_64) and replace this one with it; refused while a session runs, and `--check` only notes what is newest | yes |
| `qoro --hello` | what a remote bootstrap asks for: version, codec generation, platform | no |
| `qoro --help` | this surface | no |
| `qoro --version` | the version | no |

## Options

| Option | What it does |
|---|---|
| `--session <name>` | which session to work in. Sessions do not see each other: each is its own daemon, its own socket and its own record of what is open. `QORO_SESSION` does the same for a shell that would rather set it once. The flag wins over the environment, which wins over the session last opened, which wins over the `default` session: a bare `qoro` reopens whatever was opened last. |
| `QORO_IDLE_TIMEOUT_MINUTES` | idle auto-sleep: when set to a whole number of minutes N (recommended: `30`), the daemon closes terminals idle past N minutes with no background task running, through the same path as closing the tab by hand. Unset, `0` or invalid means disabled, which is the default. A reaped tab stays listed as a `sleeping` row until dismissed with `qoro`'s close; the agent's own conversation survives in the agent's own store. An agent that sent to another terminal with `qoro send` or `qoro prompt` stays awake while that terminal is still busy, for at most a day. |

`send`, `prompt` and `close` take `--generation G`, the number `qoro agents` prints: the
write is refused unless the terminal still runs that generation, so an answer meant for one
process never reaches the one that replaced it. A sleeping row has no generation, so close
it without the flag.

The flag may be written anywhere in the command, so `qoro --session work send 2 "x"` and
`qoro send --session work 2 "x"` are the same thing. It is put into the environment of
everything the session starts, which is what lets a shell inside a session say `qoro send`
and reach its own daemon rather than the default one. It has no short form, because the
text of `qoro send` and `qoro prompt` is searched too and `du -s` there is text. An empty
value (a trailing `--session`, or `qoro stop ""`) is refused rather than read as `default`.

A word that is not a subcommand is an error naming it, not the interface: `qoro stauts`
says so and exits 1. Every error prints as one `qoro: ...` line on stderr and exits
nonzero.

Every row says no except one, and the one is the point: `qoro update` is the only
subcommand that reaches the network, and only a person runs it. There is no control
plane in the product, no background version check anywhere, and if anything else ever
needs the network it is an addition to this table with a yes, plus a row in the gate
saying what happens when it is unreachable.

The gate skips the rows that say yes: with the network blackholed they fail closed
with "cannot reach GitHub Releases", which is the same failure a person without a
network gets, and asserting byte-identical output for them would be asserting that
the network is reachable.

## What "the same result" means

Byte for byte on stdout, and the same exit status. Not "still works": a subcommand that
printed a warning about an unreachable host when the host is unreachable would be a
subcommand whose output depends on the network, which is the thing being ruled out.

## Adding a subcommand

Add it here in the same change. The gate enumerates this file, so a subcommand that is not
here is a subcommand nobody checks, and a row here with no subcommand fails the gate too.
