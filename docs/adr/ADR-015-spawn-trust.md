# ADR-015: Spawn trust, Setup as untrusted input, and hook authentication

Status: accepted
Date: 2026-09-09

## Context

A Setup is a portable artifact. That is the whole point of it: capture on a laptop, run on
a server, share a template with a colleague. The engineering pack calls it "safe to sync"
on the grounds that it excludes secret values.

Excluding secrets does not make it safe to execute. A Setup names an executable, argv, a
working directory and environment overrides. Environment overrides alone are sufficient:
`LD_PRELOAD`, `DYLD_INSERT_LIBRARIES` or a rewritten `PATH` turn a Setup into arbitrary
code execution without touching the executable field at all. The plan's only Setup gate
scanned for secrets leaking outward. Nothing checked anything on the way in.

Two adjacent holes make it worse. `executable: PathBuf` had no stated resolution rule, so a
bare name resolved against the daemon's inherited `PATH` at spawn time runs a different
binary on every machine. And ADR-008 accepts observations from "official hook or plugin
events" with no stated way to authenticate the caller. If a
hook identifies its pane through an environment variable, any process that can
read or guess that value can forge state for any terminal.

## Decision

### Confirmation is decided by a local ledger, never by a field in the artifact

The daemon keeps a **local confirmation ledger** of content hashes it has been told to
trust. It is the file `confirmed-setups` in the runtime base directory, the parent of every
session's directory, so one confirmation holds for every session on the machine, with the
ownership and permission checks ADR-012 applies to that directory. It is never inside a
Setup, never synced, and never written by anything but an explicit confirmation. Where the
runtime directory is cleared on reboot (`XDG_RUNTIME_DIR` on Linux), so are the
confirmations, which fails closed like every other absence below.

Its integrity requirement is one-directional and therefore cheap: **absence fails closed**.
A deleted, corrupted or unreadable ledger means nothing is trusted and every Setup prompts
again, which is annoying and safe. There is no signature and no tamper detection, because a
same-uid process that can write the ledger can also just run the command directly (ADR-012),
so a signature would protect against nothing while implying it protected against something.

The hash is computed by the daemon over the artifact's canonical bytes. It is not a field
in the Setup: a hash the artifact carries about itself is a field of the artifact, which is
exactly the input this mechanism exists to distrust. Any Setup whose content hash is not in that ledger requires explicit confirmation
before it runs, full stop.

The decision does not read the artifact. An earlier version keyed the prompt on a
"captured on this machine" field inside the Setup, which hands the decision to whoever
writes the file: machine ids are not secret, so a hostile Setup would simply claim local
origin and run with no prompt at all, and every other mechanism in this ADR is downstream
of that check. Origin fields are still recorded and still shown, labelled as an unverified
claim by the artifact, never used as a branch condition.

On a hash that is not in the ledger, the daemon presents the exact things that will be
executed, in full, and requires explicit confirmation:

```text
Setup "payments" has not been confirmed on this machine.
It claims it was captured on "laptop-a1" by "alice". That claim is unverified.

  executable   /opt/homebrew/bin/claude
  argv         ["claude", "--dangerously-skip-permissions"]
  cwd          /Users/me/code/payments
  env override PATH=/opt/custom/bin:$PATH
  env override NODE_OPTIONS=--require /tmp/x.js
  services     pnpm dev
                docker compose -f docker-compose.yml up
```

followed by the question itself, `Run this? [y/N]`, asked by `qoop setup trust`, which
records the confirmation only on `y` or `yes`.

Confirmation is recorded in the ledger against the content hash. Editing the Setup
invalidates it. There is no "trust all setups" switch, because the value of the prompt is
precisely that it is rare enough to read.

**Every** environment override is shown with equal prominence and none is highlighted. A
maintained list of dangerous variables is a denylist, and this ADR argues below that a
denylist cannot work for secrets. It cannot work here either: `BASH_ENV`, `ENV`, `ZDOTDIR`,
`LD_AUDIT`, `PERL5OPT`, `RUBYOPT`, `JAVA_TOOL_OPTIONS`, `PYTHONPATH`, `GIT_CONFIG_GLOBAL`,
`GIT_EXTERNAL_DIFF`, `PROMPT_COMMAND`, `SSH_ASKPASS` and, when a Setup's own commands shell
out, `EDITOR` and `PAGER` are all sufficient. Highlighting a subset actively directs the
reader's attention away from the rest.

Variables the daemon itself injects, including the capability token below, are never shown
as user-authored overrides and are excluded from capture by name.

### With no interactive client, confirmation fails closed

The daemon has no controlling terminal, and three paths reach a Setup run with nobody
watching: crash reconciliation, remote bootstrap, and the flagship capture-on-laptop
run-on-server acceptance flow. A Setup that needs confirmation and has no interactive client
**fails with a typed error naming the hash**. It never treats an absent human as consent,
which is what an implementer under deadline would otherwise choose.

`qoop setup trust <hash>` records the confirmation ahead of time, which is the out-of-band
step the portability flow now includes.

### The executable is always an absolute path

`LaunchIntent.executable` is resolved to an absolute path at capture time and recorded as
such. A relative or bare name is refused at spawn. If the recorded path does not exist on
the target machine, that is a preflight failure with a clear message, not a silent
fallback to whatever `PATH` finds. A Setup that resolves to a different binary on every
machine is not portable, it is merely quiet.

### Secret values never enter a record

A `SecretRef` is resolved as late as possible, directly into the environment block passed
to `execve`, and the resolved value is never written to a record, an event, a log line, a
support bundle or an error message. Redaction is not a filter applied at output time,
because a filter is a list of patterns someone has to remember to update; the value simply
never enters a structure that anything else can read.

Two things this does **not** claim, because an earlier heading implied both. The child's own
environment is outside our control: the kernel copies it in, it persists for the life of the
process, it is readable through `/proc/<pid>/environ` by any same-uid process on Linux, and
every descendant inherits it. That is the same cooperative boundary as everywhere else and
it is stated rather than glossed. And "zeroed after the spawn call returns" is not
achievable by zeroing at the end: a growing `String` or `Vec` has already left copies in
freed allocations, and the standard library copies the environment into its own buffers
before the call. The requirement is therefore that resolved values are held in a zeroing
type from the moment of resolution.

### Observations authenticate with an unguessable per-terminal token

At spawn, the daemon mints a random 256 bit capability token bound to **that terminal, for
the life of the terminal**, and passes it to the child in the environment. Every observation
delivered over the socket presents the token, and the daemon checks it against the terminal
the observation names.

This covers precedence sources 1, 2 and 3: a native structured agent protocol, an official
hook, and our own shell integration. Naming only "a hook", as an earlier version did, left
source 1 unauthenticated, and source 1 is the strongest evidence in the system and outranks
even the visible-modal override. Sources 4 and 5 are derived from a PTY the daemon owns and
are authenticated by construction.

The token binds the terminal only, deliberately not the generation. The environment is fixed
at `execve` and cannot be rewritten in a running process, so a generation-bound token minted
at terminal creation would be stale the moment the user typed their first command, and every
legitimate hook observation from then on would be rejected while a forgery test still passed.
Generation lives in the observation body, where ADR-008 already requires it to be sampled,
and staleness is decided by comparing that sampled generation against the terminal's current
one. The token answers only "may this caller speak for this terminal".

A terminal id is not a credential. Anything that can guess or read a terminal id could
otherwise forge agent state for a terminal it has nothing to do with, and agent state
drives attention, which drives what a human is told to look at.

Stated limit, consistent with ADR-012: this authenticates the *terminal*, not the *user*. A
process running as this user that can read another child's environment can steal that
token. That is the same cooperative boundary as everywhere else, and the token is worth
having anyway because it removes forgery by accident and by any process that cannot read
another process's environment.

### Capture never records a secret value in any field

Secret redaction applies to every captured field, not only to environment variables. A
command line like `tool --token abc123`, a URL with embedded credentials, and a shell
command string are all common carriers. Capture substitutes a reference and marks the
field unresolved when it cannot prove what the value was. A field it cannot classify is
recorded as unresolved and shown to the user rather than guessed at.

The scanner test asserts absence of known injected secrets. It is a regression test, not a
proof, and the ADR says so: absence of unknown secrets is not provable by scanning, which
is why substitution happens at capture rather than as a filter afterwards.

## Consequences

- A shared Setup costs one confirmation prompt on first use per machine. That is the price
  of the feature being safe rather than merely convenient.
- Preflight failures become more common and more legible: a missing absolute path is
  reported instead of silently resolving to something else.
- The hook protocol gains one field and one check.

## Verification

- A fixture Setup with a hostile `LD_PRELOAD` is refused without confirmation, and the
  confirmation prompt contains that override verbatim.
- A Setup with a relative executable is refused at spawn with a typed error.
- A capture fixture with secrets in env, in argv, in a URL and in a shell command string
  produces an artifact with zero plaintext secret values, asserted by exact match over the
  serialised artifact and over the process's own log output for the same run.
- An observation presenting no token, or a token for a different terminal, or carrying a
  sampled generation older than the terminal's current one, is rejected and counted.
- The positive half, which a forgery-only test would miss entirely: after two foreground
  process changes, an observation carrying the current generation and the original token is
  **accepted**.
- A Setup whose origin field names the local machine but whose hash is not in the ledger
  still prompts.
- A Setup requiring confirmation with no interactive client fails with a typed error rather
  than running.
- The daemon-injected token appears in no captured artifact, no prompt and no support
  bundle.
- A support bundle generated from a session that used secrets contains none of them.

## Rollback

None. Removing the confirmation would restore a remote code execution primitive that ships
by default.
