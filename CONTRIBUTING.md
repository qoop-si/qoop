# Contributing to qoop

## What this repository is

The open part of qoop: the terminal, the daemon, the interface (TUI), agent detection, hooks
and skills. The phone bridge, the hub and the account features (sign in, your phone,
qoop.si/app) ship only in the released binary, so a build from here has no phone part.

Coding agents working here follow [AGENTS.md](AGENTS.md) and
[.qoop/workflow.md](.qoop/workflow.md).

## Build

Install Rust with [rustup](https://rustup.rs): the repository selects the stable channel
(`rust-toolchain.toml`), and Rust 1.98 or newer is needed. Then:

```sh
cargo build --release -p qoop-cli
```

The binary is `target/release/qoop`. On a machine paired with a phone, keep the released
binary: a build from here has no phone part, and the phone bridge stops once it is replaced.

## Checks

CI runs these on every pull request; run them before you open one:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
uv run --no-project python scripts/check-prose.py
uv run --no-project python scripts/check-unsafe.py
uv run --no-project python scripts/check-adrs.py
sh scripts/install-check.sh
cargo test --workspace --locked -- --skip costs_almost_nothing
```

The skipped tests measure idle CPU and need a quiet machine; run them without the skip
when you change the daemon's idle path.

The Python checks need [uv](https://docs.astral.sh/uv/).

## Tests

A test starts `qoop` only through `qoop_testkit::sandboxed`, which gives it a home and a
temporary directory of its own and drops the runner's `XDG_*` and `QOOP_*` variables; a test
that starts a daemon sets `QOOP_RUNTIME_DIR` to a temporary directory. Never point a test or a
manual run at your own daemon, agents or settings: for a manual run, set `QOOP_RUNTIME_DIR`,
`XDG_CONFIG_HOME` and `XDG_DATA_HOME` to a fresh temporary directory, as `.qoop/workflow.md`
shows.

A good test fails when the change it covers is reverted.

## Prose

Docs, comments, commit messages and interface text are plain English. Never use an em dash,
or a double hyphen as a dash; use a comma, a colon, parentheses or two sentences.
`scripts/check-prose.py` checks this.

## Contributor licence agreement

Before your first pull request is merged, you sign the [Contributor Licence Agreement](CLA.md)
once. You keep the copyright in your work; the agreement lets qoop.si ship it in qoop under
any licence, including the released binary's closed parts. A bot comments on your first pull
request; sign by replying with this exact sentence:

```text
I have read the CLA Document and I hereby sign the CLA
```

The signature covers your later pull requests too. Your GitHub username and the pull request
are recorded publicly in the `cla-signatures` branch.

## Pull requests

- One change per pull request, on a branch rebased onto the current `main`.
- A Conventional Commits subject: `feat(tui): ...`, `fix(cli): ...`, `docs(site): ...`.
- Say what you changed, why, and how you ran it.
- Open an issue before a large change, a new dependency, or a change to the wire protocol or
  the on-disk format.
- Pull requests land by rebase, never squash or a merge commit.

## Releases

Versions and `CHANGELOG.md` come from the commit subjects: release-please keeps a Release PR open
(`chore(main): release X.Y.Z`), and merging it is the release. The `release` workflow then builds
every platform, publishes the GitHub release, and qoop.si serves it. Never edit a version or the
changelog by hand.

Security problems go through [SECURITY.md](SECURITY.md), never a public issue.
