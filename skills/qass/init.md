# qass: the default workflow, and `/qass init`

Read this file when a worker's repo has no `.qoro/workflow.md` (use the default below as its
workflow), and when the owner types `/qass init <repo path or agent name>`.

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
   `scripts/`, CI (`.github/workflows/`, `.gitlab-ci.yml`), deploy files, and
   `git log --merges --oneline -30` for how changes land. Never ask what these answer.
2. If `.qoro/workflow.md` exists, never overwrite it: compare it with what you read and ask
   only about what drifted (a check renamed, a step no longer true).
3. Otherwise draft one: at most 12 steps, 4,000 characters, the real commands of this repo, its
   loop limit (one number of review rounds), what "done" proves with the commands that prove it,
   and what to ask the owner first. Start from the default
   above and change only what the repo shows is different.
4. Draft an `AGENTS.md` patch only for stable project facts an agent needs and cannot cheaply
   find (commands, branch, how it deploys, what agents may do without asking); the whole file
   stays under 6,000 characters (already over: no patch, say so). Never write about qoro or qass
   in it: people without qoro read it.
5. Save both drafts in `@QASS_DATA@/init-<repo name>.md` (the checkout folder's name, only
   letters, digits, `.`, `-` and `_`) and ask the owner, the way any question reaches him (A:
   write both, B: write the workflow only, C: change something first), with the drafts' full
   text after the question's first lines.
6. On his yes, write `.qoro/workflow.md` (and the patch) into the repo's working tree and leave
   them uncommitted; tell him which files changed, and that worktrees from `origin/main` get the
   workflow once he commits it (until then you read it from the main checkout). Running it again
   on the same repo is safe: step 2 applies.
