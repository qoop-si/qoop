//! `qoop plugin`: herdr marketplace plugins, run in qoop unchanged.

mod install;
// The host reads the rest of a manifest (titles, panes, events); until it lands some fields
// are only parsed.
#[allow(dead_code)]
mod manifest;
// The host that answers herdr's socket is the first caller; until it lands only the tests read
// these.
#[allow(dead_code)]
mod translate;

pub(crate) use install::run;
