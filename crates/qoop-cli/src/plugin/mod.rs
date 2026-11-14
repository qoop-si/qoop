//! `qoop plugin`: herdr marketplace plugins, run in qoop unchanged.

mod host;
mod install;
// The host reads the rest of a manifest (titles, panes); until the next part lands some fields
// are only parsed.
#[allow(dead_code)]
mod manifest;
mod shim;
// Shapes the writes and the plugin panes use land with the next part of the host.
#[allow(dead_code)]
mod translate;

pub(crate) use host::ensure_plugin_host;
pub(crate) use install::run;
pub(crate) use shim::{invoked as shim_invoked, run as shim_run};
