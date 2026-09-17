// qoro hook for Amp (plugin).
//
// Install: load this file as an Amp plugin (see the Amp manual's plugin
// section) with `qoro` on PATH. Reports go through `qoro hook-report`, which
// reads the terminal and token the daemon put in the environment. Outside a
// qoro terminal every report is a silent no-op and the screen reading keeps
// working with no hooks installed.
//
// Event reference: https://ampcode.com/manual/plugin-api
// (`session.start`, `agent.start`, `agent.end`, `tool.call`, `tool.result`).

import { execFile } from "node:child_process";

function flagsFor(state) {
  if (state === "blocked") return ["--visible-blocker"];
  if (state === "idle") return ["--visible-idle"];
  if (state === "working") return ["--visible-working"];
  return [];
}

function report(state) {
  if (!process.env.QORO_TERMINAL || !process.env.QORO_TOKEN) {
    return;
  }
  const args = ["hook-report", "--state", state, ...flagsFor(state)];
  execFile("qoro", args, { timeout: 5000 }, () => {});
}

export default function (amp) {
  // A thread starting or resuming means the agent holds the turn.
  amp.on("session.start", () => {
    report("working");
  });
  // A turn starting, or a tool call going out, is work in progress.
  amp.on("agent.start", () => {
    report("working");
  });
  amp.on("tool.call", () => {
    report("working");
  });
  amp.on("tool.result", () => {
    report("working");
  });
  // A turn ending hands the terminal back to the person.
  amp.on("agent.end", () => {
    report("idle");
  });
}
