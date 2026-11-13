//! qoop desktop, Phase 0 spike: one window, up to four of the daemon's terminals in a 2x2
//! split, keys, the input method and resize. No chrome yet. Flags are in `view.rs`.

#[cfg(all(target_os = "macos", feature = "bench"))]
mod bench;
#[cfg(target_os = "macos")]
mod element;
#[cfg(target_os = "macos")]
mod keys;
#[cfg(target_os = "macos")]
mod link;
#[cfg(target_os = "macos")]
mod probe;
#[cfg(target_os = "macos")]
mod view;

#[cfg(target_os = "macos")]
fn main() {
    view::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("qoop-desktop is built for macOS only for now");
    std::process::exit(1);
}
