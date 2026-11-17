# herdr plugins

qoop runs herdr TUI plugins unchanged. A plugin written for herdr 0.9.5 (a
`herdr-plugin.toml` and the programs it names) is installed with `qoop plugin install`, its
actions show in the palette, and its programs talk to qoop through a `herdr` command and a
socket that answer the way herdr's do. qoop does not run herdr and never connects to one:
nothing here reaches a herdr of your own, and a plugin never learns that it is not in herdr.

This is for plugins that work on terminals, tabs and spaces (rename a tab, open a review pane,
type a prompt into an agent). It is not a way to change how qoop looks, and it covers only
the part of herdr's plugin interface that the plugins we checked use. The list of what does
not work is at the end of this page and is meant to be read before installing something.

## Commands

```text
qoop plugin install owner/repo[/subdir] [--ref REF] [--yes]
qoop plugin link <dir> [--yes]
qoop plugin ls [--json]
qoop plugin rm <id>
qoop plugin run <id.action>
```

`install` takes `owner/repo`, always from `https://github.com`, with an optional folder inside
the repository. It clones the default branch, or `--ref` (a ref that starts with `-` is
refused), and reads `herdr-plugin.toml`. Before anything of the plugin runs it prints what it
would run (see "What your yes covers") and asks `install it? [y/N]`. `--yes` answers for you,
and is the only way to say yes when there is no terminal; the preview is printed either way.
Then the checkout you were shown is moved into place, never fetched again, and the build
steps run once, in the plugin's folder, with stdin closed and qoop's and herdr's own
environment variables taken out. A build that fails records no consent, and the plugin does not
run; `qoop plugin rm <id>` clears what it left.

A plugin whose id is already installed is refused until you remove it, so nothing is replaced
behind a preview. A manifest that herdr 0.9.5 would refuse is refused here too: it needs a
`name`, a `version` and a `min_herdr_version` no newer than 0.9.5, titles on its actions and
panes, ids in herdr's character set, and platforms among `linux`, `macos` and `windows`. A
plugin that does not name this machine's platform is refused.

`link` uses a folder as it is, for a plugin you are writing or keep elsewhere. The preview is
the same, no build runs, no commit is recorded, and a folder inside qoop's own plugins folder
is refused. `rm` takes the consent away first, then the folder (a linked folder is unlinked
and left as it is), the plugin's state and its startup commands. Its config folder stays, and
its path is printed; a plugin installed again under the same id is handed it.

`ls` lists each plugin with its version, the commit recorded (`none (linked)` for a link) and
whether it still matches its consent: `ok`, or the reason it would be refused, such as a
changed manifest or `broken link to <dir>`. `--json` prints the same as one array.

`run <id.action>` runs one action of a plugin, its consent checked first. When the plugin host
is running it goes through the host, with the focused terminal as its context, and prints where
the output goes (`host.log`, below). Otherwise it runs in the foreground, here, with no
context. Actions also run from the palette.

Installing or linking the first plugin starts the plugin host, and so does every interface
start while a plugin is installed. `qoop plugin host` and `qoop plugin pane-exec <nonce>` are
what qoop runs for itself and are not for typing.

## What your yes covers

The preview prints, one line each and with every argument quoted and every control character
escaped (so a manifest cannot hide one): the plugin's id and version, where it came from, the
commit, every `[[build]]` command, every action, pane, event and startup command, every link
handler as `link <id> "<pattern>" -> <action>`, and then this, word for word:

```text
It runs as you, can read every terminal and the command lines of their processes, can type
into terminals and run commands in shells, can close panes, tabs and spaces, rename them,
move focus, open panes and show notifications.
```

(It is one line on screen.) What the yes holds, and what it does not:

- It covers the manifest text. The text is kept whole in `<data>/plugins/consent.json` and
  compared byte for byte before every run, handler, startup, pane open and host start. A
  changed manifest, a missing entry or a missing ledger refuses, and `qoop plugin ls` says
  which. To see what a changed plugin would run now, remove it and install it again.
- It records the commit and does not verify it. The commit is shown and kept so you can read it
  later. Nothing checks again that the files on disk are that commit.
- It does not pin the files the commands run. A manifest that runs `./bin/tool` has your yes for
  that line, not for what `bin/tool` holds today or tomorrow. The plugin runs as you and could
  rewrite its own files anyway, so pinning them would say more than it means.
- A ledger that does not parse is set aside as `consent.json.bad-<time>` and replaced by an
  empty one, which trusts nothing: every plugin asks again.

## What a plugin can do

A plugin runs as you, with your file permissions, and through qoop it can:

- read every terminal on this machine's daemon, and the command line, working folder and
  process group of what runs in it;
- type into terminals (`send-text`, `send-input`, `send-keys`, `pane run`, `agent prompt`);
- close panes, tabs and spaces;
- rename them, and move focus (to a named pane, or to the neighbour on a side with
  `pane focus --direction`);
- swap and resize panes in a split tab, and move a pane into another tab or into a tab or
  space of its own;
- open panes, tabs and spaces, and start a plugin's own panes;
- show a desktop notification, which follows `[notify] mute`.

Writes go through the same refusals as `qoop send` and `qoop prompt`. While a person is at a
terminal (their keys from the interface, the phone, the browser or `qoop attach`), a plugin's
write is an error for the plugin, naming the second it frees. A blocked agent, a trust prompt
and a sleeping tab refuse the same way. The plugin gets the error; there is no retry.

While a pane a plugin opened still runs that plugin's program, the plugin's writes into it are
never held by a person there: a sidebar closes itself with `ctrl+q` right after you typed in it.
Once something else runs there (a shell, after qoop restarted), it is a terminal like any other.
The plugin's writes into any other terminal follow the rules above and below.

One more exception: a key a person presses in a plugin's own pane lets that plugin send text and
keys (not `agent prompt`) into the terminals of the pane's tab for the next 10 seconds, until
its first write that ends in Enter.
It passes only the hold that a line the person sent puts on a terminal (120 seconds); a draft
they are typing there, a trust prompt, a blocked agent and a sleeping tab still refuse. Hooks,
startup commands and actions started from the palette never get it.

A plugin cannot report an agent's state: `pane.report_agent` is an unknown method here, and
`agent report` is not a command. It never gets a terminal's token, because qoop strips its own
variables before a plugin's program starts. An agent that a plugin starts inside one of its
panes therefore has no hook authority: its hook reports are rejected and its state comes from
its screen only.

The plugin host is a client of the daemon that never keeps it up. It is started when a plugin
is installed and an interface starts or an install runs, it leaves with the daemon and when no
plugin is left, and it is never restarted in a loop. A plugin's `[[startup]]` commands run
when the host starts. Each runs in its own process group, which is ended when the host
leaves, on `qoop plugin rm`, and at the next host start (for a host that was killed). A startup
that moves itself into its own session, a daemon, is out of that group and keeps running, as it
does under herdr. An `[[events]]` command
is stopped by process group after ten minutes, and at most 32 run at once across all plugins;
the rest wait. Output of actions, events and startups goes to
`<data>/plugins/.state/<id>/host.log`, which is moved to `host.log.1` past 1 MiB.

## The palette

`ctrl-a space` ends with a plugins section: each installed plugin's actions, by title with the
plugin's id beside it, that fit what the palette was opened over. An action with no `contexts`
or `global` is always listed; `workspace`, `tab` and `pane` actions are listed when a terminal
of this machine is on screen; a `selection` action is never listed. Picking one says
`starting <title> (<id>)` at once, then `started ...`, or `did not start (see qoop plugin ls)`
with the host's reason.

The text you selected with the mouse in the focused pane, while it is still shown, goes to the
action as `selected_text` in its context, as herdr hands a key-bound action the selection. The
host cuts any `selected_text`, on a character, to 16 KiB as written in
`HERDR_PLUGIN_CONTEXT_JSON`, so the variable stays far under the 128 KiB a Linux environment
variable may hold. A selection made in copy mode is not passed.

The interface tells the host which terminal it shows, so an action gets that terminal as its
focused pane, tab and space. With plugins installed but no host running, one row says
`plugin host not running`; it is not a search hit for other words, and picking it says to quit
and run qoop again: the host is started when the interface starts, never from the palette.

A terminal of another machine (shown through an ssh relay) is not reported: the host acts only
on this machine's daemon. A global action run while a remote terminal is on screen gets the
last local terminal the interface reported as its focus.

## What the plugin sees

The host serves herdr's newline JSON on `herdr.sock`, in the session's runtime folder (owner
only, the peer's user checked). Plugins reach it through the `herdr` command, which is qoop
under another name, in `<data>/plugins/.bin`, first on a plugin's `PATH` and its
`HERDR_BIN_PATH`. That folder is never put on your own `PATH`, and only that file acts as the
shim: a `herdr` anywhere else is untouched. The socket's path reaches a plugin as
`HERDR_SOCKET_PATH`, and only the shim reads that variable. Nothing else in qoop does, so
nothing qoop runs for a plugin can end up at a real herdr, whatever your environment holds.

Ids follow herdr's shapes: a space is `w<N>` (qoop's space N), a pane `w<N>:p<T>` (terminal T),
a tab `w<N>:t<K>`. A tab keeps its id while any terminal of its splits remains. Each pane has
a revision that rises on every change and never resets. The host answers `ping` and
`herdr --version` as `0.9.5`, and these methods, the ones the plugins we checked call: `pane`
list, get, current, read, layout, split, close, focus, send-text, send-input, process-info,
rename, zoom, resize, swap, move and report-metadata; `tab` list, get, create, rename, focus
and close; `workspace` list, get, create, focus, rename, close and report-metadata; `agent`
list, get, focus, prompt and rename, and `agent.view.set` and `agent.view.clear` on the socket;
`session snapshot` and `session list`; `plugin` list, action list,
action invoke, pane open and close, config-dir and popup close; `notification show`;
`server reload-config`; and `events.subscribe`. Anything else gets herdr's own
`invalid_request` ("unknown variant") error, which plugins use to fall back.

Agent states map one to one for working, blocked, idle and done; failed and starting read as
`unknown`.

Events, as a subscriber sees them and as an `[[events]]` command is told in
`HERDR_PLUGIN_EVENT_JSON`: `workspace` created, renamed, moved, closed and focused; `tab`
created, renamed, moved, closed and focused; `pane` created, moved, focused, agent detected,
agent status changed, and closed or exited; and `layout.updated`. The host reads the daemon's
listings on its notices and every two seconds, so an event can arrive up to two seconds late.
A pane that goes away because the host closed it for a plugin is `pane_closed`; one that goes
away any other way, including you closing it, is `pane_exited`. Never both.

`pane move` keeps the pane's terminal, and so what runs in it, as herdr does. Into a tab
(`--tab T --split right|down`, beside `--target-pane` or that tab's focused pane, the target
keeping `--ratio` of the room, 0.1 to 0.9): it leaves its split, whose other panes stay as they
were, and joins the target's. `--new-tab` makes it a tab of its own at the end of its space or
of `--workspace`; `--new-workspace` puts it alone in a new space, which `--label` names and whose
tab `--tab-label` names. A move within one tab, or out of or into a tab zoomed by a plugin, is
answered as not made (`same_tab`, `zoomed_tab`), as in herdr. A sleeping tab is `tab_not_found`
and a sleeping space `pane_move_failed`, before anything moves. A tab left empty closes, and so
does a space left empty by a move to another one, unless it is the session's default space or
qass's. Events come in herdr's order: tab and space closed, space and tab created, `pane_moved`
(naming what it closed and made), then the layouts. If joining the target's split fails after
the pane left its own, the pane stays a tab of its own in the target's space and the move is an
error. A pane alone in its tab moved to a new tab in its own space keeps its tab and goes to the
end.

`agent.view.set` and `agent.view.clear` are checked as herdr checks them (a source of letters,
digits and `:._-` up to 120, a label up to 32, filters up to 8 deep and 64 nodes, `in` up to 32
values, up to 8 sort fields; a `plugin:<id>` source needs that plugin installed and agreed to)
and answered as herdr answers them. There is one view at a time; `clear` with a source clears
only that source's, without one any. A view a plugin owns is dropped once the plugin is removed.
While one is set, the agent rows in the sidebar's spaces tree (and in the folded column) follow
it: each space keeps its place and its rule, its agents are listed in the view's order (herdr's
sort keys: space, tab and pane order, attention, status, agent, seen, status change count and
the plugin's tokens), and the ones its filter leaves out are not listed. An agent the view does
not know yet (one started since the host was last asked, or another machine's) is listed after
them, in its own place. Nothing else on the sidebar changes: no header, label or mark, and the
inbox, the search and its counts read every agent. The interface asks the host every three
seconds and when the agents change, never more than twice a second, and never starts the host
for it. Clearing the view, removing its plugin or the host leaving puts
the rows back in their own order. `agent list` is unaffected, as in herdr.

## Links

A ctrl+click on a link in a pane goes to a plugin's `[[link_handlers]]` first, as in herdr. The
link is the target a program gave that cell with `OSC 8` (any scheme), else an `http://` or
`https://` address in the row's text under the pointer (its trailing punctuation and an
unmatched closing bracket left out). The host tries the plugins agreed to by id, each one's
handlers that run on this platform in manifest order, and runs the action of the first whose
`pattern` (a regular expression, unanchored) matches, with the clicked pane as its context,
`invocation_source` `link_click`, and `HERDR_PLUGIN_CLICKED_URL` and
`HERDR_PLUGIN_LINK_HANDLER_ID` set. A pattern that does not compile refuses the manifest, as
in herdr.

A ctrl+click is held only while a plugin here has a link handler for this platform (the
interface learns that with the agent view) and the host is listening, and only on a link in a
pane with no box or mode open; anywhere else it goes on at once, exactly as before. While the
host is asked, at most half a second, the press is held and everything read after it (keys,
other clicks) waits behind it in order. A handler that takes the link takes that click's press,
drags and release and nothing else; one that cannot start says why on the notice row and takes
them too. A link no handler takes, or no answer within the half second: the press is replayed
as it came, then what waited behind it, and it goes where a click goes (a selection, a focus,
or the program's own mouse report). An answer after the half second is too late for the click,
though a handler that matched has run. qoop never opens a link in the browser itself; the
terminal around qoop does that, as before.

The terminal around qoop has to send ctrl+click to it as a left click with control held. iTerm2
does when "Ctrl-click reported to apps, does not open menu" is on (Settings, Pointer); otherwise
it opens its menu. macOS reads ctrl+click as a secondary click, and Terminal does not document
reporting it to programs, so there a ctrl+click may open the menu and never reach a handler.

## Plugin panes

`plugin pane open` places a pane as `tab` (a new tab), `split` (beside a target pane) or
`zoomed`, `popup` and `overlay`. The last three open as a split beside the target, take focus
and are zoomed to fill the tab: the host asks the interface to zoom them, and the zoom goes back
when they close. qoop draws no floating pane, so width and height are ignored. A popup or
overlay hands focus back to the pane it was opened over when it closes, and there is one popup
at a time, as in herdr.

A plugin's program is not typed into the shell. The host keeps its argv, environment and
working folder in memory under a random one-time nonce for 30 seconds and types only a
constant `exec '<qoop>' plugin pane-exec <nonce>` line. `pane-exec` takes the spec once,
removes qoop's own variables, sets the environment and `exec`s the program. The program's
arguments and values therefore never reach the screen or the scrollback; the constant line
itself can land in shell history. A missing, used or expired nonce exits with a message.

## Not in this version

These do not work, and a plugin that needs one will install and then not do its main job:

- no floating popups: a popup or overlay opens as a split beside its target, zoomed to fill the
  tab, and the zoom goes back when it closes;
- no `layout.*`;
- a link handler runs only from a ctrl+click the terminal around qoop reports (see Links), and
  never from a link in a terminal of another machine; no `pane.link.*` methods;
- no worktree events, and no `pane.output_matched`, `pane.scroll_changed`,
  `pane.output_changed`, `workspace.updated` or `workspace.reordered` events: a subscription to
  one is accepted and never fires;
- `pane read --source recent-unwrapped` joins a row that fills the pane's width to the next
  one, since qoop does not keep which rows wrapped: a line exactly as wide as the pane is joined
  by mistake;
- no `agent start`, `agent report`, `events.wait`, `herdr integration` or
  `herdr machine`: unknown methods or commands;
- an agent view orders and filters the spaces tree's agent rows only: its label is not drawn,
  and the priority list and the inbox keep their own order;
- `pane report-metadata` and `workspace report-metadata` keep their tokens (per pane or space,
  with `--seq` ordering and `--ttl-ms` expiry) and give them back as `tokens` in pane, agent and
  space lists, but the qoop interface does not draw them, and a pane's title, display agent and
  state labels are accepted and left unused; the tokens live in the plugin host, so a host that
  restarts starts without them;
- `server reload-config` succeeds and changes nothing, and no `session.json` is written;
- a tab's name is the qoop name of its first terminal, which only `tab rename` sets; `pane
  rename` and `agent rename` keep a pane label and an agent name for plugins (kept across a
  restart of the plugin host), which qoop itself does not show; a tab with no name is labelled by
  its number, as in herdr;
- after a daemon restart a plugin pane comes back as a plain shell in its folder: the restore
  opens a fresh shell and types a resume line only for a known agent, and the plugin's program
  and environment are not kept (a plugin program that was a known agent is resumed as that
  agent, without the plugin's environment);
- while a plugin pane runs, sleeping its tab is refused, naming the program (`<name> is
  running in it`), as for any program in front that is neither a shell nor a known agent;
- nushell panes are not supported;
- an agent started inside a plugin pane has no hook authority (above);
- a global action run with a remote terminal on screen uses the local focus (above);
- key bindings for plugin actions: they run from the palette and `qoop plugin run`;
- Windows.

Desktop mods are a different thing and are not covered. A plugin's `herdr` calls reach only
this machine's daemon.

## Where it lives

```text
<data>/plugins/
  consent.json            what you agreed to, one entry per plugin
  <id>/                   the checkout, or a link to the folder you linked
  .bin/herdr              the shim
  .state/<id>/            what the host keeps for it: host.log
  .state/.host-pgids      the startup commands the host started, so rm and a leaving host end them
<config>/plugins/
  <id>/                   HERDR_PLUGIN_CONFIG_DIR: the plugin's own settings, kept by rm
  .herdr-config.toml      the file HERDR_CONFIG_PATH points at; qoop never reads it
```

`<data>` is `$XDG_DATA_HOME/qoop`, else `~/Library/Application Support/qoop` on macOS and
`~/.local/share/qoop` on Linux. `<config>` is the folder `config.toml` is in
(`~/.config/qoop`). Nothing in `config.toml` is read for plugins except `[notify] mute`; see
[config.toml](config.md).

## Tested plugins

On 2026-11-15, 15 plugins were installed from their pinned commits on an isolated stack and
each one's main action was run, with its visible effect read back (a pane opened with the right
content, a tab renamed, a prompt delivered, a file written). Pass means that effect happened.
10 passed, 2 partly, 2 failed, and 1 was left out.

| Plugin | Commit | Result | What happened |
|---|---|---|---|
| `persiyanov/herdr-reviewr` | `aede1794` | pass | opened beside the agent tab, showed the uncommitted diff, and sent a comment into the agent's terminal |
| `smarzban/herdr-file-viewer` | `d17a068e` | pass | opened a file pane rooted at the focused pane's folder and showed a file's diff |
| `furkankly/zoetrope` | `b1f31dd2` | pass | read the focused pane's agent session and drew its session graph (needed its `zoe` CLI built first, and a session id reported by the agent's hook) |
| `cloudmanic/herdr-plus` | `f38df357` | pass | opened its project picker and built a space with two tabs from a template |
| `kryptamine/herdr-auto-title` | `67c03f3a` | pass | its title daemon renamed both tabs within 8 s |
| `qu8n/herdr-automatic-rename` | `a9975434` | pass | renamed the first tab at startup and a new tab within 5 s |
| `ntindle/herdr-resurrect` | `dab5bed6` | pass | saved a snapshot of the space and tabs, and restore read it back |
| `jeffarese/herdr-bar` | `598395f6` | pass | opened its popup, filtered, and switched to the chosen tab |
| `alexarthurs/herdr-sidebar` | `081ed133` | pass | docked one sidebar per tab, as a split and not full height (no pane move) |
| `dcolinmorgan/herdr-remote` | `051f6e6d` | pass | its status hook sent the expected datagrams; the relay daemon itself was not run |
| `plannotator/herdr-annotate` | `29643fb4` | partial | the overlay opened and saved a comment, but it took the clipboard as the selection: qoop passes no selected text |
| `levi-qiao/herdr-agent-usage` | `af2ac5e3` | partial | its tokens were set and read back and its settings popup opened; the interface does not draw tokens, and `agent view set` is unknown |
| `nikok6/herdr-mirror` | `195398c1` | fail | it mirrors a remote herdr server over ssh, which needs a herdr server and `pane swap`, `layout` and `agent start` |
| `ChmaraX/herdr-nvim` | `50b87a62` | fail | its sidebar needs `pane move` and link handlers, which are not served |
| `AltanS/collie` | `88276e3c` | left out | its build output is killed by macOS at its first run on the test Mac, which is not qoop's doing; its `start` installs a service and was never run |

The run found one bug in qoop, a popup handing focus back too early after the plugin had moved
it, and it is fixed.

## Design

The boundary, the consent ledger and what a plugin never gets are in
[ADR-018](adr/ADR-018-plugin-isolation.md). Every command is in
[the command list](cli-surface.md).
