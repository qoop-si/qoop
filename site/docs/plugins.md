# herdr plugins

qoop runs herdr plugins unchanged. A plugin written for herdr (a `herdr-plugin.toml` and the
programs it names) installs with one command, its actions show up in the palette, and its
programs talk to qoop through a `herdr` command that answers the way herdr's does. qoop does
not run herdr, and nothing here ever reaches a herdr of your own.

Plugins work on terminals, tabs and spaces: rename a tab, open a review pane, type a prompt
into an agent. They do not change how qoop looks, and they cover the parts of herdr's plugin
interface that the plugins we tried use. The limits are at the end of this page.

## Install and manage

| Command | What it does |
|---|---|
| `qoop plugin install owner/repo` | fetch a plugin from GitHub, show what it runs, build it once you agree. `owner/repo/folder` for a plugin inside a repository, `--ref` for a branch, tag or commit |
| `qoop plugin link <dir>` | use a plugin from a folder as it is, for one you are writing; no build runs |
| `qoop plugin ls` | the plugins here, their version, the commit recorded and whether each still matches what you agreed to |
| `qoop plugin rm <id>` | remove one; its settings folder stays and its path is printed |
| `qoop plugin run <id.action>` | run one of its actions from the shell |

`install` and `link` print every command the plugin would run before anything runs, then ask.
`--yes` answers for you when there is no terminal; the list is printed either way. A plugin that
is already installed has to be removed before it is installed again.

## What your yes covers

The list names the plugin's version, the commit, every build step, action, pane, event and
startup command, and what a plugin can do. Your yes covers the plugin's manifest, the text of
`herdr-plugin.toml`. qoop keeps it and compares it with the file on disk before anything of the
plugin runs. If it changed, the plugin is refused until you remove it and install it again, and
`qoop plugin ls` says so.

It does not cover the files the commands run. A manifest that runs `./bin/tool` has your yes for
that line, not for what `bin/tool` holds later. The commit is shown and kept, not checked again.
A plugin runs as you; treat an install like running a script someone sent you.

## What a plugin can do

A plugin runs as you and can read every terminal on this computer and the command lines of
what runs in them. It can type into terminals and run commands in shells, close panes, tabs and
spaces, rename them, move focus, open panes and show notifications.

Typing goes through the same refusals as `qoop send` and `qoop prompt`. While you are at a
terminal, or an agent is blocked, a plugin's write fails and the plugin gets the error. A plugin
cannot tell qoop what state an agent is in, and never gets a terminal's token, so an agent it
starts inside one of its panes is read from its screen only.

## In the palette

`ctrl-a space` ends with a plugins section: each plugin's actions, by title, that fit what you
have open. Pick one and the hint row says it is starting, then that it started or why it did not.
The plugin host, the small process that serves plugins, starts when qoop does once a plugin is
installed. If it is not running, one row says so; quit and run `qoop` again. An action's output
goes to `host.log` in the plugin's state folder (`qoop plugin run` prints the path).

## Limits

These do not work, so a plugin that needs one installs and then does not do its main job:

- no floating popups: a popup or overlay opens as a split beside its pane;
- no layout changes, moving or swapping panes, or resizing;
- no link handlers, and no worktree events;
- what a plugin reports about a pane (its metadata tokens, such as quota marks) is kept for the
  plugin to read, but qoop does not draw it;
- a pane, an agent and a tab share one name, so renaming one renames all three;
- after the daemon restarts, a plugin's pane comes back as a plain shell;
- a tab with a plugin's pane running will not go to sleep, like a tab running any command;
- nushell panes are not supported;
- plugins act on this computer's terminals, not on another computer shown through ssh;
- plugins have no key bindings of their own: they run from the palette and `qoop plugin run`.

The full list, where plugins live on disk and the exact consent rules are in `docs/plugins.md` in
the qoop repository.
