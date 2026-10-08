//! On macOS, the notch: `notch/main.swift` compiled with the system's own Swift and
//! embedded in the qoro binary, which writes it out and runs it (`src/notch.rs`). One
//! file, one version with the CLI, no second download. Every other target gets nothing.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=notch/main.swift");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("no notch build for a macOS target on {other:?}"),
    };
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("qoro-notch");
    // macOS 14 is the floor: the counts roll with SwiftUI's numeric text transition.
    // Swift 5 language mode, since the app is one file driven from the main thread.
    let status = Command::new("xcrun")
        .args(["swiftc", "-O", "-swift-version", "5", "-target"])
        .arg(format!("{arch}-apple-macos14"))
        .arg("-o")
        .arg(&out)
        .arg("notch/main.swift")
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("swiftc failed ({s}) on notch/main.swift"),
        Err(e) => panic!(
            "cannot run xcrun swiftc ({e}); the Xcode command line tools build the notch: \
             xcode-select --install"
        ),
    }
}
