//! The full text of one tool call, fetched when the user opens it.

use agentparse::{Part, Session, Tool, ToolCall};

use crate::item::Detail;
use crate::load::{Error, MAX_TAIL_BYTES, read, stamp};
use crate::locate::Source;
use crate::project::item_id;
use crate::text::redacted_cut;

/// Tail windows tried in turn: the call is usually in the newest, and an older one costs more.
/// A source that is read whole (Gemini, OpenCode, a small file) is parsed once.
const WINDOWS: [u64; 3] = [1024 * 1024, 8 * 1024 * 1024, MAX_TAIL_BYTES];

/// Input and output of the call `tool_id`, redacted and cut at `max_bytes` each (a limit of
/// 64 KiB is the usual one). `Error::NotFound` when no window of the session holds the call.
pub fn detail(source: &Source, tool_id: &str, max_bytes: usize) -> Result<Detail, Error> {
    let len = stamp(source)?.len;
    for window in WINDOWS {
        let (session, cut) = read(source, len, window)?;
        if let Some(found) = find(&session, tool_id, max_bytes) {
            return Ok(found);
        }
        if !cut {
            break;
        }
    }
    Err(Error::NotFound)
}

fn find(session: &Session, tool_id: &str, max_bytes: usize) -> Option<Detail> {
    for call in session.tool_calls() {
        if item_id(&call.id) == tool_id {
            return Some(of_call(call, max_bytes));
        }
    }
    let orphan = session
        .messages()
        .flat_map(|m| &m.parts)
        .find_map(|p| match p {
            Part::ToolResult(r) if item_id(&r.call_id) == tool_id => Some(r),
            _ => None,
        })?;
    Some(build("", &orphan.output.text, max_bytes))
}

fn of_call(call: &ToolCall, max_bytes: usize) -> Detail {
    let output = call.output.as_ref().map(|o| {
        let shell = o.shell.as_ref();
        match (o.text.is_empty(), shell) {
            (true, Some(s)) => [s.stdout.as_deref(), s.stderr.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n"),
            _ => o.text.clone(),
        }
    });
    build(&input_text(call), &output.unwrap_or_default(), max_bytes)
}

fn build(input: &str, output: &str, max_bytes: usize) -> Detail {
    let (input, cut_in) = redacted_cut(input, max_bytes);
    let (output, cut_out) = redacted_cut(output, max_bytes);
    Detail {
        input,
        output,
        truncated: cut_in || cut_out,
    }
}

fn input_text(call: &ToolCall) -> String {
    match &call.tool {
        Tool::Shell(s) => s.command.clone(),
        Tool::Patch { patch } => patch.clone(),
        Tool::Code { source } => source.clone(),
        Tool::Write { path, content } => format!("{path}\n{}", content.as_deref().unwrap_or("")),
        Tool::Edit { path, edits } => edits.iter().fold(path.clone(), |acc, e| {
            format!("{acc}\n- {}\n+ {}", e.old, e.new)
        }),
        _ => match &call.input {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        },
    }
}
