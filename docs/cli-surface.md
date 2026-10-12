# The command line surface

Owned by `qoop-cli` and grown with it from milestone 1 onward, so that the enumeration
exists before a gate needs it rather than being invented when one does.

Gate G9.1 reads this file: with the control-plane host blackholed, every subcommand here
produces the same result as when it is reachable. Some rows are listed but not run by it:
`qoop`, `qoop attach`, `qoop daemon` and `qoop bridge` do not return, nor does `qoop agents --watch` on macOS
with no daemon, which waits for one; `qoop relay` starts a daemon and then prints
whatever that daemon says to a client rather than a result of its own, and the `qoop notch`
rows start a window or change what the person's login starts, which a comparison run twice
must not do to the machine it runs on. They are still
required to be here, and the gate asserts that this table and `qoop --help` name the same
set, which is what stopped `relay` from staying undocumented after milestone 6 added it.

An optional control plane is the whole claim here, and a claim that is only made in prose is
a claim nobody has to keep.

## Subcommands

| Command | What it does | Needs the network |
|---|---|---|
| `qoop` | open the interface, starting the daemon if there is not one | no |
| `qoop attach` | attach to one terminal, with no interface around it | no |
| `qoop daemon` | run the daemon in the foreground. A daemon that moves onto a new binary starts that binary as `qoop daemon --adopt <file> --nonce <n>`, with the running tabs' ptys open, after a dry run with `--adopt-check`; neither is for typing | no |
| `qoop reset` | restore a terminal left in raw mode by a killed client | no |
| `qoop relay` | pipe stdin and stdout to this machine's daemon, which is what `ssh` starts on the far side | no |
| `qoop ensure-awake` | wake the tab that slept last, or open a first tab when nothing is asleep either, and exit; what `qoop --remote` asks of the far side before its interface opens | no |
| `qoop --space <id\|name>` | the interface with that one space in it and no other: made when the name is new, its last slept tab woken when nothing of it is awake, given a tab when empty; a new space is refused there, and `qoop` still shows them all | yes |
| `qoop --remote <remote>` | the interface for another machine's qoop alone, over ssh, by an added name or an ssh target; a dropped connection comes back on its own | yes |
| `qoop remote add <name> <ssh-target>` | reach a machine over ssh in the foreground, check its qoop speaks this codec, offer to install this version there when it has none, and list its spaces in `qoop` as `<name>:<space>` beside this machine's | yes |
| `qoop remote rm <name>` | stop listing a machine's spaces | no |
| `qoop remote ls` | the machines added and their ssh targets | no |
| `qoop setup show <file>` | what a Setup will run, its content hash and its reproducibility level | no |
| `qoop setup trust <file>` | confirm a Setup on this machine | no |
| `qoop setup check <file>` | whether a Setup may run here | no |
| `qoop hook-report --state <state>` | what an agent's own hook script calls to report a state change, from inside a qoop terminal | no |
| `qoop hook-report --session-id <id>` | report only the agent's conversation id, without claiming a state | no |
| `qoop task new [--replace] <goal>` | open the task of the terminal it runs in (one open at a time; `--replace` closes the open one as superseded) | no |
| `qoop task goal [--id KEY\|N] <text\|->` | rewrite a task's goal: what the owner wants and when it is done, in plain words. Without `--id` the terminal's live task, begun first as `begin` does (nothing is recorded in qass's space, and it says so); with `--id` any open task, as qass sets a worker's, or an item's title by its key. One line, at most 300 characters, refused past that; an item's title over 32 is cut at a word, the whole text kept in its history | no |
| `qoop task stage\|plan\|evidence\|done ...` | record on that terminal's task: its stage, its plan (`-` reads stdin), a check and its result, a done claim checked outside the agent | no |
| `qoop task close <KEY\|id> --verified\|--failed\|--dropped [reason]` | end a task or a board item by id or key, from anywhere; `--dropped` closes an item as abandoned, which takes it off the board (items only: a task is never dropped) | no |
| `qoop task verify <KEY\|id> [--repo <path>] [--json]` | the closing facts of a finished task, read-only, so qass does no arithmetic: never closes or writes anything, at most 200 words cut by the code, never a screen's or a transcript's text. Its repo from its live row's folder (a folder gone: `<parent>/<repo>-wt-<slug>` back to the first prefix that is a main checkout; else `--repo`, standing in for that folder: its `-wt-` folder, even when gone, or a main checkout, which ties nothing). Each PR its evidence names (`PR #N`, `pull #N`, `owner/repo#N`, a github.com pull link, `http`, `https` or `www.` alike; a bare `#N` is not one), asked of `gh`, must be this repo's, into its default branch, created and merged after the task opened, from the worker's own `-wt-<slug>` worktree (none to match: not tied); then no open PR from the same branch, that worktree no longer listed, and the install named by qass's approved copy `init-<repo>.md` (its `- where: installed <path>` line, never the repo's own workflow file) newer than the merge. No PR named, a `gh` missing or failing, or a remote worker is missing, never a pass. An item: every linked task still open (a closed one was checked when it closed). Its last line is `Missing: <each>`, or `Missing: none; still yours: Verify where it runs, then close`. `--json` adds `missing` and `passed` | no |
| `qoop task add --space <S> <title> [--description <text\|->]` | put an item on a space's board, from anywhere, and print its key (`<PREFIX>-<n>`, the space's letters and digits, upper case, at most 10, `ITEM` for none, fixed when the item is made); refused in qass's own space. Title at most 300 characters; one over 32 is cut at a word to fit a card (a line says so), its whole text kept in the item's history and, when it fits, at the top of the description. Description at most 2,500 | no |
| `qoop task describe [--id KEY\|N] <text\|->` | rewrite an item's description; without `--id`, the item this terminal's task is linked to, refused when it is on none. Every `--id` here takes an item's key or an id | no |
| `qoop task steps [--id KEY\|N] -` | replace an item's steps, one per line on standard input, at most 30 of 200 characters; a step whose text is unchanged keeps its tick | no |
| `qoop task check [--id KEY\|N] <n> [--undo]` | tick step `n` of an item, or untick it | no |
| `qoop task link [--id N] <KEY\|#id>` | put this terminal's task (begun first, as `begin` does) or task `N` on an open item; refused in qass's space | no |
| `qoop task link <KEY\|#id> --remote <R> --task <N> --name <NAME> --stage <S>` | what qass reports for an agent on the ssh remote `R` working on the item: its task there, its name and its stage | no |
| `qoop task unlink [--id KEY\|N]` | take this terminal's task (or task `N`) off its item; named by id, an item loses every agent and a waiting start, and is Todo again | no |
| `qoop task start <KEY\|id>` | ask for an item to be started: it reads Doing for ten minutes, and `start <KEY>` (the key as stored) is typed into qass's terminal when qass runs | no |
| `qoop task reopen <KEY\|id>` | bring a closed item back to the board, its steps kept; refused once its space is gone | no |
| `qoop task ls\|show [KEY\|id] [--before E] [--json]\|rules [--turn]` | the open tasks, one task and what was recorded on it (an item: its card, with its key, state, steps, description and activity in words, which `--json` carries as `item`), and the short rules an agent's hook prints at session start, with the block for its repository: in one whose GitHub `origin` owner is `gh config get -h github.com user` (any case, every push address too, read in at most 300 ms or not own; a submodule never, a worktree as its repository; the hooks run it in the agent's directory) the standing way (commit through /commit, push, a PR on this repo, rebase-and-merge only its own PR, install, follow the deploy read only, clean up its own worktree and branch, close its tab), else "not your repo: ask before commit and beyond"; both name what always asks (`--turn`: the one line it adds to each request, the repo part in a few words) | no |
| `qoop task board [--json] [--dir DIR]` | the board's items first (key, title, Todo, Doing or Done, steps ticked of all, the agents on it with their machine; Done ones for a day, dropped ones never), then every space with its goal and its open tasks (name, goal, stage and since when, plan, newest evidence, done claim) and the last tasks closed verified or failed in the past day with their closing note, as Markdown, read from the record; `--dir` writes `README.md` and one `space-<id>.md` per space there, removing only its own files of spaces that are gone. qass's own space is left out. `--json` prints the items alone | no |
| `qoop task board --page` | the board as a page (needs a terminal): three lanes, Todo, Doing and Done, of issue cards (space, title, key, steps ticked of all, the agent on it and its stage; a Done card says only done or failed), newest activity first in each lane, with how long it has been in its lane and how long its agent has worked. `v`, or a click on `[ full · quiet ]` at the top right, turns the cards quiet (one line, a word only on "waits on you" and "qass checks") and back; `board.cards` in config.toml keeps the choice. `h j k l` and the arrows move, enter opens the issue (Description, Checklist with `x` to tick, Activity, Details), `c` adds one to Todo in the selected card's space (`tab` another space), `H` and `L` move a card a lane (Todo to Doing records a start and types `start <KEY>` into qass's terminal; Doing to Done closes it verified; Doing to Todo takes its agents off it; Done to Todo reopens it), `/` filters by key, title or space, `t` types a line into qass's terminal, `q` leaves. Read again when the daemon says a task changed, and every half minute for what time alone changes. `qoop qass` shows it above qass in qass's tab (a pane named `board`, the tab's first); a wake of that tab, from anywhere, types the page into it again, and `qoop qass` and `ctrl-a Q` put it back when the tab is without it. A daemon from before the board is said to need a restart | no |
| `qoop task begin` | what an agent's hook runs when the agent edits or a turn changed the repo: the terminal's open task, opened now and named after the newest request when there is none | no |
| `qoop ask add E<n> --agent <T52\|ubox/T2\|-> --handle <word> --urgency now\|later` | put a question card in the owner's inbox, as qass writes it: the card is `key: value` lines on standard input (`doing`, `why`, `asks`, two to four options `A:` to `D:` as `label \| reason \| words`, optional `asked` and `pick`), checked strictly (200 characters a value), refused with every problem and the format, exit 2, stored nowhere. Only from a terminal in qass's space. Exit 3 when the daemon cannot store it (older, or not running) | no |
| `qoop ask close E<n> [--note <words>]` | end a card that was handled (answered in the chat, by qass, or gone); only from qass's space | no |
| `qoop ask answer E<n> [--option a\|b\|c\|d] [--seen <its words>] [--text <words>]` / `qoop ask dismiss E<n>` | answer or put away a card as the owner (never from a terminal): the answer is kept (`queued`) and never refused: the option's words are typed to the worker when it is the same conversation and at rest, otherwise `<handle> <letter>` is typed to qass once it is idle and nobody is typing in it; the card then reads `answered` or `passed`, and a send that fails gives it back as `open` with the reason | no |
| `qoop ask ls [--json]` | the cards waiting (oldest first) and the last five that ended | no |
| `qoop doctor [--json] [--new <file>]` | whether the board, the question inbox and the agents tell one truth, read-only: one line per thing that does not hold (I1 an item closed in the last day still has a task that claimed done or whose tab is gone or asleep; I4 an agent at rest waiting on the owner with no live card; I6 a live card whose tab closed, or with no tab while qass has none), nothing when all hold. Exit 0 clean, 1 findings, 3 when a read could not be made, each skipped check named. `--new` prints only findings whose id is not in the file, its exit code counting only those, and keeps the current ids there | no |
| `qoop send <terminal> <text>` | type into another terminal, over the addressed terminal channel; after `--` every word is text, dashes included | no |
| `qoop send <terminal> --key <name>` | press named keys in another terminal, one frame each: `enter esc tab shift-tab backspace space up down left right home end pageup pagedown ctrl-a..ctrl-z`; arrows follow the program's cursor key mode | no |
| `qoop read <terminal> [--lines N] [--json] [--watch]` | print another terminal's current screen; `--json` prints it as styled runs (`{"cols","cursor","behind","alt","rows":[[{"t","fg","bg","b",...}]]}`, colours as hex), `--watch` again on each change, at most about every 150 ms, without sending a size | no |
| `qoop read <terminal> --back UP [--lines N] [--json]` | read the terminal's history instead: N rows (200 by default), the first of them UP rows before the newest one written; `--json` answers `{"from","total","rows"}` | no |
| `qoop prompt <target> [--wait] [--timeout SECS] [--] <text>` | send text plus Enter to another terminal, refused while it is blocked or waiting on a trust prompt. Flags go right after the target; the first other word starts the text, `--` lets the text start with a dash, and a text ending in `--wait` or `--timeout N` is refused as a misplaced flag unless it follows `--`. `--wait` then waits for the turn the prompt caused, with the exit status of `qoop wait`: from a stopped agent it ends on the first stop after the agent took the prompt; from a busy one (which may fold the prompt into its running turn or queue it) it ends once the agent has stayed stopped for 2 s. Blocked counts as stopped, since it needs a person | no |
| `qoop wait [target...] [--until STATE,...] [--match TEXT] [--timeout SECS] [--json]` | block until a target reaches a state (default `idle,done,blocked,failed`; no target means every agent terminal but the caller's), print the matching rows; exits 124 on timeout and 1 when a target closes. A target already there returns at once, so after writing to one use `prompt --wait`. `--match` names one target and waits for TEXT on its live screen (never the scrollback); with `--until` both must hold | no |
| `qoop new [--name N] [--cwd DIR] [--space ID\|NAME] [-- command...]` | open a terminal in the caller's directory, print its number, and type the command into its shell once it has drawn a prompt, quoted so each argument stays one word; nothing is typed after 10 s. `--space` takes an id or a name, and a new name is made once however many callers race for it. Refused when qoop is not running: it never starts one | no |
| `qoop close <terminal>` | end a terminal and everything it started, as `ctrl-a x` does; refused for the terminal the command runs in | no |
| `qoop worktree new <branch> [--base REF] [--path DIR] [--space ID\|NAME] [--name NAME] [-- CMD...]` | add a git worktree at `<parent>/<repo>-wt-<slug>`, where the slug is the branch lowercased with every run of other characters one dash (`feat/Login` becomes `feat-login`), and open a terminal in it named after the branch. A new branch starts from `--base` or HEAD; an existing one is checked out as is, and one that exists only on a remote is checked out tracking it. A branch checked out elsewhere is refused with where | no |
| `qoop worktree ls [--json]` | every worktree of the current repository, and this machine's terminals inside each | no |
| `qoop worktree rm <branch\|path> [--force]` | remove a linked worktree, keeping its branch. Refused while any process of this user has its directory inside, whatever started it, named by pid and by qoop terminal where known; `--force` never overrides that. qoop's own clients do not count; a qoop daemon running from inside is named and refused, because new terminals start in its directory; a caller whose shell is inside is told to run it from outside; if the processes cannot be listed it refuses. A dirty tree is refused without `--force`, and a worktree whose directory is already gone says to run `git worktree prune` | no |
| `qoop history <target> [--last N] [--role user\|assistant] [--json]` | print another agent's recent transcript window from its own product store, local files only | no |
| `qoop history --dir <repo> [--last N] [--json]` | what the person typed to their agents in a git repository and its worktrees (live or deleted), across every Claude Code, Codex and opencode session there, oldest first, the newest N kept (200, at most 2,000); review runs, subagents, headless runs and notices are left out, Gemini is not read. No daemon | no |
| `qoop context <target> [--last N] [--role user\|assistant] [--lines N] [--json]` | understand another agent before writing to it: who it is, whether it is writable, its live screen and its recent transcript window, local only | no |
| `qoop agents [--json]` | every terminal: number, state, program, name, workspace, generation | no |
| `qoop agents --watch [--all] [--json]` | the same list now and again each time it reads differently, woken by the daemon's own news on one connection, never by a timer. It only watches, so it keeps no daemon up; when the daemon leaves it prints the empty list and, on macOS, waits without a timer for the next one to start and follows it, elsewhere it ends. `--all` names the session, `default`, on every row, which is what the notch reads | no |
| `qoop agents explain <target> [--json]` | one terminal: state, the source holding it, the screen rule that last matched and its manifest, and what its screen discarded when anything was | no |
| `qoop focus <terminal> [--json]` | show that terminal in every attached interface, as a click on its row would; `--json` answers `{"clients":[pid,...]}`, the interface processes asked, so a launcher can raise the window one runs in | no |
| `qoop reload-manifests` | re-read the agent-detection overrides without restarting the daemon | no |
| `qoop hooks status` | which agents' hooks are installed, and whether they are current | no |
| `qoop hooks install [agent...]` | write an agent's hook and register it in that agent's own configuration, beside whatever is already there | no |
| `qoop hooks uninstall [agent...]` | take out exactly what was installed | no |
| `qoop skills status` | which agents' skills are installed, and whether they are current | no |
| `qoop skills install [agent...]` | teach an agent what qoop is for, beside whatever skills it already has; with no agent named, only the ones on this machine | no |
| `qoop skills uninstall [agent...]` | take out exactly what was installed | no |
| `qoop avatar` | show the picked avatar, or pick one when on a terminal | no |
| `qoop avatar pick` | choose a terminal pet from a batch: a number keeps one, `r` deals again | no |
| `qoop avatar show [--json]` | print the picked avatar; `--json` prints its pixels, each a `#rrggbb` or `null`, rows top first, or `null` with none picked | no |
| `qoop avatar options --json` | every part a pet may have: hair and eyes by id and title, hair colours by ink, title and `#rrggbb` | no |
| `qoop avatar set [--hair H] [--hair-ink N] [--eyes E]` | change those parts of the pet and keep the rest; only parts the picker offers are taken | no |
| `qoop config [--json]` | the settings in `config.toml` (prefix, theme, notifications, the notch), each from the file or its default, where the file is, and what in it could not be used | no |
| `qoop config set <key> <value>` | write one setting, checked first, keeping every other line of the file; `notch.enabled` is read by the notch when it starts or from its settings window | no |
| `qoop settings` | every setting on one page with the pet: on a terminal, typing searches, tab moves the chip, up and down walk the rows (up from the first reaches the pet), left and right change a value and write it at once, esc clears the search or leaves; piped, prints what `qoop config` prints | no |
| `qoop onboard` | the first start, one step a screen: pet, sounds, light or dark, the command key, notifications, sleep, the agent qass runs on; left and right choose, up and down move between rows, enter goes on, esc finishes; piped, says it needs a terminal | no |
| `qoop qass` | open qass, the assistant over every agent: the running one is shown, one asleep in the `qass` space is woken, otherwise it starts in its own folder (`qoop paths qass-data`) on `[qass] agent` from `config.toml` (claude, codex, gemini, opencode or an `[agents]` name, typed with its flags), carrying on its own last conversation on that agent while the agent still keeps it (Claude: its newest of qass's; the others: the id their hook last reported from qass's folder, kept in `last-session`), or, with none, told to start its qass skill. It lives in the space `qass`, which the sidebar and `ctrl-a s` leave out: `ctrl-a Q`, or a click on the pet in the sidebar foot, goes there and back. Its tab shows the board above it (`qoop task board --page` in a pane of its own, the tab's first; `ctrl-a z` on it is full screen), put back by `qoop qass` and `ctrl-a Q` when the tab is without it. Closing it is `qoop close` | no |
| `qoop qass page` | qass's page, built like `qoop settings`: its state (running, asleep, not started or not set up), the agent it runs on, what it is doing, when it last did anything, wake or start, sleep now, open its tab, and its sleep mode (`sleep.qass_idle_minutes`: never, 15, 30 or 60 minutes idle); up and down move, enter does, `w` `s` `o` are wake, sleep, open, left and right change the sleep mode, `q` or esc leaves; piped, says it needs a terminal | no |
| `qoop qass brief <n> [--json]` | what qass must know before it writes to agent `n` of this Mac, worked out so it does no arithmetic, at most 200 words (cut with `+N more`), times and counts only, never a screen's or a transcript's text: its name, space, state and minutes in it, generation; whether the owner is at that tab (his newest message there, one that follows a write of qass's by at most 5 s being qass's; under 120 s: hands off until a clock time; no transcript or no times: unknown, treat as engaged); qass's writes to it since the owner last spoke to qass (of 3) and in the last hour (of 10), from `prompted.log`; its task, stage and minutes in it, claim, review rounds (changes into `reviewing`) of the limit in the repository's `.qoop/workflow.md` or qass's `init.md` (else 8) and its newest evidence cut to 80 characters; its open card; `repo: own <owner/name>` or `repo: not own` (as `qoop task rules` decides it, from the agent's directory) and the prompt ending it allows (`merge` in an own repository, else `none`); whether it is blocked on a menu; and a last line of what is allowed now and what is not, with why. `ubox/T2` says a remote is read with `on.sh`. `--json` adds the same as fields | no |
| `qoop report draft` | read a note from qass about its own way of working on stdin (`area`, `what`, `cost`, `often`, `fix`, one `field: value` line each), check it (printable ASCII, common English words or qoop's own, numbers only in `often`, none of the person's own names, which are read on this machine and never sent), add the qoop version and qass's agent, keep it in qass's folder under `reports` and print the exact bytes it would send and whether `[report] send` lets it go | no |
| `qoop report list` | the kept notes, each `kept` or `sent` | no |
| `qoop report show <n>` | one kept note, checked again, as it would be sent | no |
| `qoop report rm <n>` | forget a kept note; a sent one is taken off the hub first with `qoop report delete` | no |
| `qoop report send <n> [--yes]` | send one note to the hub this machine signed in to, only as `[report] send` allows: never under `never`; under `ask` with `--yes` (the person said yes to it); under `always` by itself once a first one went after a yes. A machine not signed in keeps the note; a hub from before notes is said. Part of the qoop release | yes |
| `qoop report delete <n>` | take a sent note off the hub by the key it gave; it stays kept here | yes |
| `qoop report inbox` | the notes people sent, for a machine of the hub's owner: the largest 50 groups (an area and a `what` alike), each with its count, its newest day and its newest note, printed as text only | yes |
| `qoop paths [name] [--json]` | where qoop keeps each kind of file (config, data, runtime, cache, logs, qass-config, qass-data, skills) and how big it is; with a name, only that path, for a script or a `--cwd`; `--json` adds `exists` and `bytes` | no |
| `qoop sound` | hear the notification sounds (macOS its own, Linux the freedesktop ones installed) and keep one for a finished agent and one for an agent that waits; on a terminal the arrows move and play, enter keeps, space plays again, `o` turns sound off, `q` or esc stops; piped, a number or a name plays one, an empty line keeps, `off`, `q` or EOF stops | no |
| `qoop notch install` | on macOS 14 or newer, a LaunchAgent that runs `qoop notch run` at login: the agents of every running session in the notch, the pet with a badge for the ones that need you, working and idle counts, and a click for the list | no |
| `qoop notch uninstall` | take the LaunchAgent and the written app out | no |
| `qoop notch status` | whether the notch is installed and running | no |
| `qoop notch run` | write this build's notch app out when missing and run it against this binary | no |
| `qoop ls [--json]` | every space: its id, name, whether it is awake, asleep, partly asleep (`n of m asleep`) or stopped, how many tabs it holds and which are asleep, and its goal when one is set; read from the record when qoop is not running | no |
| `qoop sleep <space> \| --tab <terminal> [--json]` | put one tab to sleep, every pane of its split, or every tab of a space (by id or by a name only one space has): what sleeps ends, its rows are kept, and nothing of it runs until it wakes. A tab sleeps whole or not at all: a command running, an agent working, blocked or not yet telling its conversation (`qoop hooks install`), a job in the background or a wait between tabs keeps it awake, and the answer names the pane and why; a space sleeps each tab that can and lists the ones that stayed awake. With qoop not running it says nothing is awake | no |
| `qoop wake [space] \| --tab <terminal> [--json]` | bring one sleeping tab back, every sleeping tab of a space, or with nothing named the tab that slept last: the same numbers, names, splits, directories and scrollback, each agent resumed into its own conversation by its id; starts qoop when it is not running | no |
| `qoop space goal <id\|name> [text\|-]` | what the owner wants from a space, one line of at most 300 characters: no text prints it, a text sets it (`-` reads stdin), an empty text clears it. Works through the running daemon and never starts one | no |
| `qoop space rm <id\|name>` | remove a space with no tab in it, awake or asleep: refused with how many it holds otherwise, as is a name two spaces share (use the id) and the default space. Works through the running daemon and never starts one; its tabs are never closed for it | no |
| `qoop restart` | move this machine's daemon onto the qoop on disk now, a build of the same version included, in the same process: every tab, running command and working agent keeps running, and it prints how many tabs it carried. It ends no tab it can avoid: when the move cannot be live (a daemon from before it, a binary on disk that cannot take tabs over, a tab that cannot be carried) it moves nothing, says why and what a `qoop stop` would interrupt, and exits 1; a tab the new binary then fails to take over is brought back, agent resumed, and named, with exit 1. Open windows close with the move. With qoop not running it says so | no |
| `qoop stop` | end qoop now: every terminal ends and what was awake comes back on the next start; a tab that was asleep stays asleep | no |
| `qoop pair [--hub URL]` | pair a phone: print a QR code and a link, then take the six digit code the phone shows; the machine key and the bridge service appear only once a phone is paired | yes |
| `qoop login [--hub URL]` | link this machine to an account: print a code and a link, sign in there, answer `y` here; the machine then shows on qoop.si/app and on the account's phones, and every saved remote joins it | yes |
| `qoop logout [--hub URL]` | unlink this machine from its account; its key, paired phones and bridge stay | yes |
| `qoop devices ls` | the phones paired with this machine, with their key fingerprints | no |
| `qoop devices rm <name>` | unpair a phone, by its name or its key fingerprint; the running bridge closes its open streams at once | no |
| `qoop bridge` | the phones' end of this machine, in the foreground: one outbound connection to the hub, piping each paired phone to this machine's qoop while it runs; `qoop bridge reload` puts the installed bridge service back on the binary now on disk (nothing when there is none), after a replaced binary | yes |
| `qoop update [--check]` | ask qoop.si for the newest release for this machine (macOS or Linux, arm64 or x86_64) and replace this one with it, then ask this machine's daemon and each saved remote's to move onto it (a working agent or a running command is never ended: a daemon that can hand its tabs over keeps them running and waits for nothing, any other moves once nothing there is working, then ends idle tabs and restores them, agents resumed in their conversations); `--check` only notes what is newest; a release build's daemon also runs it as `--auto` every six hours unless `no-auto-update` is in the config directory ([docs/update.md](update.md#updating-on-its-own)) | yes |
| `qoop --handoff-formats` | the handover layout this build takes over from, which a daemon about to move onto it asks first; a build from before it fails the question | no |
| `qoop --hello` | what a remote bootstrap asks for: version, codec generation, platform | no |
| `qoop --help` | this surface | no |
| `qoop --version` | the version | no |

## Options

| Option | What it does |
|---|---|
| `--session default` | accepted and ignored: it is what an older qoop on the other end of ssh sends. Named sessions were replaced by spaces, so any other name, as the flag or as `QOOP_SESSION`, is refused with where that session's files are and how to use them instead. |

`send`, `prompt` and `close` take `--generation G`, the number `qoop agents` prints: the
write is refused unless the terminal still runs that generation, so an answer meant for one
process never reaches the one that replaced it.

How long an idle tab waits before it sleeps by itself, and whether qoop leaves when
nothing is awake, are settings in `config.toml`, not options: [config.md](config.md).

A word that is not a subcommand is an error naming it, not the interface: `qoop stauts`
says so and exits 1. Every error prints as one `qoop: ...` line on stderr and exits
nonzero.

The phone and account commands (pair, devices, bridge, login and logout) are part of the
release, not of this repository: a build from source says it has no phone support for them,
and a bridge reload does nothing.

The rows that say yes are each the point of their row: `qoop update` reaches qoop.si for the
releases, and `qoop pair`, `qoop bridge`, `qoop login` and `qoop logout` reach the hub, and
only once a person has asked for a phone or an account; `qoop report send`, `delete` and
`inbox` reach the hub this machine signed in to, and only when asked for one note. Nothing else touches the network: no control plane, no
background version check, and a machine that never pairs a phone never opens a
connection to a hub. Anything else that ever needs the network is an addition to this
table with a yes, plus a row in the gate saying what happens when it is unreachable.

The gate skips the rows that say yes: with the network blackholed `qoop update` fails
closed with "cannot reach the releases" and `qoop pair` fails naming the hub it could
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
