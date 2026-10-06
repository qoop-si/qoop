# qass: the default workflow, `/qass init`, and verifying where it runs

Read this file when a worker's repo has no `.qoro/workflow.md` (use the default below as its
workflow), when the owner types `/qass init <repo path or agent name>`, and when a finished
task's workflow has a "Verify where it runs" section for you to run.

## The default workflow

1. Understand the outcome asked for; infer it from the request, the repo and its history
   before asking.
2. Investigate the code, the history and the constraints.
3. Write a plan with how it will be verified; have it reviewed; revise.
4. Implement.
5. Review the change independently; revise what blocks.
6. Run the project's checks (build, lint, tests).
7. Run it for real and keep the output.
8. Commit, push, open a PR, as the repo's own rules allow.
9. Deploy or install, if the project does.
10. Verify the outcome itself, where it runs.

Loop limit: at most 8 review rounds per task; a round is a change of the task's stage to
`reviewing`. Done means each closing step above has its proof in the task's evidence or in the
repo; a build or install is checked only where the workflow names where it lands.

## `/qass init <repo>`

The repo is a path, or the agent whose directory it is (by its sidebar name). Use the repo's
main checkout, never a worker's worktree (a worker could commit your draft with its own work);
an agent on a remote: ask the owner for a local path. Then:

1. Read what is there: `README*`, `AGENTS.md`, `CLAUDE.md`, `.claude/rules/`, the build and
   test files (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `Makefile`, `justfile`),
   `scripts/`, CI (`.github/workflows/`, `.gitlab-ci.yml`), deploy files (`Dockerfile`,
   `docker-compose*`, `vercel.json`, `fly.toml`, `wrangler.toml`, `.env.example`), and
   `git log --merges --oneline -30` for how changes land. Never ask what these answer.
2. Read what he asked for in this repo, all sessions and worktrees: `qoro history --dir <repo>
   --last 100` as it is, and `qoro history --dir <repo> --last 2000` through `grep -iE` for
   testing and shipping words (test, staging, prod, deploy, local, browser, simulator, e2e,
   log, merge, and the same in the language he writes in), keeping the 200 most telling: where
   it runs, how he checks it, what proof he wants, what he forbids. They may only narrow the
   draft or add a prohibition, never add a verb `policy` does not grant. A key, token, password
   or URL with credentials never goes into a draft (it reaches his phone).
3. If `.qoro/workflow.md` exists, never overwrite it: compare it with what you read and ask
   only about what drifted (a check renamed, a step no longer true).
4. Otherwise draft one: at most 12 steps, 4,000 characters, the real commands of this repo, its
   loop limit (one number of review rounds), what "done" proves with the commands that prove it,
   what to ask the owner first, and its "Verify where it runs" section (below). Start from the
   default above and change only what the repo and his words show is different.
5. Draft an `AGENTS.md` patch only for stable project facts an agent needs and cannot cheaply
   find (commands, branch, how it deploys, what agents may do without asking); the whole file
   stays under 6,000 characters (already over: no patch, say so). Never write about qoro or qass
   in it: people without qoro read it.
6. Save both drafts in `@QASS_DATA@/init-<repo name>.md` (the checkout folder's name, only
   letters, digits, `.`, `-` and `_`) and ask the owner, the way any question reaches him (A:
   write both, B: write the workflow only, C: change something first), with the drafts' full
   text after the question's first lines.
7. On his yes, write `.qoro/workflow.md` (and the patch) into the repo's working tree and leave
   them uncommitted; tell him which files changed, and that worktrees from `origin/main` get the
   workflow once he commits it (until then you read it from the main checkout). The saved draft
   is now his approved copy; a drift he approves in step 3 rewrites it. Running init again on
   the same repo is safe: step 3 applies.

## The "Verify where it runs" section

The text from its `## Verify where it runs` heading to the next `## ` heading or the end.
Bullets, never numbered (numbers are steps). `none: the tests are the proof` unless the repo
shows a server, an app or a deploy target. Otherwise:

- where: `staging <url>`, `local`, or `installed <path>`;
- start (for `local`): the shell lines that run it from a fresh copy of the merge commit, its
  install and build included (the copy has no `.git`, `node_modules`, `target` or built
  assets), the env var or flag that sets its port, and how long it may take to answer;
- check: a GET or HEAD on a URL with what must come back, a page and what it shows, or an
  output line; and the proof to keep;
- never: what the check must not touch (production data, live services, a real channel).

A check against staging is a GET or HEAD only; anything else needs his word.

## Verifying where it runs

After a finished task's closing checks pass, before `qoro task close --verified`. Always, even
when the worker ran it: he wants you to see it work yourself. A section of `none`, or no
section: nothing to run.

1. Only what he approved: the section, whitespace trimmed, must equal the one saved in
   `@QASS_DATA@/init-<repo name>.md`. None saved yet, or different: a card with the section's
   full text (A: approve it, B: change it first); on his A save it there, then go on. Never run
   a section he has not approved.
2. `installed <path>`: compare its version or mtime with the merge, and run its check.
   `staging <url>`: the GET or HEAD it names (`curl -sS -o /dev/null -w '%{http_code}' <url>`)
   or the page in a browser.
3. `local`: never in his main checkout. `git -C <checkout> fetch`, then `mktemp -d
   /tmp/qv.XXXXXX` prints the scratch path `<s>` (short: a socket path has a limit; write it
   out in each later command, a shell variable does not outlive one call), and
   `git -C <checkout> archive <merge> | tar -x -C <s>` and `mkdir <s>/home`. Write the
   section's start lines to `<s>/.qass-start.sh`; a toolchain it needs from his home
   (`CARGO_HOME`, `RUSTUP_HOME`, a cache dir) is named there, never inherited. Pick a port nothing listens on (`lsof -nP
   -iTCP:<port> -sTCP:LISTEN` prints nothing).
4. Start it in the repo's space, with nothing from your environment but `PATH` (no `.env`,
   key or token passes through it; a check that needs one: ask him):
   `qoro new --cwd <s> --space <space> -- env -i PATH="$PATH" HOME=<s>/home <PORT>=<port> sh .qass-start.sh`. Put the terminal's number on the worker's `goals.md` line. `qoro read <n>`
   that it was typed (`qoro new` types nothing after 10 s): not typed is a card, never a fail.
   Wait as long as the section says (else 5 minutes) for it to answer: no answer by then, or a
   missing toolchain, is a card, never a fail. Run the check, keep its output and the terminal's last lines with keys,
   tokens and personal data cut out, then `qoro close <n>` and `rm -rf <s>`.
5. Pass: the close note names what ran and what came back. Fail: a card, `now` (main is
   broken), with the failing output trimmed the same way; his answer may be a new worker. A
   remote worker: a card, as for its other checks.
