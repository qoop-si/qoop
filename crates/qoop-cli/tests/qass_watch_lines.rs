//! qass's watcher (`skills/qass/watch.sh`) as it runs: a copy with its data folder filled in, a
//! stand-in `qoop` first on `PATH` (snapshots, briefs, remotes) and a stand-in `on.sh` beside it.
//! What each line says before it wakes qass: the brief's verdict (A1), a tab the owner is at held
//! (A2), and a stop qass already handled riding (A3).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const QOOP: &str = r#"#!/bin/bash
# A stand-in for qoop: snapshots, briefs and remotes from $STUB.
case "$1 ${2:-}" in
    "agents --watch")
        while IFS= read -r l; do
            case "$l" in "sleep "*) sleep "${l#sleep }" ;; *) printf '%s\n' "$l" ;; esac
        done < "$STUB/snaps"
        sleep "${STUB_HOLD:-1}" ;;
    "qass brief")
        t="$3"; echo "$t" >> "$STUB/calls"
        [ ! -f "$STUB/brief-$t.sleep" ] || sleep "$(cat "$STUB/brief-$t.sleep")"
        [ ! -f "$STUB/brief-$t.exit" ] || exit "$(cat "$STUB/brief-$t.exit")"
        if [ -x "$STUB/brief-$t.sh" ]; then "$STUB/brief-$t.sh"
        elif [ -f "$STUB/brief-$t.json" ]; then cat "$STUB/brief-$t.json"
        else cat "$STUB/brief.json"; fi ;;
    "remote ls") cat "$STUB/remotes" 2>/dev/null || true ;;
    "read "*) cat "$STUB/screen" 2>/dev/null || true ;;
    "ls --json") echo '[]' ;;
    "ask ls") echo '{"live":[],"ended":[]}' ;;
    *) exit 0 ;;
esac
"#;

const ON: &str = r#"#!/bin/bash
# A stand-in for on.sh: the remote's snapshots from $STUB/remote-snaps.
[ "$1" != --master ] || shift
shift
case "$*" in
    "agents --watch --json")
        while IFS= read -r l; do
            case "$l" in "sleep "*) sleep "${l#sleep }" ;; *) printf '%s\n' "$l" ;; esac
        done < "$STUB/remote-snaps"
        sleep "${REMOTE_HOLD:-1}" ;;
    --version) echo "qoop 1" ;;
    *) echo '[]' ;;
esac
"#;

/// The brief's answer for a plain local agent: write allowed, ending none, nobody at the tab.
const ALLOWED: &str = r#"{"allowed":"write (ending none)","ending":"none","not_now":[],"at":1000,"person_known":true,"person_until":null,"last_typed_by":"owner"}"#;

/// qass's own terminal in every test.
const QASS: u32 = 9;

struct Watch {
    root: tempfile::TempDir,
}

impl Watch {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("a directory");
        let w = Self { root };
        for d in ["bin", "skill", "data", "stub"] {
            std::fs::create_dir_all(w.path(d)).expect("mkdir");
        }
        w.exe("bin/qoop", QOOP);
        w.exe("skill/on.sh", ON);
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/qass/watch.sh"),
        )
        .expect("watch.sh");
        let data = w.path("data").display().to_string();
        w.exe("skill/watch.sh", &script.replace("@QASS_DATA@", &data));
        w.put("stub/brief.json", ALLOWED);
        w
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn put(&self, rel: &str, body: &str) {
        std::fs::write(self.path(rel), body).expect("write");
    }

    fn exe(&self, rel: &str, body: &str) {
        self.put(rel, body);
        std::fs::set_permissions(self.path(rel), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap_or_default()
    }

    /// The local stream: snapshots (rows arrays), each `sleep <s>` line a pause.
    fn snaps(&self, lines: &[String]) {
        self.put("stub/snaps", &(lines.join("\n") + "\n"));
    }

    /// The briefs asked for, by terminal.
    fn calls(&self) -> Vec<String> {
        self.read("stub/calls")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Runs the watcher until the local stream ends `hold` seconds after its last snapshot:
    /// what it printed, and how long it took.
    fn run(&self, hold: u32, env: &[(&str, &str)]) -> (Vec<String>, Duration) {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new("/bin/bash");
        command
            .arg(self.path("skill/watch.sh"))
            .current_dir(self.root.path())
            .env("PATH", path)
            .env("STUB", self.path("stub"))
            .env("STUB_HOLD", hold.to_string())
            .env("QOOP_TERMINAL", QASS.to_string())
            .env("QASS_SETTLE", "0")
            .env_remove("QASS_QUIET");
        for (key, value) in env {
            command.env(key, value);
        }
        let started = Instant::now();
        let out = command.output().expect("run watch.sh");
        let lines = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        (lines, started.elapsed())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_secs() as i64
}

/// An agent row; `extra` is more JSON fields, each with its leading comma.
fn row(t: u32, state: &str, extra: &str) -> String {
    format!(
        r#"{{"terminal":{t},"generation":1,"agent":true,"program":"claude","state":"{state}","name":"w","cwd":"/tmp/x","task":{{"id":12,"stage":"implementing","claimed":false}}{extra}}}"#
    )
}

/// qass's own row: at rest since `active`, or working.
fn qass_row(state: &str, active: i64) -> String {
    format!(
        r#"{{"terminal":{QASS},"generation":1,"agent":true,"program":"claude","state":"{state}","last_active_at_unix":{active}}}"#
    )
}

fn rows(r: &[String]) -> String {
    format!("[{}]", r.join(","))
}

/// The lines about terminal `t` of this Mac.
fn of(lines: &[String], t: u32) -> Vec<&String> {
    let head = format!("T{t} ");
    lines.iter().filter(|l| l.starts_with(&head)).collect()
}

/// A plain stop gets the brief's verdict and its stop before the task, and leaves its said
/// record and a `print:` note.
#[test]
fn a_local_stop_carries_the_verdict_before_its_task() {
    let w = Watch::new();
    w.snaps(&[
        rows(&[row(5, "working", "")]),
        rows(&[row(5, "idle", r#","stop":"100.1""#)]),
    ]);
    let (lines, _) = w.run(1, &[]);
    assert_eq!(
        lines,
        [
            r#"T5 g1 claude working->idle "w" /tmp/x stop:100.1 allowed:write:none@1000 task:12:implementing"#
        ]
    );
    assert_eq!(w.calls(), ["5"]);
    let said = w.read("data/said");
    let f: Vec<&str> = said.split_whitespace().collect();
    assert_eq!(&f[..4], ["5", "1", "idle", "implementing"], "{said}");
    assert_eq!(f[5], "100.1", "{said}");
    assert!(
        w.read("data/quiet.log")
            .contains("print: T5 g1 100.1 repeat:no handled:no"),
        "{}",
        w.read("data/quiet.log")
    );
}

/// A brief that fails, outlives its 2 s, or gives no `allowed` reads `allowed:?`, even when it
/// wrote a whole verdict first, and the slow one holds the block about 2 s, not its own time.
#[test]
fn a_brief_that_fails_or_is_slow_reads_unknown() {
    let w = Watch::new();
    w.put("stub/brief-5.exit", "1");
    w.put("stub/brief-6.sleep", "6");
    w.put("stub/brief-7.json", r#"{"lines":[]}"#);
    w.exe(
        "stub/brief-8.sh",
        "#!/bin/sh\ncat \"$STUB/brief.json\"\nexit 1\n",
    );
    w.exe(
        "stub/brief-10.sh",
        "#!/bin/sh\ncat \"$STUB/brief.json\"\nsleep 6\n",
    );
    let blocked = r#","stop":"1.1""#;
    let terminals = [5, 6, 7, 8, 10];
    let before: Vec<String> = terminals.iter().map(|t| row(*t, "working", "")).collect();
    let after: Vec<String> = terminals
        .iter()
        .map(|t| row(*t, "blocked", blocked))
        .collect();
    w.put("data/last.json", &rows(&before));
    w.snaps(&[rows(&after)]);
    let (lines, took) = w.run(0, &[]);
    for t in terminals {
        let l = of(&lines, t);
        assert!(
            l.len() == 1 && l[0].contains(" allowed:? task:12:"),
            "{lines:?}"
        );
    }
    assert!(took < Duration::from_secs(5), "{took:?}");
}

/// The verdict's words: a hands-off time, the review limit, and a tab another agent drives.
#[test]
fn the_tag_names_until_no_round_and_driven() {
    let w = Watch::new();
    let until = now() + 300;
    w.put(
        "stub/brief-5.json",
        &format!(
            r#"{{"allowed":null,"ending":"merge","not_now":["write (owner typed here 5 s ago)"],"at":1000,"hands_off_until":{until},"person_known":true,"person_until":null}}"#
        ),
    );
    w.put(
        "stub/brief-6.json",
        r#"{"allowed":"write (ending merge)","ending":"merge","not_now":["another review round (2 of 2)"],"at":1000,"last_typed_by":"agent"}"#,
    );
    w.put(
        "data/last.json",
        &rows(&[row(5, "working", ""), row(6, "working", "")]),
    );
    w.snaps(&[rows(&[row(5, "failed", ""), row(6, "failed", "")])]);
    let (lines, _) = w.run(0, &[]);
    assert!(
        of(&lines, 5)[0].contains(&format!(" allowed:nothing:until-{until}@1000 task:")),
        "{lines:?}"
    );
    assert!(
        of(&lines, 6)[0].contains(" allowed:write:merge:no-round:driven@1000 task:"),
        "{lines:?}"
    );
}

/// At most 12 briefs a block, the lines said at once first: a held line due with 12 urgent ones
/// is the 13th and reads `allowed:?`.
#[test]
fn twelve_briefs_a_block_the_urgent_first() {
    let w = Watch::new();
    let at = now() - 5;
    let held = r#"T7 g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#;
    w.put("data/held", &format!("7 1 short {at} {at} {held}\n"));
    let mut before = vec![row(7, "idle", "")];
    let mut after = vec![row(7, "idle", "")];
    for t in 20..32 {
        before.push(row(t, "working", ""));
        after.push(row(t, "blocked", ""));
    }
    w.put("data/last.json", &rows(&before));
    w.snaps(&[rows(&after)]);
    let (lines, _) = w.run(0, &[]);
    assert_eq!(w.calls().len(), 12, "{:?}", w.calls());
    assert!(!w.calls().contains(&"7".to_string()));
    assert!(of(&lines, 7)[0].contains(" allowed:? task:"), "{lines:?}");
    assert!(of(&lines, 25)[0].contains(" allowed:write:none@1000 task:"));
}

/// A usage limit keeps ` resets:` after its task; the tag goes before the task.
#[test]
fn a_limited_line_keeps_its_reset_after_the_task() {
    let w = Watch::new();
    w.exe("bin/cswap", "#!/bin/sh\nexit 0\n");
    w.put(
        "stub/screen",
        "  ⚠ Usage limit reached · resets 7:20am\n  continuing automatically at 7:20am\n",
    );
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&[rows(&[row(5, "idle", "")])]);
    let (lines, _) = w.run(0, &[]);
    assert_eq!(
        lines,
        [
            r#"T5 g1 claude working->limited "w" /tmp/x stop:? allowed:write:none@1000 task:12:implementing resets:7:20am"#
        ]
    );
}

/// A remote's line reads `allowed:?` with no brief asked, and its stream running on does not
/// hold the local block (the briefs are waited on by name).
#[test]
fn a_remote_line_is_unknown_and_its_stream_never_holds_the_block() {
    let w = Watch::new();
    w.put("stub/remotes", "ubox\n");
    w.put(
        "stub/remote-snaps",
        &format!(
            "{}\n{}\n",
            rows(&[row(2, "working", "")]),
            rows(&[row(2, "idle", "")])
        ),
    );
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&["sleep 1".into(), rows(&[row(5, "blocked", "")])]);
    let (lines, took) = w.run(1, &[("REMOTE_HOLD", "20")]);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("ubox/T2 ") && l.contains(" allowed:? task:")),
        "{lines:?}"
    );
    assert!(
        of(&lines, 5)[0].contains(" allowed:write:none@1000 "),
        "{lines:?}"
    );
    assert_eq!(w.calls(), ["5"]);
    assert!(took < Duration::from_secs(10), "{took:?}");
}

/// A brief that says the owner is at the tab until `until` once, and nobody after.
fn at_the_tab_once(w: &Watch, t: u32, until: i64) {
    w.exe(
        &format!("stub/brief-{t}.sh"),
        &format!(
            r#"#!/bin/bash
if [ ! -e "$STUB/asked-{t}" ]; then
    touch "$STUB/asked-{t}"
    echo '{{"allowed":null,"ending":"none","not_now":["write (a person is at this tab)"],"at":1000,"person_known":true,"person_until":{until}}}'
else
    cat "$STUB/brief.json"
fi
"#
        ),
    );
}

/// A stop of a tab the owner is at is held, not said, and said once his clock runs out, at the
/// next tick; a blocked line the same way.
#[test]
fn a_tab_the_owner_is_at_holds_its_stop_until_he_leaves() {
    for state in ["idle", "blocked"] {
        let w = Watch::new();
        at_the_tab_once(&w, 5, now() + 2);
        w.put("data/last.json", &rows(&[row(5, "working", "")]));
        w.snaps(&[rows(&[row(5, state, "")])]);
        let (lines, took) = w.run(13, &[]);
        assert_eq!(of(&lines, 5).len(), 1, "{state}: {lines:?}");
        let quiet = w.read("data/quiet.log");
        assert!(quiet.contains(" person: T5 until "), "{quiet}");
        assert!(quiet.contains(" person: said T5 "), "{quiet}");
        assert!(took >= Duration::from_secs(10), "said after a tick");
    }
}

/// A person hold that comes due while he still types is held again with the new due, never
/// marked said; a brief that fails keeps a known due.
#[test]
fn a_person_hold_renews_and_survives_a_failed_brief() {
    let w = Watch::new();
    let past = now() - 1;
    let line = r#"T5 g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#;
    w.put("data/held", &format!("5 1 person {past} {past} {line}\n"));
    let until = now() + 90;
    at_the_tab_once(&w, 5, until);
    w.put("data/last.json", &rows(&[row(5, "idle", "")]));
    w.snaps(&[rows(&[row(5, "idle", "")])]);
    let (lines, _) = w.run(12, &[]);
    assert!(of(&lines, 5).is_empty(), "{lines:?}");
    let held = w.read("data/held");
    assert!(held.starts_with(&format!("5 1 person {until} ")), "{held}");
    assert!(!w.read("data/said").contains("5 1 "));

    // The owner still there, then the agent blocks and its brief fails: the known due stands.
    let w = Watch::new();
    let until = now() + 100;
    w.put("data/held", &format!("5 1 person {until} {past} {line}\n"));
    w.put("stub/brief-5.exit", "1");
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&[rows(&[row(5, "blocked", "")])]);
    let (lines, _) = w.run(0, &[]);
    assert!(of(&lines, 5).is_empty(), "{lines:?}");
    let held = w.read("data/held");
    assert!(held.starts_with(&format!("5 1 person {until} ")), "{held}");
}

/// No other hold moves a person hold still ahead: a short line (a stop on its task) and a
/// routine one (a rest with no task) keep its class and due, only the text is new.
#[test]
fn no_other_hold_demotes_a_person_hold() {
    let until = now() + 100;
    let past = now() - 1;
    let line = r#"T5 g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#;
    let no_task = r#"{"terminal":5,"generation":1,"agent":true,"program":"claude","state":"STATE","name":"w","cwd":"/tmp/x"}"#;
    for (before, after) in [
        (row(5, "working", ""), row(5, "idle", "")),
        (
            no_task.replace("STATE", "working"),
            no_task.replace("STATE", "idle"),
        ),
    ] {
        let w = Watch::new();
        w.put("data/held", &format!("5 1 person {until} {past} {line}\n"));
        w.put("data/last.json", &rows(&[before]));
        w.snaps(&[rows(&[after])]);
        let (lines, _) = w.run(0, &[("QASS_SETTLE", "20")]);
        assert!(of(&lines, 5).is_empty(), "{lines:?}");
        let held = w.read("data/held");
        assert!(
            held.starts_with(&format!("5 1 person {until} {past} ")),
            "{held}"
        );
    }
}

/// A held line whose agent went back to work goes stale, its note naming what it was held as.
#[test]
fn a_stale_note_names_its_hold() {
    let w = Watch::new();
    let past = now() - 1;
    let line = r#"T5 g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#;
    w.put("data/held", &format!("5 1 short {past} {past} {line}\n"));
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&[rows(&[row(5, "working", "")])]);
    let (lines, _) = w.run(12, &[]);
    assert!(of(&lines, 5).is_empty(), "{lines:?}");
    assert!(w.read("data/quiet.log").contains(" stale short T5 g1 "));
    assert!(w.read("data/held").is_empty());
}

/// A person hold whose agent went back to work goes stale.
#[test]
fn a_person_hold_of_an_agent_back_at_work_goes_stale() {
    let w = Watch::new();
    let past = now() - 1;
    let line = r#"T5 g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#;
    w.put("data/held", &format!("5 1 person {past} {past} {line}\n"));
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&[rows(&[row(5, "working", "")])]);
    let (lines, _) = w.run(12, &[]);
    assert!(of(&lines, 5).is_empty(), "{lines:?}");
    assert!(w.read("data/quiet.log").contains(" person: stale T5 "));
    assert!(w.read("data/held").is_empty());
}

/// The same stop flapping (a screen-read work and rest, no new Stop) after qass had its line and
/// finished a turn after it. Times are seconds ago: the watcher drops said records over a week old.
#[derive(Clone, Copy)]
struct Repeat {
    stage: &'static str,
    said_stop: &'static str,
    said_gen: u32,
    stop: &'static str,
    qass: (&'static str, i64),
    write_at: Option<i64>,
    printed: i64,
}

impl Default for Repeat {
    fn default() -> Self {
        Self {
            stage: "implementing",
            said_stop: "100.1",
            said_gen: 1,
            stop: "100.1",
            qass: ("idle", 500),
            write_at: None,
            printed: 1000,
        }
    }
}

/// What the watcher prints of the flap, and its quiet.log.
fn repeat(r: Repeat) -> (Vec<String>, String, String) {
    let w = Watch::new();
    let stage_row = |state: &str, stop: &str| {
        let stop = if stop == "?" {
            String::new()
        } else {
            format!(r#","stop":"{stop}""#)
        };
        format!(
            r#"{{"terminal":5,"generation":1,"agent":true,"program":"claude","state":"{state}","name":"w","cwd":"/tmp/x","task":{{"id":12,"stage":"{}","claimed":false}}{stop}}}"#,
            r.stage
        )
    };
    let now = now();
    w.put(
        "data/said",
        &format!(
            "5 {} idle {} {} {} {}\n",
            r.said_gen,
            r.stage,
            now - 2000,
            r.said_stop,
            now - r.printed
        ),
    );
    if let Some(ago) = r.write_at {
        w.put("data/prompted.log", &format!("{} {QASS} T5\n", now - ago));
    }
    let qass = qass_row(r.qass.0, now - r.qass.1);
    w.put(
        "data/last.json",
        &rows(&[stage_row("idle", r.stop), qass.clone()]),
    );
    w.snaps(&[
        rows(&[stage_row("working", r.stop), qass.clone()]),
        rows(&[stage_row("idle", r.stop), qass]),
    ]);
    let (lines, _) = w.run(0, &[]);
    (lines, w.read("data/quiet.log"), w.read("data/held"))
}

/// A handled stop's repeat rides, noted with its print and rest times; every other case is
/// said: a qass turn that ended before the print, a write after it, qass still working, another
/// generation, a new stop, and no stop at all.
#[test]
fn only_a_handled_stop_rides() {
    let (lines, quiet, held) = repeat(Repeat::default());
    assert!(of(&lines, 5).is_empty(), "{lines:?}");
    assert!(
        quiet.contains(" ride: stop handled T5 g1 100.1 printed ")
            && quiet.contains(" T5 g1 claude "),
        "{quiet}"
    );
    assert!(held.starts_with("5 1 ride "), "{held}");

    for (case, r) in [
        (
            "rested before the print",
            Repeat {
                printed: 100,
                ..Repeat::default()
            },
        ),
        (
            "written since",
            Repeat {
                write_at: Some(700),
                ..Repeat::default()
            },
        ),
        (
            "qass working",
            Repeat {
                qass: ("working", 500),
                ..Repeat::default()
            },
        ),
        (
            "another generation",
            Repeat {
                said_gen: 0,
                ..Repeat::default()
            },
        ),
        (
            "a new stop",
            Repeat {
                said_stop: "90.1",
                ..Repeat::default()
            },
        ),
        (
            "no stop",
            Repeat {
                stop: "?",
                said_stop: "?",
                ..Repeat::default()
            },
        ),
    ] {
        let (lines, quiet, _) = repeat(r);
        assert_eq!(of(&lines, 5).len(), 1, "{case}: {lines:?}");
        assert!(!quiet.contains("ride: stop handled"), "{case}: {quiet}");
    }
}

/// A blocked agent is said at once, its stop handled or not.
#[test]
fn a_blocked_line_is_said_whatever_its_stop() {
    let w = Watch::new();
    let now = now();
    w.put(
        "data/said",
        &format!("5 1 idle implementing {} 100.1 {}\n", now - 200, now - 100),
    );
    let qass = qass_row("idle", now - 50);
    w.put(
        "data/last.json",
        &rows(&[row(5, "idle", r#","stop":"100.1""#), qass.clone()]),
    );
    w.snaps(&[rows(&[row(5, "blocked", r#","stop":"100.1""#), qass])]);
    let (lines, _) = w.run(0, &[]);
    assert_eq!(of(&lines, 5).len(), 1, "{lines:?}");
}

/// Two Stops in a row with the agent at rest all along each make a line from `working`: short on
/// an open task (said now), routine with none (held).
#[test]
fn a_new_stop_at_rest_is_a_line() {
    let w = Watch::new();
    w.put(
        "data/last.json",
        &rows(&[row(5, "idle", r#","stop":"1.1""#)]),
    );
    w.snaps(&[
        rows(&[row(5, "idle", r#","stop":"2.2""#)]),
        rows(&[row(5, "idle", r#","stop":"3.3""#)]),
    ]);
    let (lines, _) = w.run(0, &[]);
    let l = of(&lines, 5);
    assert_eq!(l.len(), 2, "{lines:?}");
    assert!(l[0].starts_with("T5 g1 claude working->idle ") && l[0].contains(" stop:2.2 "));
    assert!(l[1].contains(" stop:3.3 "), "{lines:?}");

    let w = Watch::new();
    let no_task = |stop: &str| {
        format!(
            r#"{{"terminal":5,"generation":1,"agent":true,"program":"claude","state":"idle","name":"w","cwd":"/tmp/x","stop":"{stop}"}}"#
        )
    };
    w.put("data/last.json", &rows(&[no_task("1.1")]));
    w.snaps(&[rows(&[no_task("2.2")])]);
    let (lines, _) = w.run(0, &[]);
    assert!(of(&lines, 5).is_empty(), "{lines:?}");
    assert!(w.read("data/held").starts_with("5 1 routine "));
    // A stop cleared by new work makes no line.
    let w = Watch::new();
    w.put(
        "data/last.json",
        &rows(&[row(5, "idle", r#","stop":"1.1""#)]),
    );
    w.snaps(&[rows(&[row(5, "idle", "")])]);
    let (lines, _) = w.run(0, &[]);
    assert!(of(&lines, 5).is_empty(), "{lines:?}");
}

/// In waiting-human only a handled same stop rides; a new stop, no stop, qass still working and
/// a write since go on to the classes below it (long, or short after qass's write).
#[test]
fn a_waiting_human_repeat_rides_only_on_a_handled_stop() {
    let human = Repeat {
        stage: "waiting-human",
        ..Repeat::default()
    };
    let (_, quiet, held) = repeat(human);
    assert!(quiet.contains("ride: stop handled"), "{quiet}");
    assert!(held.starts_with("5 1 ride "), "{held}");
    for (case, r) in [
        (
            "a new stop",
            Repeat {
                said_stop: "90.1",
                ..human
            },
        ),
        (
            "no stop",
            Repeat {
                stop: "?",
                said_stop: "?",
                ..human
            },
        ),
        (
            "qass working",
            Repeat {
                qass: ("working", 500),
                ..human
            },
        ),
        (
            "written since",
            Repeat {
                write_at: Some(700),
                ..human
            },
        ),
    ] {
        let (_, quiet, held) = repeat(r);
        assert!(!quiet.contains("ride: stop handled"), "{case}: {quiet}");
        assert!(!held.starts_with("5 1 ride "), "{case}: {held}");
    }
}

/// With QASS_QUIET=0 nothing is held: a tab the owner is at goes out at once, its tag saying
/// until when.
#[test]
fn quiet_off_says_the_owner_at_the_tab_in_the_tag() {
    let w = Watch::new();
    let until = now() + 60;
    at_the_tab_once(&w, 5, until);
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&[rows(&[row(5, "idle", "")])]);
    let (lines, _) = w.run(0, &[("QASS_QUIET", "0")]);
    assert_eq!(
        lines,
        [format!(
            r#"T5 g1 claude working->idle "w" /tmp/x stop:? allowed:nothing:until-{until}@1000 task:12:implementing"#
        )]
    );
}

/// An agent row in task stage `stage` (or no task with "-"), with its stop when not "?".
fn staged(state: &str, stage: &str, stop: &str) -> String {
    let task = if stage == "-" {
        String::new()
    } else {
        format!(r#","task":{{"id":12,"stage":"{stage}","claimed":false}}"#)
    };
    let stop = if stop == "?" {
        String::new()
    } else {
        format!(r#","stop":"{stop}""#)
    };
    format!(
        r#"{{"terminal":5,"generation":1,"agent":true,"program":"claude","state":"{state}","name":"w","cwd":"/tmp/x"{task}{stop}}}"#
    )
}

/// A daemon that went away with an agent at work is said at once, as it was before the briefs.
#[test]
fn a_daemon_gone_is_said_in_quiet_mode() {
    let w = Watch::new();
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&["[]".into()]);
    let (lines, _) = w.run(0, &[]);
    assert_eq!(lines, ["daemon-gone"]);
    assert!(
        w.calls().is_empty(),
        "no brief for a line that is no agent's"
    );
}

/// A remote's link down and back up are said, with a held agent line riding along, which alone
/// gets a brief and a said record.
#[test]
fn a_link_down_and_up_are_said_with_held_agents() {
    let w = Watch::new();
    let line = r#"T7 g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#;
    let (due, at) = (now() + 500, now() - 5);
    w.put("data/held", &format!("7 1 long {due} {at} {line}\n"));
    w.put("data/last.json", &rows(&[row(7, "idle", "")]));
    w.put("stub/remotes", "ubox\n");
    w.put(
        "stub/remote-snaps",
        &format!("{}\n", rows(&[row(2, "idle", "")])),
    );
    w.snaps(&[rows(&[row(7, "idle", "")])]);
    let (lines, _) = w.run(18, &[("REMOTE_HOLD", "0"), ("QASS_LINKDOWN", "1")]);
    let down = lines.iter().position(|l| l == "ubox/link-down");
    let up = lines.iter().position(|l| l == "ubox/link-up");
    assert!(down.is_some() && up > down, "{lines:?}");
    assert!(
        of(&lines, 7)[0].contains(" allowed:write:none@1000 "),
        "{lines:?}"
    );
    assert_eq!(w.calls(), ["7"]);
    let said = w.read("data/said");
    assert!(
        said.starts_with("7 1 idle ") && !said.contains("link"),
        "{said}"
    );
}

/// When every line that wakes is held again for a person at the tab, nothing goes out: a ride
/// and a long not yet due stay held, never a wake of their own.
#[test]
fn a_person_rehold_leaves_rides_and_longs_held() {
    let w = Watch::new();
    let (past, later) = (now() - 1, now() + 500);
    let until = now() + 100;
    let held =
        |t: u32| format!(r#"T{t} g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#);
    w.put(
        "data/held",
        &format!(
            "5 1 person {past} {past} {}\n7 1 ride {later} {past} {}\n8 1 long {later} {past} {}\n",
            held(5),
            held(7),
            held(8)
        ),
    );
    w.put(
        "stub/brief-5.json",
        &format!(
            r#"{{"allowed":null,"ending":"none","not_now":["write (a person is at this tab)"],"at":1000,"person_known":true,"person_until":{until}}}"#
        ),
    );
    let r = rows(&[row(5, "idle", ""), row(7, "idle", ""), row(8, "idle", "")]);
    w.put("data/last.json", &r);
    w.snaps(&[r]);
    let (lines, _) = w.run(12, &[]);
    assert!(lines.is_empty(), "{lines:?}");
    let held = w.read("data/held");
    assert!(held.contains(&format!("5 1 person {until} ")), "{held}");
    assert!(
        held.contains("7 1 ride ") && held.contains("8 1 long "),
        "{held}"
    );
    assert!(w.read("data/said").is_empty());
}

/// A routine stop (no task) qass already handled rides as well, noted, rather than waking qass
/// at the end of the routine hold.
#[test]
fn a_handled_routine_stop_rides() {
    let w = Watch::new();
    let now = now();
    w.put(
        "data/said",
        &format!("5 1 idle - {} 100.1 {}\n", now - 2000, now - 1000),
    );
    let qass = qass_row("idle", now - 500);
    w.put(
        "data/last.json",
        &rows(&[staged("idle", "-", "100.1"), qass.clone()]),
    );
    w.snaps(&[
        rows(&[staged("working", "-", "100.1"), qass.clone()]),
        rows(&[staged("idle", "-", "100.1"), qass]),
    ]);
    let (lines, _) = w.run(0, &[]);
    assert!(lines.is_empty(), "{lines:?}");
    assert!(
        w.read("data/held").starts_with("5 1 ride "),
        "{}",
        w.read("data/held")
    );
    assert!(
        w.read("data/quiet.log")
            .contains(" ride: stop handled T5 g1 100.1 ")
    );
}

/// The said record keeps when a held line went out, not when it first came: a qass turn that
/// ended between the two does not make its repeat handled.
#[test]
fn a_held_delivery_records_its_print_time() {
    let w = Watch::new();
    let start = now();
    at_the_tab_once(&w, 5, start + 2);
    let qass = qass_row("idle", start + 1);
    w.put(
        "data/last.json",
        &rows(&[row(5, "working", r#","stop":"100.1""#), qass.clone()]),
    );
    w.snaps(&[
        rows(&[row(5, "idle", r#","stop":"100.1""#), qass.clone()]),
        "sleep 12".into(),
        rows(&[row(5, "working", r#","stop":"100.1""#), qass.clone()]),
        rows(&[row(5, "idle", r#","stop":"100.1""#), qass]),
    ]);
    let (lines, _) = w.run(1, &[]);
    assert_eq!(of(&lines, 5).len(), 2, "{lines:?}");
    let quiet = w.read("data/quiet.log");
    assert!(!quiet.contains("ride: stop handled"), "{quiet}");
    // The repeat's note carries the first print's time: after qass's rest, not its arrival.
    let note = quiet
        .lines()
        .find(|l| l.contains(" print: T5 g1 100.1 repeat:yes "))
        .expect("the repeat's print note");
    assert!(note.contains(" handled:no "), "{note}");
    let printed: i64 = note
        .split_whitespace()
        .skip_while(|w| *w != "printed")
        .nth(1)
        .and_then(|w| w.parse().ok())
        .expect("a print time");
    assert!(printed > start + 5, "{note}");
}

/// A ride (a handled stop's repeat) and a long (a new stop waiting on the owner) put into a tab
/// the owner is at keep its person hold.
#[test]
fn a_ride_or_long_put_keeps_a_person_hold() {
    for (stage, said_stop) in [("implementing", "100.1"), ("waiting-human", "90.1")] {
        let w = Watch::new();
        let now = now();
        let until = now + 100;
        let line = r#"T5 g1 claude working->idle "w" /tmp/x stop:? task:12:implementing"#;
        w.put(
            "data/held",
            &format!("5 1 person {until} {} {line}\n", now - 50),
        );
        w.put(
            "data/said",
            &format!(
                "5 1 idle {stage} {} {said_stop} {}\n",
                now - 2000,
                now - 1000
            ),
        );
        let qass = qass_row("idle", now - 500);
        w.put(
            "data/last.json",
            &rows(&[staged("idle", stage, "100.1"), qass.clone()]),
        );
        w.snaps(&[
            rows(&[staged("working", stage, "100.1"), qass.clone()]),
            rows(&[staged("idle", stage, "100.1"), qass]),
        ]);
        let (lines, _) = w.run(0, &[]);
        assert!(of(&lines, 5).is_empty(), "{stage}: {lines:?}");
        let held = w.read("data/held");
        assert!(
            held.starts_with(&format!("5 1 person {until} {} ", now - 50)),
            "{stage}: {held}"
        );
    }
}

/// Terminal 5 at rest on stop `stop` with `n` things running in the background.
fn resting(stop: &str, n: u32) -> String {
    let bg = if n > 0 {
        format!(r#","background":{n}"#)
    } else {
        String::new()
    };
    row(5, "idle", &format!(r#","stop":"{stop}"{bg}"#))
}

/// Terminal 5's stop as said, `n` things still running in the background (`bg:<n>` above 0).
fn rest_line(stop: &str, n: u32) -> String {
    let bg = if n > 0 {
        format!(" bg:{n}")
    } else {
        String::new()
    };
    format!(
        r#"T5 g1 claude working->idle "w" /tmp/x stop:{stop}{bg} allowed:write:none@1000 task:12:implementing"#
    )
}

/// A worker at rest with work in the background wakes nobody; once none runs, the tick lets it
/// go and it is said once.
#[test]
fn a_rest_with_background_work_is_said_once_none_runs() {
    let w = Watch::new();
    w.snaps(&[rows(&[row(5, "working", "")]), rows(&[resting("1.1", 2)])]);
    let (lines, _) = w.run(0, &[]);
    assert!(lines.is_empty(), "{lines:?}");
    assert!(w.read("data/quiet.log").contains("bg: held 2"));
    w.snaps(&[rows(&[resting("1.1", 0)])]);
    let (lines, _) = w.run(11, &[]);
    assert_eq!(lines, [rest_line("1.1", 0)]);
    assert!(w.read("data/bghold").is_empty());
}

/// Fewer running than when the hold began is said too, once.
#[test]
fn a_background_count_that_drops_is_said_once() {
    let w = Watch::new();
    w.snaps(&[
        rows(&[row(5, "working", "")]),
        rows(&[resting("1.1", 2)]),
        rows(&[resting("1.1", 1)]),
    ]);
    let (lines, _) = w.run(11, &[]);
    assert_eq!(lines, [rest_line("1.1", 1)]);
    // Held at its stop, so the one line is the drop.
    assert!(w.read("data/quiet.log").contains("bg: held 2"));
    assert!(w.read("data/bghold").is_empty());
}

/// A job that outlasts QASS_BG_LIMIT is said once on the tick, and not again on the next one.
#[test]
fn a_background_hold_past_its_limit_is_said_once() {
    let w = Watch::new();
    w.snaps(&[rows(&[row(5, "working", "")]), rows(&[resting("1.1", 2)])]);
    let (lines, _) = w.run(21, &[("QASS_BG_LIMIT", "2")]);
    assert_eq!(lines, [rest_line("1.1", 2)]);
    assert!(lines[0].contains(" bg:2 "), "{lines:?}");
    assert!(w.read("data/quiet.log").contains("bg: limit"));
    assert!(
        w.read("data/bghold").starts_with("5 1 2 ") && w.read("data/bghold").ends_with(" spent\n")
    );
}

/// A job that never ends (a dev server): past the limit the hold is spent, so each later stop
/// with as many running takes its usual class at once, with no second hold.
#[test]
fn a_spent_hold_holds_no_later_stop() {
    let w = Watch::new();
    w.snaps(&[
        rows(&[row(5, "working", "")]),
        rows(&[resting("1.1", 1)]),
        "sleep 11".into(),
        rows(&[row(5, "working", "")]),
        rows(&[resting("1.2", 1)]),
        rows(&[row(5, "working", "")]),
        rows(&[resting("1.3", 1)]),
    ]);
    let (lines, _) = w.run(1, &[("QASS_BG_LIMIT", "2")]);
    assert_eq!(
        lines,
        [
            rest_line("1.1", 1),
            rest_line("1.2", 1),
            rest_line("1.3", 1)
        ]
    );
    assert_eq!(w.read("data/quiet.log").matches("bg: held").count(), 1);
    assert!(w.read("data/bghold").ends_with(" spent\n"));
}

/// A job that ends wakes the agent, which works and stops again: with the settle a real watcher
/// has, the drop the tick let go and that stop are one line, not two.
#[test]
fn one_job_end_is_one_line_with_the_settle() {
    let w = Watch::new();
    w.snaps(&[
        rows(&[row(5, "working", "")]),
        rows(&[resting("1.1", 2)]),
        rows(&[resting("1.1", 0)]),
        "sleep 11".into(),
        rows(&[row(5, "working", "")]),
        "sleep 1".into(),
        rows(&[resting("1.2", 0)]),
    ]);
    let (lines, _) = w.run(30, &[("QASS_SETTLE", "20")]);
    assert_eq!(lines, [rest_line("1.2", 0)]);
    assert!(w.read("data/quiet.log").contains("bg: let go"));
}

/// Terminal `t` at rest on a task in `stage`, claimed or not, with two jobs running.
fn task_row(t: u32, stage: &str, claimed: bool) -> String {
    format!(
        r#"{{"terminal":{t},"generation":1,"agent":true,"program":"claude","state":"idle","name":"w","cwd":"/tmp/x","task":{{"id":12,"stage":"{stage}","claimed":{claimed}}},"stop":"1.1","background":2}}"#
    )
}

/// What is said at once stays so with work in the background (a block, a failure), and a repeat
/// on a task waiting on the owner or claimed done is never held for it.
#[test]
fn urgent_waiting_and_claimed_rows_are_never_background_held() {
    let w = Watch::new();
    let bg = r#","stop":"1.1","background":2"#;
    let at = now() - 60;
    w.put(
        "data/said",
        &format!(
            "7 1 idle waiting-human {at} 1.0 {at}\n8 1 idle implementing:claimed {at} 1.0 {at}\n"
        ),
    );
    let working: Vec<String> = [5, 6, 7, 8]
        .iter()
        .map(|t| row(*t, "working", ""))
        .collect();
    w.put("data/last.json", &rows(&working));
    w.snaps(&[rows(&[
        row(5, "blocked", bg),
        row(6, "failed", bg),
        task_row(7, "waiting-human", false),
        task_row(8, "implementing", true),
    ])]);
    let (lines, _) = w.run(0, &[]);
    assert_eq!(of(&lines, 5).len(), 1, "{lines:?}");
    assert_eq!(of(&lines, 6).len(), 1, "{lines:?}");
    assert!(
        w.read("data/bghold").is_empty(),
        "{}",
        w.read("data/bghold")
    );
}

/// The hold and its start outlive a restart of the watcher, and a new stop keeps that start.
#[test]
fn a_background_hold_outlives_a_restart() {
    let w = Watch::new();
    w.snaps(&[rows(&[row(5, "working", "")]), rows(&[resting("1.1", 2)])]);
    let (lines, _) = w.run(0, &[]);
    assert!(lines.is_empty(), "{lines:?}");
    std::thread::sleep(Duration::from_secs(3));
    w.snaps(&[rows(&[resting("1.2", 2)])]);
    let (lines, _) = w.run(11, &[("QASS_BG_LIMIT", "3")]);
    assert_eq!(lines, [rest_line("1.2", 2)]);
}

/// A worker another agent drives (routine before this) is held too, and said when its job ends.
#[test]
fn a_driven_worker_with_background_work_is_held_and_said() {
    let w = Watch::new();
    w.put("data/prompted.log", &format!("{} 8 T5\n", now()));
    let other = row(8, "working", "");
    w.snaps(&[
        rows(&[row(5, "working", ""), other.clone()]),
        rows(&[resting("1.1", 2), other.clone()]),
        rows(&[resting("1.1", 0), other]),
    ]);
    let (lines, _) = w.run(11, &[]);
    assert_eq!(of(&lines, 5), [&rest_line("1.1", 0)]);
    assert!(w.read("data/quiet.log").contains("bg: held 2"));
}

/// A person hold that runs out while the job runs says nothing: the hold took its line, and its
/// release meets the brief again.
#[test]
fn a_person_hold_gives_way_to_the_background_hold() {
    let w = Watch::new();
    let line = r#"T5 g1 claude working->idle "w" /tmp/x stop:1.0 task:12:implementing"#;
    w.put(
        "data/held",
        &format!("5 1 person {} {} {line}\n", now() + 3, now() - 5),
    );
    w.put("data/last.json", &rows(&[row(5, "working", "")]));
    w.snaps(&[rows(&[resting("1.1", 2)])]);
    let (lines, _) = w.run(11, &[]);
    assert!(lines.is_empty(), "{lines:?}");
    assert!(
        !w.read("data/held").contains("5 1 "),
        "{}",
        w.read("data/held")
    );
    assert!(w.read("data/bghold").starts_with("5 1 2 "));
}

/// A remote worker is held in its own file and said with its machine's name; a resume (the
/// owner turned watching on again) drops every hold.
#[test]
fn a_remote_background_hold_and_a_resume() {
    let w = Watch::new();
    w.put("stub/remotes", "ubox\n");
    let remote = |state: &str, extra: &str| row(2, state, extra);
    w.put(
        "stub/remote-snaps",
        &format!(
            "{}\n{}\nsleep 1\n{}\n",
            rows(&[remote("working", "")]),
            rows(&[remote("idle", r#","stop":"1.1","background":2"#)]),
            rows(&[remote("idle", r#","stop":"1.1""#)])
        ),
    );
    let local = rows(&[row(5, "working", "")]);
    w.put("data/last.json", &local);
    w.snaps(std::slice::from_ref(&local));
    let (lines, _) = w.run(11, &[("REMOTE_HOLD", "20")]);
    let ubox: Vec<&String> = lines.iter().filter(|l| l.starts_with("ubox/T2 ")).collect();
    assert_eq!(ubox.len(), 1, "{lines:?}");
    assert!(w.read("data/quiet.log").contains("bg: held 2"));
    w.put("data/bghold", &format!("5 1 2 {} working\n", now()));
    w.put("data/bghold-ubox", &format!("2 1 2 {} working\n", now()));
    w.put("data/watch", "resume 77\n");
    w.put("stub/remotes", "");
    w.snaps(&[local]);
    let (lines, _) = w.run(0, &[]);
    assert!(lines.is_empty(), "{lines:?}");
    assert!(!w.path("data/bghold").exists() && !w.path("data/bghold-ubox").exists());
}
