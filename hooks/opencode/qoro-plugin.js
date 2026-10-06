// qoro hook for OpenCode (plugin).
//
// Install: `qoro hooks install opencode`, or copy this file into
// `~/.config/opencode/plugin/`, with `qoro` on PATH. Do not also list it in
// `opencode.json`, which would load it twice. Reports go through
// `qoro hook-report`, which reads the terminal and token the daemon put in
// the environment. Outside a qoro terminal every report is a silent no-op and
// the screen reading keeps working with no hooks installed.
//
// Event reference: https://opencode.ai/docs/plugins/ and the event types in
// @opencode-ai/sdk. What is read here:
//   session.status    {sessionID, status: {type: idle | busy | retry}}
//   session.idle      {sessionID}
//   session.error     {sessionID?, error?}
//   permission.asked  {id, sessionID, permission, patterns}  (permission.updated
//                     in older plugin types, with type, pattern and title)
//   permission.replied {requestID} (permissionID in older types)
//   question.asked {id}, question.replied, question.rejected
//   session.created / session.updated {info: {id, parentID?}}
// plus the `chat.message` and `tool.execute.before` hooks for focus events.
//
// The session id travels as `--session-id`, which is what lets a tab come back
// to this conversation after a restart. A subagent's session (one with a
// parent) never does: it is not the conversation a person resumes, and its
// going idle is not the agent going idle.
//
// Requests still waiting on the person are kept by id. While any is open, a
// report that would say working says blocked instead: with parallel tools or
// subagents, work elsewhere does not answer the request that is still up.

import { execFile } from "node:child_process";

const LABEL_MAX = 120;

// Sessions known to be subagents, by id.
const children = new Set();

// Permission and question requests asked and not yet answered, by id.
const pending = new Set();

// The report in flight, so the next one waits for it.
let queue = Promise.resolve();

function visibleFlag(state) {
  if (state === "blocked") return ["--visible-blocker"];
  if (state === "idle") return ["--visible-idle"];
  if (state === "working") return ["--visible-working"];
  return [];
}

// One line, no control characters, cut to the length the daemon keeps.
function clean(text) {
  if (typeof text !== "string") return "";
  return text
    .replace(/[\u0000-\u001f\u007f]+/g, " ")
    .trim()
    .slice(0, LABEL_MAX);
}

function report(state, { sessionID, event, label } = {}) {
  if (!process.env.QORO_TERMINAL || !process.env.QORO_TOKEN) {
    return;
  }
  if (state === "working" && pending.size > 0) state = "blocked";
  const args = ["hook-report", "--state", state, ...visibleFlag(state)];
  if (typeof sessionID === "string" && sessionID && !children.has(sessionID)) {
    args.push("--session-id", sessionID);
  }
  if (event) {
    args.push("--event", event);
    const text = clean(label);
    if (text) args.push("--label", text);
  }
  // One report at a time, in event order: two started together could reach the
  // daemon the other way round, and the later state would be the stale one.
  queue = queue.then(
    () =>
      new Promise((done) =>
        execFile("qoro", args, { timeout: 5000 }, () => done()),
      ),
  );
}

function noteSession(info) {
  if (info && typeof info.id === "string" && info.parentID) {
    children.add(info.id);
  }
}

function permissionLabel(p) {
  if (typeof p?.title === "string" && p.title) return p.title;
  const patterns = Array.isArray(p?.patterns)
    ? p.patterns.join(" ")
    : (p?.pattern ?? "");
  return [p?.permission ?? p?.type, patterns].filter(Boolean).join(" ");
}

function requestID(p) {
  return p?.requestID ?? p?.permissionID ?? p?.id;
}

function toolLabel(tool, args) {
  const detail =
    args?.command ?? args?.filePath ?? args?.pattern ?? args?.url ?? "";
  return [tool, typeof detail === "string" ? detail : ""]
    .filter(Boolean)
    .join(" ");
}

export const QoroPlugin = async () => ({
  event: async ({ event }) => {
    const p = event?.properties ?? {};
    switch (event?.type) {
      case "session.created":
      case "session.updated":
        noteSession(p.info);
        break;
      case "session.status": {
        const type = p.status?.type;
        if (type === "busy" || type === "retry") {
          report("working", { sessionID: p.sessionID });
        } else if (type === "idle" && !children.has(p.sessionID)) {
          // The turn is over: nothing it asked can still be waiting, even when an
          // answer came without a reply event that named its request.
          pending.clear();
          report("idle", { sessionID: p.sessionID });
        }
        break;
      }
      case "session.idle":
        if (!children.has(p.sessionID)) {
          // The turn is over: nothing it asked can still be waiting.
          pending.clear();
          report("idle", { sessionID: p.sessionID, event: "done" });
        }
        break;
      case "session.error":
        // An interrupt the person asked for is not a failure.
        if (
          !children.has(p.sessionID) &&
          p.error?.name !== "MessageAbortedError"
        ) {
          report("failed", { sessionID: p.sessionID });
        }
        break;
      case "permission.asked":
      case "permission.updated":
        if (requestID(p)) pending.add(requestID(p));
        report("blocked", {
          sessionID: p.sessionID,
          event: "blocked",
          label: permissionLabel(p),
        });
        break;
      case "question.asked":
        if (requestID(p)) pending.add(requestID(p));
        report("blocked", {
          sessionID: p.sessionID,
          event: "blocked",
          label: "question",
        });
        break;
      case "permission.replied":
      case "question.replied":
      case "question.rejected":
        pending.delete(requestID(p));
        report("working", { sessionID: p.sessionID });
        break;
    }
  },
  // A prompt sent is the turn starting. The prompt text is never a label.
  "chat.message": async (input) => {
    report("working", { sessionID: input?.sessionID, event: "user" });
  },
  "tool.execute.before": async (input, output) => {
    report("working", {
      sessionID: input?.sessionID,
      event: "tool",
      label: toolLabel(input?.tool, output?.args),
    });
  },
});
