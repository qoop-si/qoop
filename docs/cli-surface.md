# The command line surface

Owned by `qoro-cli` and grown with it from milestone 1 onward, so that the enumeration
exists before a gate needs it rather than being invented when one does.

Gate G9.1 reads this file: with the control-plane host blackholed, every subcommand here
produces the same result as when it is reachable. Some rows are listed but not run by it:
`qoro`, `qoro attach`, `qoro daemon` and `qoro bridge` do not return, nor does `qoro agents --watch` on macOS
with no daemon, which waits for one; `qoro relay` starts a daemon and then prints
whatever that daemon says to a client rather than a result of its own, and the `qoro notch`
rows start a window or change what the person's login starts, which a comparison run twice
must not do to the machine it runs on. They are still
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
| `qoro ensure-awake` | wake the tab that slept last, or open a first tab when nothing is asleep either, and exit; what `qoro --remote` asks of the far side before its interface opens | no |
| `qoro --space <id\|name>` | the interface with that one space in it and no other: made when the name is new, its last slept tab woken when nothing of it is awake, given a tab when empty; a new space is refused there, and `qoro` still shows them all | yes |
| `qoro --remote <remote>` | the interface for another machine's qoro alone, over ssh, by an added name or an ssh target; a dropped connection comes back on its own | yes |
| `qoro remote add <name> <ssh-target>` | reach a machine over ssh in the foreground, check its qoro speaks this codec, offer to install this version there when it has none, and list its spaces in `qoro` as `<name>:<space>` beside this machine's | yes |
| `qoro remote rm <name>` | stop listing a machine's spaces | no |
| `qoro remote ls` | the machines added and their ssh targets | no |
| `qoro setup show <file>` | what a Setup will run, its content hash and its reproducibility level | no |
| `qoro setup trust <file>` | confirm a Setup on this machine | no |
| `qoro setup check <file>` | whether a Setup may run here | no |
| `qoro hook-report --state <state>` | what an agent's own hook script calls to report a state change, from inside a qoro terminal | no |
| `qoro hook-report --session-id <id>` | report only the agent's conversation id, without claiming a state | no |
| `qoro task new [--replace] <goal>` | open the task of the terminal it runs in (one open at a time; `--replace` closes the open one as superseded) | no |
| `qoro task stage\|plan\|evidence\|done ...` | record on that terminal's task: its stage, its plan (`-` reads stdin), a check and its result, a done claim checked outside the agent | no |
| `qoro task close <id> --verified\|--failed [reason]` | end a task by id, from anywhere | no |
| `qoro task ls\|show [id] [--before E] [--json]\|rules [--turn]` | the open tasks, one task and what was recorded on it, and the short rules an agent's hook prints at session start (`--turn`: the one line it adds to each request) | no |
| `qoro task begin` | what an agent's hook runs at an edit attempt: the terminal's open task, opened now and named after the newest request when there is none | no |
| `qoro send <terminal> <text>` | type into another terminal, over the addressed terminal channel; after `--` every word is text, dashes included | no |
| `qoro send <terminal> --key <name>` | press named keys in another terminal, one frame each: `enter esc tab shift-tab backspace space up down left right home end pageup pagedown ctrl-a..ctrl-z`; arrows follow the program's cursor key mode | no |
| `qoro read <terminal> [--lines N] [--json] [--watch]` | print another terminal's current screen; `--json` prints it as styled runs (`{"cols","cursor","behind","alt","rows":[[{"t","fg","bg","b",...}]]}`, colours as hex), `--watch` again on each change, at most about every 150 ms, without sending a size | no |
| `qoro read <terminal> --back UP [--lines N] [--json]` | read the terminal's history instead: N rows (200 by default), the first of them UP rows before the newest one written; `--json` answers `{"from","total","rows"}` | no |
| `qoro prompt <target> [--wait] [--timeout SECS] [--] <text>` | send text plus Enter to another terminal, refused while it is blocked or waiting on a trust prompt. Flags go right after the target; the first other word starts the text, `--` lets the text start with a dash, and a text ending in `--wait` or `--timeout N` is refused as a misplaced flag unless it follows `--`. `--wait` then waits for the turn the prompt caused, with the exit status of `qoro wait`: from a stopped agent it ends on the first stop after the agent took the prompt; from a busy one (which may fold the prompt into its running turn or queue it) it ends once the agent has stayed stopped for 2 s. Blocked counts as stopped, since it needs a person | no |
| `qoro wait [target...] [--until STATE,...] [--match TEXT] [--timeout SECS] [--json]` | block until a target reaches a state (default `idle,done,blocked,failed`; no target means every agent terminal but the caller's), print the matching rows; exits 124 on timeout and 1 when a target closes. A target already there returns at once, so after writing to one use `prompt --wait`. `--match` names one target and waits for TEXT on its live screen (never the scrollback); with `--until` both must hold | no |
| `qoro new [--name N] [--cwd DIR] [--space ID\|NAME] [-- command...]` | open a terminal in the caller's directory, print its number, and type the command into its shell once it has drawn a prompt, quoted so each argument stays one word; nothing is typed after 10 s. `--space` takes an id or a name, and a new name is made once however many callers race for it. Refused when qoro is not running: it never starts one | no |
| `qoro close <terminal>` | end a terminal and everything it started, as `ctrl-a x` does; refused for the terminal the command runs in | no |
| `qoro worktree new <branch> [--base REF] [--path DIR] [--space ID\|NAME] [--name NAME] [-- CMD...]` | add a git worktree at `<parent>/<repo>-wt-<slug>`, where the slug is the branch lowercased with every run of other characters one dash (`feat/Login` becomes `feat-login`), and open a terminal in it named after the branch. A new branch starts from `--base` or HEAD; an existing one is checked out as is, and one that exists only on a remote is checked out tracking it. A branch checked out elsewhere is refused with where | no |
| `qoro worktree ls [--json]` | every worktree of the current repository, and this machine's terminals inside each | no |
| `qoro worktree rm <branch\|path> [--force]` | remove a linked worktree, keeping its branch. Refused while any process of this user has its directory inside, whatever started it, named by pid and by qoro terminal where known; `--force` never overrides that. qoro's own clients do not count; a qoro daemon running from inside is named and refused, because new terminals start in its directory; a caller whose shell is inside is told to run it from outside; if the processes cannot be listed it refuses. A dirty tree is refused without `--force`, and a worktree whose directory is already gone says to run `git worktree prune` | no |
| `qoro history <target> [--last N] [--role user\|assistant] [--json]` | print another agent's recent transcript window from its own product store, local files only | no |
| `qoro context <target> [--last N] [--role user\|assistant] [--lines N] [--json]` | understand another agent before writing to it: who it is, whether it is writable, its live screen and its recent transcript window, local only | no |
| `qoro agents [--json]` | every terminal: number, state, program, name, workspace, generation | no |
| `qoro agents --watch [--all] [--json]` | the same list now and again each time it reads differently, woken by the daemon's own news on one connection, never by a timer. It only watches, so it keeps no daemon up; when the daemon leaves it prints the empty list and, on macOS, waits without a timer for the next one to start and follows it, elsewhere it ends. `--all` names the session, `default`, on every row, which is what the notch reads | no |
| `qoro agents explain <target> [--json]` | one terminal: state, the source holding it, the screen rule that last matched and its manifest, and what its screen discarded when anything was | no |
| `qoro focus <terminal> [--json]` | show that terminal in every attached interface, as a click on its row would; `--json` answers `{"clients":[pid,...]}`, the interface processes asked, so a launcher can raise the window one runs in | no |
| `qoro reload-manifests` | re-read the agent-detection overrides without restarting the daemon | no |
| `qoro hooks status` | which agents' hooks are installed, and whether they are current | no |
| `qoro hooks install [agent...]` | write an agent's hook and register it in that agent's own configuration, beside whatever is already there | no |
| `qoro hooks uninstall [agent...]` | take out exactly what was installed | no |
| `qoro skills status` | which agents' skills are installed, and whether they are current | no |
| `qoro skills install [agent...]` | teach an agent what qoro is for, beside whatever skills it already has | no |
| `qoro skills uninstall [agent...]` | take out exactly what was installed | no |
| `qoro avatar` | show the picked avatar, or pick one when on a terminal | no |
| `qoro avatar pick` | choose a terminal pet from a batch: a number keeps one, `r` deals again | no |
| `qoro avatar show [--json]` | print the picked avatar; `--json` prints its pixels, each a `#rrggbb` or `null`, rows top first, or `null` with none picked | no |
| `qoro avatar options --json` | every part a pet may have: hair and eyes by id and title, hair colours by ink, title and `#rrggbb` | no |
| `qoro avatar set [--hair H] [--hair-ink N] [--eyes E]` | change those parts of the pet and keep the rest; only parts the picker offers are taken | no |
| `qoro config [--json]` | the settings in `config.toml` (prefix, theme, notifications, the notch), each from the file or its default, where the file is, and what in it could not be used | no |
| `qoro config set <key> <value>` | write one setting, checked first, keeping every other line of the file; `notch.enabled` is read by the notch when it starts or from its settings window | no |
| `qoro settings` | every setting on one page with the pet: on a terminal, typing searches, tab moves the chip, up and down walk the rows (up from the first reaches the pet), left and right change a value and write it at once, esc clears the search or leaves; piped, prints what `qoro config` prints | no |
| `qoro onboard` | the first start, one step a screen: pet, sounds, light or dark, the command key, notifications, sleep, the agent qass runs on; left and right choose, up and down move between rows, enter goes on, esc finishes; piped, says it needs a terminal | no |
| `qoro qass` | open qass, the assistant over every agent: the running one is shown, one asleep in the `qass` space is woken, otherwise it starts in its own folder (`qoro paths qass-data`) on `[qass] agent` from `config.toml` (claude, codex, gemini, opencode or an `[agents]` name), told to start its qass skill; closing it is `qoro close` or the `x` on its row | no |
| `qoro paths [name] [--json]` | where qoro keeps each kind of file (config, data, runtime, cache, logs, qass-config, qass-data, skills) and how big it is; with a name, only that path, for a script or a `--cwd`; `--json` adds `exists` and `bytes` | no |
| `qoro sound` | hear the notification sounds (macOS its own, Linux the freedesktop ones installed) and keep one for a finished agent and one for an agent that waits; on a terminal the arrows move and play, enter keeps, space plays again, `o` turns sound off, `q` or esc stops; piped, a number or a name plays one, an empty line keeps, `off`, `q` or EOF stops | no |
| `qoro notch install` | on macOS 14 or newer, a LaunchAgent that runs `qoro notch run` at login: the agents of every running session in the notch, the pet with a badge for the ones that need you, working and idle counts, and a click for the list | no |
| `qoro notch uninstall` | take the LaunchAgent and the written app out | no |
| `qoro notch status` | whether the notch is installed and running | no |
| `qoro notch run` | write this build's notch app out when missing and run it against this binary | no |
| `qoro ls [--json]` | every space: its id, name, whether it is awake, asleep, partly asleep (`n of m asleep`) or stopped, how many tabs it holds and which are asleep; read from the record when qoro is not running | no |
| `qoro sleep <space> \| --tab <terminal> [--json]` | put one tab to sleep, every pane of its split, or every tab of a space (by id or by a name only one space has): what sleeps ends, its rows are kept, and nothing of it runs until it wakes. A tab sleeps whole or not at all: a command running, an agent working, blocked or not yet telling its conversation (`qoro hooks install`), a job in the background or a wait between tabs keeps it awake, and the answer names the pane and why; a space sleeps each tab that can and lists the ones that stayed awake. With qoro not running it says nothing is awake | no |
| `qoro wake [space] \| --tab <terminal> [--json]` | bring one sleeping tab back, every sleeping tab of a space, or with nothing named the tab that slept last: the same numbers, names, splits, directories and scrollback, each agent resumed into its own conversation by its id; starts qoro when it is not running | no |
| `qoro space rm <id\|name>` | remove a space with no tab in it, awake or asleep: refused with how many it holds otherwise, as is a name two spaces share (use the id) and the default space. Works through the running daemon and never starts one; its tabs are never closed for it | no |
| `qoro stop` | end qoro now: every terminal ends and what was awake comes back on the next start; a tab that was asleep stays asleep | no |
| `qoro pair [--hub URL]` | pair a phone: print a QR code and a link, then take the six digit code the phone shows; the machine key and the bridge service appear only once a phone is paired | yes |
| `qoro devices ls` | the phones paired with this machine, with their key fingerprints | no |
| `qoro devices rm <name>` | unpair a phone, by its name or its key fingerprint; the running bridge closes its open streams at once | no |
| `qoro bridge` | the phones' end of this machine, in the foreground: one outbound connection to the hub, piping each paired phone to this machine's qoro while it runs | yes |
| `qoro update [--check]` | ask GitHub Releases for the newest binary for this machine (macOS or Linux, arm64 or x86_64) and replace this one with it; refused while a tab is awake (a qoro with nothing awake is stopped first), and `--check` only notes what is newest | yes |
| `qoro --hello` | what a remote bootstrap asks for: version, codec generation, platform | no |
| `qoro --help` | this surface | no |
| `qoro --version` | the version | no |

## Options

| Option | What it does |
|---|---|
| `--session default` | accepted and ignored: it is what an older qoro on the other end of ssh sends. Named sessions were replaced by spaces, so any other name, as the flag or as `QORO_SESSION`, is refused with where that session's files are and how to use them instead. |

`send`, `prompt` and `close` take `--generation G`, the number `qoro agents` prints: the
write is refused unless the terminal still runs that generation, so an answer meant for one
process never reaches the one that replaced it.

How long an idle tab waits before it sleeps by itself, and whether qoro leaves when
nothing is awake, are settings in `config.toml`, not options: [config.md](config.md).

A word that is not a subcommand is an error naming it, not the interface: `qoro stauts`
says so and exits 1. Every error prints as one `qoro: ...` line on stderr and exits
nonzero.

Three rows say yes, and each is the point of its row: `qoro update` reaches GitHub
Releases, and `qoro pair` and `qoro bridge` reach the hub a phone uses, and only once a
person has asked for a phone. Nothing else touches the network: no control plane, no
background version check, and a machine that never pairs a phone never opens a
connection to a hub. Anything else that ever needs the network is an addition to this
table with a yes, plus a row in the gate saying what happens when it is unreachable.

The gate skips the rows that say yes: with the network blackholed `qoro update` fails
closed with "cannot reach GitHub Releases" and `qoro pair` fails naming the hub it could
not reach, leaving no key, device list or service behind; both are the failure a person
without a network gets, and asserting byte-identical output for them would be asserting
that the network is reachable.

## What "the same result" means

Byte for byte on stdout, and the same exit status. Not "still works": a subcommand that
printed a warning about an unreachable host when the host is unreachable would be a
subcommand whose output depends on the network, which is the thing being ruled out.

## Adding a subcommand

Add it here in the same change. The gate enumerates this file, so a subcommand that is not
here is a subcommand nobody checks, and a row here with no subcommand fails the gate too.
