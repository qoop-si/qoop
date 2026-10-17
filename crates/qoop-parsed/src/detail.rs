//! The full text of one tool call, fetched when the user opens it.

use agentparse::{Part, Session};

use crate::item::Detail;
use crate::load::{Error, MAX_TAIL_BYTES, read, stamp};
use crate::locate::Source;
use crate::project::{cell_output, input_text, item_id, output_text, rows};
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
    for part in session.messages().flat_map(|m| &m.parts) {
        if let Part::ToolCall(call) = part
            && let Some(row) = rows(call).into_iter().find(|r| r.id == tool_id)
        {
            return Some(build(
                &input_text(row.call),
                &output_text(row.call),
                max_bytes,
            ));
        }
    }
    let orphan = session
        .messages()
        .flat_map(|m| &m.parts)
        .find_map(|p| match p {
            Part::ToolResult(r) if item_id(&r.call_id) == tool_id => Some(r),
            _ => None,
        })?;
    Some(build("", &cell_output(&orphan.output.text), max_bytes))
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
