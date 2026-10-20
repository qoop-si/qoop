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

## The deeper version

Brief the deeper version of their request, not a bigger one. After their words, word for word with
nothing put inside them, come four answers, one clause each, from the files the work touches (read
them first):

1. what a top professional would do here that they did not ask for;
2. the rawest material to start from instead of what is already digested (the code, a real failing
   input, the real log, over a summary of them);
3. the specific methods and tools that professional would use;
4. what, other than you, the worker or them, would show the result is right: a check inside the
   fence (a test that fails without the change, a real run's output) that proves their `done when`.

Deeper is how, never what: no feature, refactor, deliverable, review round or `done when` they did
not ask for, nothing the fence forbids. Name only a file, test or tool you saw, else its kind ("a
row that breaks"); never production data, secrets or another machine's files. Yours to write,
never a question to them; a `report:` goal gets none.

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
2. The goal: their words in double quotes, unchanged, with when it is done (cleared up with them:
   no second round), then
   `Deeper: a top professional would <1>; start from <2>; use <3>; it is right when <4>.`
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
qoop prompt 57 --generation 1 -- 'First run `qoop task link SHOP-4` and keep its steps ticked, then follow `qoop task rules` and the repo’s `.qoop/workflow.md` as far as the fence allows. Each `qoop task` command alone, one per call, never chained (a chain asks permission). Goal: "the cart total is wrong when a coupon and free shipping meet; done when a test shows the right total and the fix is in a PR." Deeper: a top professional would find why the two rules clash, not patch the sum; start from an order that came out wrong and the code that sums it; use a failing test first; it is right when that test fails before the fix and passes after. tier: mid, because it is one bug in known code. Sub-agents: one low agent to find every place the total is computed; two reviewers on your plan, best for the one that tries to break it, mid for the other; none anywhere else. Do not commit, push, merge, delete, deploy, or contact anything external in this step; ask first.'
```
