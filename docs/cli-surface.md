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
| `qoro setup show <file>` | what a Setup will run, its content hash and its reproducibility level | no |
| `qoro setup trust <file>` | confirm a Setup on this machine | no |
| `qoro setup check <file>` | whether a Setup may run here | no |
| `qoro hook-report --state <state>` | what an agent's own hook script calls to report a state change, from inside a qoro terminal | no |
| `qoro send <terminal> <text>` | type into another terminal's session, over the addressed terminal channel | no |
| `qoro read <terminal>` | print another terminal's current screen | no |
| `qoro prompt <target> <text>` | send text plus Enter to another session, refused while it is blocked | no |
| `qoro reload-manifests` | re-read the agent-detection overrides without restarting the daemon | no |
| `qoro ls` | every session, whether it is running, and how many tabs it holds | no |
| `qoro stop [name]` | end a session: its daemon and every terminal in it. The tabs come back on the next start | no |
| `qoro --hello` | what a remote bootstrap asks for: version, codec generation, platform | no |
| `qoro --help` | this surface | no |
| `qoro --version` | the version | no |

## Options

| Option | What it does |
|---|---|
| `--session <name>` | which session to work in. Sessions do not see each other: each is its own daemon, its own socket and its own record of what is open. `QORO_SESSION` does the same for a shell that would rather set it once, and the flag wins over it. Saying nothing is the `default` session. |

The flag may be written anywhere in the command, so `qoro --session work send 2 "x"` and
`qoro send --session work 2 "x"` are the same thing. It is put into the environment of
everything the session starts, which is what lets a shell inside a session say `qoro send`
and reach its own daemon rather than the default one.

Every row says no, and that is the design rather than a coincidence: there is no control
plane in the product, and if one is ever added it is an addition to this table with a yes,
plus a row in the gate saying what happens when it is unreachable.

## What "the same result" means

Byte for byte on stdout, and the same exit status. Not "still works": a subcommand that
printed a warning about an unreachable host when the host is unreachable would be a
subcommand whose output depends on the network, which is the thing being ruled out.

## Adding a subcommand

Add it here in the same change. The gate enumerates this file, so a subcommand that is not
here is a subcommand nobody checks, and a row here with no subcommand fails the gate too.
