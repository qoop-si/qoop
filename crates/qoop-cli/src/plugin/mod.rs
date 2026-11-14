//! `qoop plugin`: herdr marketplace plugins, run in qoop unchanged.

// The host that answers herdr's socket is the first caller; until it lands only the tests read
// these.
#[allow(dead_code)]
mod translate;
