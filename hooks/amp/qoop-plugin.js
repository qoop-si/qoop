// qoop hook for Amp (plugin).
//
// Install: load this file as an Amp plugin (see the Amp manual's plugin
// section) with `qoop` on PATH. Reports go through `qoop hook-report`, which
// reads the terminal and token the daemon put in the environment. Outside a
// qoop terminal every report is a silent no-op and the screen reading keeps
// working with no hooks installed.
//
// Event reference: https://ampcode.com/manual/plugin-api
// (`session.start`, `agent.start`, `agent.end`, `tool.call`, `tool.result`).
//
// Every one of these events carries the thread as `event.thread.id`: the
// conversation the report belongs to. It is passed through as `--session-id`,
// which is what lets a tab come back to this thread after a restart rather
// than to whichever one the resume command picks. An event with no thread
// reports with no flag, same as before.

import { execFile } from "node:child_process";

function flagsFor(state) {
  if (state === "blocked") return ["--visible-blocker"];
  if (state === "idle") return ["--visible-idle"];
  if (state === "working") return ["--visible-working"];
  return [];
}

function threadIDOf(event) {
  const id = event?.thread?.id;
  return typeof id === "string" && id.length > 0 ? id : undefined;
}

function report(state, threadID) {
  // A terminal started before the rename carries the old names.
  if (
    !(process.env.QOOP_TERMINAL || process.env.QORO_TERMINAL) ||
    !(process.env.QOOP_TOKEN || process.env.QORO_TOKEN)
  ) {
    return;
  }
  const args = ["hook-report", "--state", state, ...flagsFor(state)];
  if (threadID) {
    args.push("--session-id", threadID);
  }
  execFile("qoop", args, { timeout: 5000 }, () => {});
}

export default function (amp) {
  // A thread starting or resuming means the agent holds the turn.
  amp.on("session.start", (event) => {
    report("working", threadIDOf(event));
  });
  // A turn starting, or a tool call going out, is work in progress.
  amp.on("agent.start", (event) => {
    report("working", threadIDOf(event));
  });
  amp.on("tool.call", (event) => {
    report("working", threadIDOf(event));
  });
  amp.on("tool.result", (event) => {
    report("working", threadIDOf(event));
  });
  // A turn ending hands the terminal back to the person.
  amp.on("agent.end", (event) => {
    report("idle", threadIDOf(event));
  });
}
