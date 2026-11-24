//! qoop desktop: home, sessions and the Inbox over this machine's daemon and its remotes.
//! A `bench` build with flags runs the Phase 0 measurement window instead (`view.rs`).

#[cfg(target_os = "macos")]
mod app;
#[cfg(all(target_os = "macos", feature = "bench"))]
mod bench;
#[cfg(target_os = "macos")]
mod board;
#[cfg(target_os = "macos")]
mod element;
#[cfg(target_os = "macos")]
mod field;
#[cfg(target_os = "macos")]
mod inbox;
#[cfg(target_os = "macos")]
mod keys;
#[cfg(target_os = "macos")]
mod link;
#[cfg(target_os = "macos")]
mod newwork;
#[cfg(target_os = "macos")]
mod probe;
#[cfg(target_os = "macos")]
mod qass;
#[cfg(target_os = "macos")]
mod question;
#[cfg(target_os = "macos")]
mod review;
#[cfg(target_os = "macos")]
mod schedules;
#[cfg(target_os = "macos")]
mod session;
#[cfg(target_os = "macos")]
mod settings;
#[cfg(target_os = "macos")]
mod shell;
#[cfg(all(target_os = "macos", feature = "bench"))]
mod spike_link;
#[cfg(target_os = "macos")]
mod state;
#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests;
#[cfg(target_os = "macos")]
mod theme;
#[cfg(target_os = "macos")]
mod ui;
#[cfg(all(target_os = "macos", feature = "bench"))]
mod view;

/// `--version`: the version the app was built as, for the bundle check and a person's question.
fn version_asked() -> bool {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("qoop-desktop {}", env!("CARGO_PKG_VERSION"));
        return true;
    }
    false
}

#[cfg(target_os = "macos")]
fn main() {
    if version_asked() {
        return;
    }
    #[cfg(feature = "bench")]
    if std::env::args().len() > 1 {
        view::main();
        return;
    }
    app::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    if version_asked() {
        return;
    }
    eprintln!("qoop-desktop is built for macOS only for now");
    std::process::exit(1);
}
