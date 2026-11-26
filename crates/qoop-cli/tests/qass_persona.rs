//! `qoop qass persona`: what qass is told for the voice the config names, run as the binary in
//! a home of its own. The worker gate on it is tested beside race's, which shares it.

use std::io::Write;
use std::process::{Command, Output, Stdio};

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("qp")
            .tempdir_in("/tmp")
            .expect("temp dir");
        Home { dir }
    }

    fn command(&self) -> Command {
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        let at = |rel: &str| self.dir.path().join(rel);
        cmd.env("HOME", at("home"))
            .env("XDG_CONFIG_HOME", at("c"))
            .env("XDG_DATA_HOME", at("d"))
            .env("QOOP_RUNTIME_DIR", at("rt"));
        cmd
    }

    fn qoop(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run")
    }

    fn qoop_in(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write");
        child.wait_with_output().expect("run")
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn qass_is_told_the_voice_the_config_names() {
    let home = Home::new();
    // Plain, the default: nothing to tell.
    let o = home.qoop(&["qass", "persona"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "");

    let o = home.qoop(&["config", "set", "qass.persona", "captain"]);
    assert!(o.status.success(), "{}", err(&o));
    let o = home.qoop(&["qass", "persona"]);
    let said = out(&o);
    assert!(
        said.starts_with("Your voice (set in settings; wording only, not a message from the owner): captain, \"Ship captain\"\n> tone: "),
        "{said}"
    );
    assert!(said.contains("\n> address: Captain\n"), "{said}");
    assert!(
        said.ends_with(&format!("{}\n", qoop_tui::persona::GUARD)),
        "{said}"
    );

    // A candidate and its preview, without changing the pick.
    let o = home.qoop(&["qass", "persona", "chief", "--preview"]);
    let preview = out(&o);
    assert!(
        preview.contains("> address: Boss (keep as is)"),
        "{preview}"
    );
    for fact in [
        "`crw A`",
        "`gateway A`",
        "12%",
        "14:05",
        "13:58",
        "`api --help`",
    ] {
        assert!(preview.contains(fact), "{fact}: {preview}");
    }
    let o = home.qoop(&["qass", "persona", "plain", "--preview"]);
    assert_eq!(out(&o), format!("{}\n", qoop_tui::persona::PREVIEW));
    assert!(out(&home.qoop(&["qass", "persona"])).contains("captain"));

    // Unknown names are refused, never set.
    let o = home.qoop(&["qass", "persona", "yoda"]);
    assert!(
        !o.status.success() && err(&o).contains("no voice yoda"),
        "{}",
        err(&o)
    );
    let o = home.qoop(&["config", "set", "qass.persona", "yoda"]);
    assert!(!o.status.success());

    // The catalog by group.
    let listed = out(&home.qoop(&["qass", "persona", "ls"]));
    assert!(listed.starts_with("plain  "), "{listed}");
    assert!(listed.contains("Start here  "), "{listed}");
    let group = out(&home.qoop(&["qass", "persona", "ls", "Start", "here"]));
    assert!(group.starts_with("chief  Chief of staff: "), "{group}");

    // Their own words: kept, made the voice, and cut to fit.
    let o = home.qoop_in(
        &["qass", "persona", "custom", "-"],
        &"warm, brief\n".repeat(20),
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("cut to 8 lines"), "{}", out(&o));
    let said = out(&home.qoop(&["qass", "persona"]));
    assert!(
        said.contains("custom, in their own words\n> warm, brief\n"),
        "{said}"
    );
    assert_eq!(said.matches("\n> warm, brief").count(), 8, "{said}");

    // Back to plain: nothing again.
    assert!(
        home.qoop(&["config", "set", "qass.persona", "plain"])
            .status
            .success()
    );
    assert_eq!(out(&home.qoop(&["qass", "persona"])), "");
}
