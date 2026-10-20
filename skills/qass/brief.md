# The first brief to a worker you started

Read this before `qoop task add`, `spawn.sh` and the first `qoop prompt` to the worker it starts.

## Before you create

Check their request says what, where (repo or space), when it is done, its fences (what not to
touch) and how urgent. The tier is your pick (below), never a question. Missing one that changes
the work: ask ONE card ("Cards" in SKILL.md; agent `-`, its handle a word of their request) with
every missing point at once, options with reasons and your pick. Never a second round, never what
the repo, the lessons or the screen answers. Nothing missing: create, do not ask.
After their answer: the item (a short title, a real description, their steps, a `done when` line),
the worker started with the brief below, and one line to them with its key and space.

## Tier

Pick one for the work, and say which and why in one clause of the brief:

- `best`: hard planning, review, risky or ambiguous work.
- `mid`: normal implementation; most tasks.
- `low`: mechanical, bulk or read-heavy work: scans, listings, simple edits, summaries.

Pass it as `spawn.sh ... --tier <tier>`: a claude worker on that tier's model from `[qass.models]`
in config.toml (the newest Opus, Sonnet and Haiku by default; never a Fable model). They change it
with one word (`best`, `mid`, `low`): spawn again, or tell them it takes a new worker. spawn.sh
says on stderr which model it used (the worker's banner, `qoop read`, shows the exact id): tell
them once. An opencode worker keeps its own model. A woken worker comes back on claude's default
(a restore types a bare `claude --resume`): the tier holds only from its start.

## The brief

One `qoop prompt` line, in this order:

1. "First run `qoop task link <KEY>`" (or `qoop task new <goal>` for work on no item) "and keep its
   steps ticked, then follow `qoop task rules` and the repo’s `.qoop/workflow.md` as far as the
   fence allows. Each `qoop task`
   command alone, one per call, never chained (a chain asks permission)."
2. The goal in the owner's words and when it is done (already cleared up with them: no second round).
3. `tier: <tier>, because <why>`.
4. Sub-agents (claude workers only), a few clauses: where the work fans out and on which tier
   (its subagent `model`: best `opus`, mid `sonnet`, low `haiku`), and that it uses them there and
   nowhere else. For example: three reviewers in parallel at the plan
   (best for the one that tries to break it, mid for the others); a low agent to scan many files;
   one mid agent per independent lane.
5. The fence sentence (`SKILL.md`, "Writing to a worker"), then: "Run your own browser and
   simulator tests headless and read the screenshots yourself; open a window only when the owner
   asks to see something, and close what you open."

A step the fence forbids comes back to you. A file their message names (a screenshot) you may Read,
and put its path in the brief.

## Example

```
"<this skill's dir>/spawn.sh" ~/src/shop fix/cart-total --tier mid       # prints "57 1"
qoop prompt 57 --generation 1 -- 'First run `qoop task link SHOP-4` and keep its steps ticked, then follow `qoop task rules` and the repo’s `.qoop/workflow.md` as far as the fence allows. Each `qoop task` command alone, one per call, never chained (a chain asks permission). Goal: the cart total is wrong when a coupon and free shipping meet; done when a test shows the right total and the fix is in a PR. tier: mid, because it is one bug in known code. Sub-agents: one low agent to find every place the total is computed; two reviewers on your plan, best for the one that tries to break it, mid for the other; none anywhere else. Do not commit, push, merge, delete, deploy, or contact anything external in this step; ask first.'
```
