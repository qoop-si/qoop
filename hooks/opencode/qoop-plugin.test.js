// `bun test hooks/opencode`: the plugin's mapping and its guard, against a fake `qoop`.
import { beforeEach, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const dir = mkdtempSync(join(tmpdir(), "qoop-plugin-"));
const calls = join(dir, "calls");
writeFileSync(join(dir, "qoop"), `#!/bin/sh\necho "$*" >> "${calls}"\n`);
chmodSync(join(dir, "qoop"), 0o755);
writeFileSync(calls, "");
process.env.PATH = `${dir}:${process.env.PATH}`;
process.env.QOOP_TERMINAL = "4";
process.env.QOOP_TOKEN = "t";

const { default: plugin, handle } = await import("./qoop-plugin.js");

// The lines the fake qoop saw, once `count` of them are in (reports go out one at a time).
const sent = async (count) => {
  for (let i = 0; i < 100; i++) {
    const lines = readFileSync(calls, "utf8").trim().split("\n").filter(Boolean);
    if (lines.length >= count) return lines;
    await new Promise((r) => setTimeout(r, 20));
  }
  return readFileSync(calls, "utf8").trim().split("\n").filter(Boolean);
};
const none = async () => {
  await new Promise((r) => setTimeout(r, 200));
  return readFileSync(calls, "utf8").trim();
};
const ev = (type, data) => handle({ type, data });

// Reports of the last test may still be in flight: let them land before the file is emptied.
beforeEach(async () => {
  await new Promise((r) => setTimeout(r, 150));
  writeFileSync(calls, "");
});

test("a turn reports working with its session, then idle", async () => {
  ev("session.execution.started", { sessionID: "ses_a" });
  ev("session.execution.succeeded", { sessionID: "ses_a" });
  expect(await sent(2)).toEqual([
    "hook-report --state working --visible-working --session-id ses_a --event user",
    "hook-report --state idle --visible-idle --session-id ses_a --event done",
  ]);
});

test("a failed turn reports failed", async () => {
  ev("session.execution.failed", { sessionID: "ses_f" });
  expect(await sent(1)).toEqual(["hook-report --state failed --session-id ses_f"]);
});

test("a subagent never speaks for the tab", async () => {
  ev("session.created", { sessionID: "ses_c", parentID: "ses_a" });
  ev("session.execution.started", { sessionID: "ses_c" });
  ev("session.tool.called", { sessionID: "ses_c", tool: "bash" });
  ev("session.execution.succeeded", { sessionID: "ses_c" });
  expect(await none()).toBe("");
});

test("an open request turns working into blocked, and the turn's end clears it", async () => {
  ev("permission.asked", { id: "p1", sessionID: "ses_b", permission: "bash" });
  ev("session.execution.started", { sessionID: "ses_b" });
  ev("session.execution.succeeded", { sessionID: "ses_b" });
  ev("session.execution.started", { sessionID: "ses_b" });
  const lines = await sent(4);
  expect(lines[0]).toContain("--state blocked");
  expect(lines[1]).toContain("--state blocked");
  expect(lines[2]).toContain("--state idle");
  expect(lines[3]).toContain("--state working");
});

test("an event with no data does not throw", () => {
  expect(() => ev("session.execution.started", null)).not.toThrow();
});

test("only a per-tab server (serve --stdio) subscribes, and it reports", async () => {
  const argv = process.argv;
  try {
    const ctx = { event: { subscribe: () => { throw new Error("subscribed"); } } };
    process.argv = ["opencode", "serve", "--service"];
    expect(plugin.setup(ctx)).toBeUndefined();

    let aborted = false;
    const stream = {
      async *[Symbol.asyncIterator]() {
        yield { type: "session.execution.started", data: { sessionID: "ses_s" } };
        yield null;
        yield { type: "session.execution.succeeded", data: { sessionID: "ses_s" } };
      },
    };
    process.argv = ["opencode", "serve", "--stdio", "--port", "0"];
    const stop = plugin.setup({
      event: { subscribe: ({ signal }) => (signal.addEventListener("abort", () => (aborted = true)), stream) },
    });
    const lines = await sent(2);
    expect(lines[0]).toContain("--state working");
    expect(lines[1]).toContain("--state idle");
    stop();
    expect(aborted).toBe(true);
  } finally {
    process.argv = argv;
  }
});

test("the module is a v2 definition", () => {
  expect(plugin.id).toBe("qoop");
  expect(typeof plugin.setup).toBe("function");
});
