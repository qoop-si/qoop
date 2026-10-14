//! Pictures for a tab on another machine.
//!
//! A picture dropped into a remote tab arrives as a path on this machine, and ctrl+v makes
//! the agent there read a clipboard that is not ours: either way the agent cannot open it.
//! So the picture is made small here, kept by the daemon that runs the tab (`SaveUpload`),
//! and the path it answers is pasted instead, which the agent attaches as if it were local.
//!
//! Small on purpose: an upload is one control message on the link that also carries every
//! keystroke to that machine, and one that takes longer than the reassembly deadline drops
//! the whole link.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse};

/// The kinds a drop sends as a picture, by lowercased extension. All of them leave as JPEG.
const KINDS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp",
];

/// The kinds the agent attaches as they are: a picture of one of these that is small enough
/// goes unchanged, so a cropped screenshot keeps its sharp text.
const AS_IS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// The longest edge a picture is sent with: the one Claude Code scales a picture to anyway.
#[cfg(target_os = "macos")]
const EDGE: u32 = 1568;

/// The most a prepared picture may weigh. Encoded it is about 700 KB, which arrives inside
/// the 10 s the daemon gives one message even on a 1 Mbit/s uplink.
pub const MOST: usize = 512 << 10;

/// What every picture's request id starts with.
pub const ID: &str = "picture-";

/// How long a picture may take before the keys held behind it are let go.
const PATIENCE: Duration = Duration::from_secs(15);

/// Whether `path` names a picture a drop should send, by its extension.
pub fn is_picture(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| KINDS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Whether a read is the one ctrl+v for a remote agent: the key alone, so a paste or a
/// burst that happens to hold the byte is typed as it is.
pub fn is_ctrl_v(bytes: &[u8]) -> bool {
    bytes == [0x16]
}

/// A private file name for a picture on its way: the per-user temporary directory, which
/// on macOS is the person's own.
#[cfg(target_os = "macos")]
fn scratch(ext: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("qoop-picture-{}-{n}.{ext}", std::process::id()))
}

/// A picture ready to send: its kind as the daemon names it, and its bytes.
pub type Prepared = (&'static str, Vec<u8>);

/// The picture at `path` as it is, when the agent reads its kind and it is small enough.
fn as_is(path: &Path) -> Option<Prepared> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let kind = match ext.as_str() {
        "jpeg" => "jpg",
        other => AS_IS.iter().copied().find(|k| *k == other)?,
    };
    if std::fs::metadata(path).ok()?.len() > MOST as u64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    (bytes.len() <= MOST).then_some((kind, bytes))
}

/// The picture at `path`, small enough to send: as it is when it can be, else as JPEG at
/// most `EDGE` on its longest side, never enlarged. Runs on the blocking pool.
#[cfg(target_os = "macos")]
pub fn prepare(path: &Path) -> Result<Prepared, String> {
    if let Some(ready) = as_is(path) {
        return Ok(ready);
    }
    let unreadable = || format!("{} could not be read as a picture", path.display());
    let sips = |args: &[std::ffi::OsString]| {
        std::process::Command::new("sips")
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    let mut ask: Vec<std::ffi::OsString> = ["-g", "pixelWidth", "-g", "pixelHeight"]
        .map(Into::into)
        .into();
    ask.push(path.into());
    let size = sips(&ask).ok_or_else(unreadable)?;
    // `  pixelWidth: 4032` and its height, after a line naming the file.
    let edge = String::from_utf8_lossy(&size.stdout)
        .lines()
        .filter(|line| line.trim_start().starts_with("pixel"))
        .filter_map(|line| line.rsplit(' ').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    // A noisy photo can stay over `MOST` at the first setting: one smaller try, then no.
    for (quality, most_edge) in [("80", EDGE), ("60", 1024)] {
        let out = scratch("jpg");
        let mut args: Vec<std::ffi::OsString> = Vec::new();
        if edge > most_edge {
            args.extend(["-Z".into(), most_edge.to_string().into()]);
        }
        args.extend(["-s", "format", "jpeg", "-s", "formatOptions", quality].map(Into::into));
        args.extend([path.into(), "--out".into(), out.clone().into()]);
        let made = sips(&args);
        let bytes = std::fs::read(&out);
        let _ = std::fs::remove_file(&out);
        match (made, bytes) {
            (Some(_), Ok(bytes)) if bytes.len() <= MOST => return Ok(("jpg", bytes)),
            (Some(_), Ok(_)) => continue,
            _ => return Err(unreadable()),
        }
    }
    Err("the picture is too big to send".into())
}

/// Elsewhere there is no `sips`: only a picture the agent reads as it is goes.
/// ponytail: no resizing off macOS; add one if a Linux interface sends big pictures.
#[cfg(not(target_os = "macos"))]
pub fn prepare(path: &Path) -> Result<Prepared, String> {
    as_is(path).ok_or_else(|| format!("{} cannot be sent from here", path.display()))
}

/// The clipboard's picture, made small enough to send, or `None` when it holds none.
/// Runs on the blocking pool: macOS takes about a second to hand a picture over.
#[cfg(target_os = "macos")]
pub fn clipboard() -> Result<Option<Prepared>, String> {
    let png = scratch("png");
    // The way Claude Code reads it on macOS: the clipboard as PNG, written to a file.
    let script = format!(
        "try\nset d to the clipboard as «class PNGf»\non error\nreturn \"none\"\nend try\n\
         set f to open for access POSIX file \"{}\" with write permission\nset eof f to 0\n\
         write d to f\nclose access f\nreturn \"ok\"",
        png.display()
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if String::from_utf8_lossy(&out.stdout).trim() != "ok" {
        let _ = std::fs::remove_file(&png);
        return Ok(None);
    }
    let picture = prepare(&png);
    let _ = std::fs::remove_file(&png);
    picture.map(Some)
}

/// ponytail: macOS only; ctrl+v reaches the remote agent unchanged elsewhere.
#[cfg(not(target_os = "macos"))]
pub fn clipboard() -> Result<Option<Prepared>, String> {
    Ok(None)
}

/// One piece of what a drop or a ctrl+v types once its pictures are kept.
#[derive(Debug)]
enum Part {
    /// Typed as it is: a path that is not a picture, a link.
    Text(String),
    /// A picture under its request id. `key` is a ctrl+v's, which types the key itself when
    /// the clipboard held no picture.
    Picture { id: String, key: bool, at: At },
}

#[derive(Debug, PartialEq)]
enum At {
    /// Being made small, or on its way to the daemon.
    Going,
    /// Kept there, at this path.
    Kept(String),
    /// Left out, as said in the message.
    Dropped,
    /// A ctrl+v found no picture on the clipboard.
    Nothing,
}

#[derive(Debug)]
struct Pending {
    parts: Vec<Part>,
    /// Keys typed into the terminal meanwhile, sent after the paste so they keep their order.
    held: Vec<u8>,
    /// What went wrong on the way, said once the batch is done.
    said: Option<String>,
    since: Instant,
}

/// What a finished batch leaves to send to its terminal, in this order.
#[derive(Debug, PartialEq, Default)]
pub struct Done {
    pub terminal: u32,
    /// Pasted, so the agent attaches the paths in it.
    pub paste: Option<String>,
    /// Typed after the paste.
    pub input: Vec<u8>,
    /// What to tell the person.
    pub message: Option<String>,
}

/// The pictures on their way, one batch per terminal that later ones join.
#[derive(Debug, Default)]
pub struct Pictures {
    next: u64,
    pending: BTreeMap<u32, Pending>,
}

impl Pictures {
    /// Whether keys for `terminal` must wait, and if so, holds them.
    pub fn hold(&mut self, terminal: u32, bytes: &[u8]) -> bool {
        match self.pending.get_mut(&terminal) {
            Some(p) => {
                p.held.extend_from_slice(bytes);
                true
            }
            None => false,
        }
    }

    pub fn busy(&self, terminal: u32) -> bool {
        self.pending.contains_key(&terminal)
    }

    fn id(&mut self) -> String {
        self.next += 1;
        format!("{ID}{}", self.next)
    }

    /// Whether exactly one picture is on its way: an answer naming no request is its.
    pub fn going_one(&self) -> bool {
        self.going().len() == 1
    }

    /// A drop of `items` (each as typed, with its file when it is a local picture) into a
    /// remote `terminal`. Answers the pictures to prepare, by id; none means type it now.
    pub fn drop(
        &mut self,
        terminal: u32,
        items: Vec<(String, Option<PathBuf>)>,
    ) -> Vec<(String, PathBuf)> {
        let mut prepare = Vec::new();
        let mut parts = Vec::new();
        for (typed, file) in items {
            match file {
                Some(file) => {
                    let id = self.id();
                    prepare.push((id.clone(), file));
                    parts.push(Part::Picture {
                        id,
                        key: false,
                        at: At::Going,
                    });
                }
                None => parts.push(Part::Text(typed)),
            }
        }
        // Text alone is typed now, unless an earlier batch is still going: then it waits
        // its turn behind it.
        if !prepare.is_empty() || self.busy(terminal) {
            self.join(terminal, parts);
        }
        prepare
    }

    /// A ctrl+v into a remote agent `terminal`: answers the id the clipboard is read under.
    pub fn ctrl_v(&mut self, terminal: u32) -> String {
        let id = self.id();
        let part = Part::Picture {
            id: id.clone(),
            key: true,
            at: At::Going,
        };
        self.join(terminal, vec![part]);
        id
    }

    /// Keys typed after an earlier batch keep their place: a later one joins it.
    fn join(&mut self, terminal: u32, parts: Vec<Part>) {
        let p = self.pending.entry(terminal).or_insert_with(|| Pending {
            parts: Vec::new(),
            held: Vec::new(),
            said: None,
            since: Instant::now(),
        });
        // ponytail: keys held before this one go after the whole batch; a drop then keys
        // then a drop keeps the keys last.
        p.parts.extend(parts);
        // The wait is for the latest picture, not the first.
        p.since = Instant::now();
    }

    /// The picture `id` was made small (or the clipboard read): answers the request that
    /// sends it, or what is finished.
    pub fn prepared(
        &mut self,
        id: &str,
        picture: Result<Option<Prepared>, String>,
    ) -> (Option<ControlRequest>, Option<Done>) {
        let Some(terminal) = self.terminal_of(id) else {
            return (None, None);
        };
        match picture {
            Ok(Some((ext, bytes))) => {
                let request = ControlRequest::save_upload(id.to_string(), ext, &bytes, terminal);
                (Some(request), None)
            }
            Ok(None) => (None, self.settle(id, At::Nothing, None)),
            Err(why) => (None, self.settle(id, At::Dropped, Some(why))),
        }
    }

    /// An answer that may be for one of these pictures: answers what is finished.
    pub fn answered(&mut self, response: &ControlResponse) -> Option<Done> {
        match response {
            ControlResponse::Uploaded { id, path } => self.settle(id, At::Kept(path.clone()), None),
            ControlResponse::Failed { id, reason } => {
                // A daemon from before 2026-09-27 cannot name the request it refuses: the
                // one picture on its way is the one it means.
                let id = match self.going().as_slice() {
                    [only] if id.is_empty() => only.clone(),
                    _ => id.clone(),
                };
                self.settle(
                    &id,
                    At::Dropped,
                    Some(format!("picture not sent: {reason}")),
                )
            }
            _ => None,
        }
    }

    /// Batches that waited too long, finished with what they have, so held keys go.
    pub fn expired(&mut self, now: Instant) -> Vec<Done> {
        let late: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, p)| now.duration_since(p.since) >= PATIENCE)
            .map(|(t, _)| *t)
            .collect();
        late.into_iter()
            .filter_map(|terminal| {
                let mut p = self.pending.remove(&terminal)?;
                p.said = Some("picture not sent: no answer".into());
                Some(finish(terminal, p))
            })
            .collect()
    }

    fn going(&self) -> Vec<String> {
        self.pending
            .values()
            .flat_map(|p| &p.parts)
            .filter_map(|part| match part {
                Part::Picture {
                    id, at: At::Going, ..
                } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    fn terminal_of(&self, id: &str) -> Option<u32> {
        self.pending
            .iter()
            .find(|(_, p)| p.parts.iter().any(|part| owns(part, id)))
            .map(|(t, _)| *t)
    }

    fn settle(&mut self, id: &str, to: At, said: Option<String>) -> Option<Done> {
        let terminal = self.terminal_of(id)?;
        let p = self.pending.get_mut(&terminal)?;
        if let Some(Part::Picture { at, .. }) = p.parts.iter_mut().find(|part| owns(part, id)) {
            *at = to;
        }
        // Moving along: three pictures over a slow link wait for the last one, not all
        // three inside one wait.
        p.since = Instant::now();
        if said.is_some() {
            p.said = said;
        }
        let open = p
            .parts
            .iter()
            .any(|part| matches!(part, Part::Picture { at: At::Going, .. }));
        if open {
            return None;
        }
        let p = self.pending.remove(&terminal)?;
        Some(finish(terminal, p))
    }
}

fn owns(part: &Part, wanted: &str) -> bool {
    matches!(part, Part::Picture { id, .. } if id == wanted)
}

fn finish(terminal: u32, p: Pending) -> Done {
    let mut paste = String::new();
    let mut input = Vec::new();
    for part in p.parts {
        match part {
            Part::Text(text) => paste.push_str(&text),
            // A path the agent reads as one word, the way a drop types one.
            Part::Picture {
                at: At::Kept(path), ..
            } => paste.push_str(&format!("{path} ")),
            // ctrl+v with nothing on the clipboard is the key itself: the agent says there is
            // no picture, or the program means something else by it.
            Part::Picture {
                key: true,
                at: At::Nothing,
                ..
            } => input.push(0x16),
            Part::Picture { .. } => {}
        }
    }
    input.extend(p.held);
    Done {
        terminal,
        paste: (!paste.is_empty()).then_some(paste),
        input,
        message: p.said,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_known_by_extension_in_any_case() {
        for path in [
            "/a/b.png",
            "/a/B.JPEG",
            "/a/c.jpg",
            "/a/IMG_1.HEIC",
            "/x/s.webp",
        ] {
            assert!(is_picture(path), "{path}");
        }
        for path in ["/a/b.txt", "/a/png", "/a/b.png.zip", "https://x.org/a"] {
            assert!(!is_picture(path), "{path}");
        }
    }

    #[test]
    fn only_ctrl_v_alone_is_the_key() {
        assert!(is_ctrl_v(&[0x16]));
        assert!(!is_ctrl_v(b"a\x16"));
        assert!(!is_ctrl_v(&[0x16, 0x16]));
    }

    fn kept(id: &str, path: &str) -> ControlResponse {
        ControlResponse::Uploaded {
            id: id.into(),
            path: path.into(),
        }
    }

    /// The pasted text is the drop with each local picture swapped for where the remote
    /// keeps it, whichever answer comes first; keys typed meanwhile go after it.
    #[test]
    fn a_drop_pastes_the_remote_paths_in_place_once_all_are_kept() {
        let mut p = Pictures::default();
        let go = p.drop(
            9,
            vec![
                ("/mac/a.png ".into(), Some("/mac/a.png".into())),
                ("/mac/notes.txt ".into(), None),
                ("/mac/b.jpg ".into(), Some("/mac/b.jpg".into())),
            ],
        );
        assert_eq!(go.len(), 2);
        assert!(p.hold(9, b"x"));
        assert!(!p.hold(8, b"y"), "another tab's keys were held");
        for (id, _) in &go {
            let (request, done) = p.prepared(id, Ok(Some(("jpg", vec![1, 2, 3]))));
            assert!(done.is_none());
            assert!(matches!(
                request,
                Some(ControlRequest::SaveUpload { terminal: Some(9), ref ext, .. }) if ext == "jpg"
            ));
        }
        assert!(p.answered(&kept(&go[1].0, "/r/2.jpg")).is_none());
        let done = p.answered(&kept(&go[0].0, "/r/1.jpg"));
        assert_eq!(
            done,
            Some(Done {
                terminal: 9,
                paste: Some("/r/1.jpg /mac/notes.txt /r/2.jpg ".into()),
                input: b"x".to_vec(),
                message: None,
            })
        );
        assert!(!p.busy(9));
    }

    /// A picture the remote refused is left out and said: a path on this machine is the
    /// very thing the agent there cannot open.
    #[test]
    fn a_refused_picture_is_left_out_and_said() {
        let mut p = Pictures::default();
        let go = p.drop(9, vec![("/mac/a.png ".into(), Some("/mac/a.png".into()))]);
        let failed = ControlResponse::Failed {
            id: go[0].0.clone(),
            reason: "devbox is offline".into(),
        };
        let done = p.answered(&failed).expect("finished");
        assert_eq!(done.paste, None);
        assert!(done.message.as_deref().unwrap().contains("offline"));
    }

    /// The oldest daemons refuse without naming the request: the one picture on its way is
    /// the one refused, so the held keys go at once rather than after the wait.
    #[test]
    fn a_refusal_naming_nothing_is_the_one_picture_on_its_way() {
        let mut p = Pictures::default();
        p.drop(9, vec![("/mac/a.png ".into(), Some("/mac/a.png".into()))]);
        p.hold(9, b"k");
        let failed = ControlResponse::Failed {
            id: String::new(),
            reason: "unknown variant `SaveUpload`".into(),
        };
        let done = p.answered(&failed).expect("finished");
        assert_eq!(done.paste, None);
        assert_eq!(done.input, b"k".to_vec());
    }

    /// ctrl+v with no picture on the clipboard is the key itself, still before what was
    /// typed after it.
    #[test]
    fn ctrl_v_without_a_picture_is_the_key_in_its_place() {
        let mut p = Pictures::default();
        let id = p.ctrl_v(9);
        assert!(p.hold(9, b"\r"));
        let (request, done) = p.prepared(&id, Ok(None));
        assert!(request.is_none());
        let done = done.expect("finished");
        assert_eq!(done.paste, None);
        assert_eq!(done.input, b"\x16\r".to_vec());
    }

    #[test]
    fn ctrl_v_with_a_picture_pastes_its_remote_path_then_the_keys() {
        let mut p = Pictures::default();
        let id = p.ctrl_v(9);
        p.hold(9, b"\r");
        let (request, _) = p.prepared(&id, Ok(Some(("png", vec![7]))));
        assert!(request.is_some());
        let done = p.answered(&kept(&id, "/r/c.jpg")).expect("finished");
        assert_eq!(done.paste.as_deref(), Some("/r/c.jpg "));
        assert_eq!(done.input, b"\r".to_vec());
    }

    /// A second drop while the first is on its way joins it: both paste, in order, with the
    /// keys typed between them after.
    #[test]
    fn a_second_drop_joins_the_first_rather_than_losing_it() {
        let mut p = Pictures::default();
        let a = p.drop(9, vec![("/mac/a.png ".into(), Some("/mac/a.png".into()))]);
        p.hold(9, b"k");
        let b = p.drop(9, vec![("/mac/b.png ".into(), Some("/mac/b.png".into()))]);
        assert!(p.answered(&kept(&b[0].0, "/r/b.png")).is_none());
        let done = p.answered(&kept(&a[0].0, "/r/a.png")).expect("finished");
        assert_eq!(done.paste.as_deref(), Some("/r/a.png /r/b.png "));
        assert_eq!(done.input, b"k".to_vec());
    }

    /// A clipboard picture that could not be sent is said, not followed by the bare key and
    /// the agent's own "no image" on top.
    #[test]
    fn a_clipboard_picture_that_failed_does_not_type_the_key() {
        let mut p = Pictures::default();
        let id = p.ctrl_v(9);
        let (_, done) = p.prepared(&id, Err("the picture is too big to send".into()));
        let done = done.expect("finished");
        assert!(done.input.is_empty());
        assert!(done.message.is_some());
    }

    /// A picture made by `sips` from a system icon, `size` on each side.
    #[cfg(target_os = "macos")]
    fn made(dir: &Path, size: u32, ext: &str) -> PathBuf {
        let out = dir.join(format!("p{size}.{ext}"));
        let icon = "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/Actions.icns";
        let format = if ext == "tiff" { "tiff" } else { "png" };
        let ok = std::process::Command::new("sips")
            .args([
                "-s",
                "format",
                format,
                "-z",
                &size.to_string(),
                &size.to_string(),
                icon,
            ])
            .arg("--out")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("sips")
            .success();
        assert!(ok, "sips made no picture");
        out
    }

    /// A small picture of a kind the agent reads goes unchanged: no JPEG, no enlarging.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_small_png_goes_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let png = made(dir.path(), 120, "PNG");
        let (kind, bytes) = prepare(&png).expect("prepared");
        assert_eq!(kind, "png");
        assert_eq!(bytes, std::fs::read(&png).unwrap());
    }

    /// A big picture of another kind leaves as a JPEG no wider than the agent's own limit.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_big_picture_leaves_small_as_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let tiff = made(dir.path(), 3000, "tiff");
        let (kind, bytes) = prepare(&tiff).expect("prepared");
        assert_eq!(kind, "jpg");
        assert!(bytes.len() <= MOST, "{} bytes", bytes.len());
        let out = dir.path().join("out.jpg");
        std::fs::write(&out, &bytes).unwrap();
        let said = std::process::Command::new("sips")
            .args(["-g", "pixelWidth"])
            .arg(&out)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&said.stdout).into_owned();
        assert!(said.contains(&format!("pixelWidth: {EDGE}")), "{said}");
    }

    /// No answer at all still lets the held keys go.
    #[test]
    fn a_picture_with_no_answer_lets_the_keys_go() {
        let mut p = Pictures::default();
        p.drop(9, vec![("/mac/a.png ".into(), Some("/mac/a.png".into()))]);
        p.hold(9, b"hi");
        assert!(p.expired(Instant::now()).is_empty());
        let done = p.expired(Instant::now() + PATIENCE);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].input, b"hi".to_vec());
        assert!(!p.busy(9));
    }
}
