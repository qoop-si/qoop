//! d-review: what a tab's checkout changed, read in this process on open only
//! (`qoop_runtime::changes::read`), each file with its counts, one open and the rest folded;
//! and opening a file in the person's editor without ever handing a diff's path to a program
//! that would run it.

use std::path::{Path, PathBuf};

use qoop_runtime::control::ChangedFile;

/// A `Changes` answer, kept while the review is open.
#[derive(Clone, Debug, PartialEq)]
pub struct Changes {
    pub root: String,
    pub base: String,
    pub files: Vec<ChangedFile>,
    pub diffs: Vec<FileDiff>,
    pub cut: bool,
}

/// The review of one tab: asked under `id`, then the answer or why there is none.
#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    pub terminal: u32,
    pub id: String,
    pub read: Option<Result<Changes, String>>,
    /// The file shown open; the rest are folded.
    pub open: usize,
    /// Why a file did not open in the editor, said under the list.
    pub said: Option<String>,
}

impl Review {
    /// The file after the open one, round to the first.
    pub fn next_file(&mut self) {
        if let Some(Ok(changes)) = &self.read
            && !changes.files.is_empty()
        {
            self.open = (self.open + 1) % changes.files.len();
        }
    }
}

impl Changes {
    pub fn diff_of(&self, path: &str) -> Option<&FileDiff> {
        self.diffs.iter().find(|d| d.path == path)
    }

    /// Lines added and removed over every file that has counts.
    pub fn totals(&self) -> (u32, u32) {
        self.files.iter().fold((0, 0), |(a, r), f| {
            (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0))
        })
    }
}

/// "+12 -1", "+12", "-3", "new" for an untracked file, nothing for a binary one.
pub fn counts(file: &ChangedFile) -> Option<String> {
    if file.status == "?" {
        return Some("new".into());
    }
    match (file.added?, file.removed?) {
        // A mode change only: no lines to count.
        (0, 0) => None,
        (a, 0) => Some(format!("+{a}")),
        (0, r) => Some(format!("-{r}")),
        (a, r) => Some(format!("+{a} -{r}")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Hunk,
    Same,
    Added,
    Removed,
}

/// One row of a file's diff, with the line it is at in the new file (a removed row: where the
/// new file goes on), for a click that opens the editor there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub mark: Mark,
    pub text: String,
    pub line: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub binary: bool,
    pub lines: Vec<DiffLine>,
}

/// A `git diff` cut into its files: each `diff --git` header starts one, named by its new path
/// (`+++ b/...`, else the header's `b/` side); hunks number their rows by the new file.
pub fn split_diff(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut new, mut in_hunk) = (0u32, false);
    for row in diff.lines() {
        if let Some(header) = row.strip_prefix("diff --git ") {
            let path = header
                .rfind(" b/")
                .map_or(header, |at| &header[at + 3..])
                .to_string();
            files.push(FileDiff {
                path,
                ..FileDiff::default()
            });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if !in_hunk {
            if let Some(path) = row.strip_prefix("+++ b/") {
                // git ends the line with a tab when the path holds a space.
                file.path = path.strip_suffix('\t').unwrap_or(path).to_string();
            } else if row.starts_with("Binary files ") {
                file.binary = true;
            }
        }
        if row.starts_with("@@") {
            // `@@ -a,b +c,d @@`: the new file's rows start at c.
            new = row
                .split(' ')
                .find_map(|w| w.strip_prefix('+'))
                .and_then(|w| w.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            in_hunk = true;
            file.lines.push(DiffLine {
                mark: Mark::Hunk,
                text: row.to_string(),
                line: new,
            });
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (mark, text) = match row.as_bytes().first() {
            Some(b'+') => (Mark::Added, &row[1..]),
            Some(b'-') => (Mark::Removed, &row[1..]),
            Some(b' ') => (Mark::Same, &row[1..]),
            // `\ No newline at end of file`.
            _ => continue,
        };
        file.lines.push(DiffLine {
            mark,
            text: text.to_string(),
            line: new,
        });
        if mark != Mark::Removed {
            new += 1;
        }
    }
    files
}

/// How a file of the checkout at `root` opens at `line`: `zed <path>:<line>` or `code -g
/// <path>:<line>` when either is found (`find` gives its full path), else `open -t <path>`, which
/// opens text in the default editor and never runs it. The path is joined under the root and
/// refused when it resolves outside it (`..`, a link out); it is passed absolute, so it never
/// starts with `-`. A bare `open` is never used for a file from a diff: a `.command`, `.app`,
/// `.terminal` or `.webloc` would start something.
pub fn editor_for(
    root: &Path,
    file: &str,
    line: Option<u32>,
    find: impl Fn(&str) -> Option<PathBuf>,
) -> Result<(PathBuf, Vec<String>), String> {
    let root = root
        .canonicalize()
        .map_err(|_| "the checkout is not there any more".to_string())?;
    let path = root
        .join(file)
        .canonicalize()
        .map_err(|_| format!("{file} is not there any more"))?;
    if !path.starts_with(&root) {
        return Err(format!("{file} is outside the checkout"));
    }
    if !path.is_file() {
        return Err(format!("{file} is not a file to open"));
    }
    let path = path.display().to_string();
    let at = match line {
        Some(n) => format!("{path}:{n}"),
        None => path.clone(),
    };
    if let Some(zed) = find("zed") {
        return Ok((zed, vec![at]));
    }
    if let Some(code) = find("code") {
        return Ok((code, vec!["-g".into(), at]));
    }
    Ok((PathBuf::from("/usr/bin/open"), vec!["-t".into(), path]))
}

/// Start `program` with `args`, not waiting for it; a failure is said on stderr.
pub fn launch(program: &Path, args: &[String]) {
    let started = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match started {
        // Reaped on a thread of its own, so it never stays a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("qoop-desktop: {} did not start: {e}", program.display()),
    }
}

/// The checkout `cwd` is in, opened in the Finder's way for a folder (`open <root>`): a folder
/// opens, it never runs. git is asked for the root on a thread, so the window never waits.
pub fn open_folder(cwd: String) {
    std::thread::spawn(move || {
        let root = std::process::Command::new("git")
            .args(["-C", &cwd, "rev-parse", "--show-toplevel"])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or(cwd);
        if Path::new(&root).is_dir() {
            launch(Path::new("/usr/bin/open"), &[root]);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.ts b/src/a.ts
index 1..2 100644
--- a/src/a.ts
+++ b/src/a.ts
@@ -18,3 +18,3 @@ export function isValid(
   export function isValid(c: Coupon) {
-    return c.expiresAt > now;
+    return c.at > now;
   }
\\ No newline at end of file
diff --git a/pic.png b/pic.png
new file mode 100644
Binary files /dev/null and b/pic.png differ
";

    #[test]
    fn a_diff_is_cut_into_files_numbered_by_the_new_file() {
        let files = split_diff(DIFF);
        assert_eq!(files.len(), 2);
        let a = &files[0];
        assert_eq!(a.path, "src/a.ts");
        let rows: Vec<(Mark, u32)> = a.lines.iter().map(|l| (l.mark, l.line)).collect();
        assert_eq!(
            rows,
            [
                (Mark::Hunk, 18),
                (Mark::Same, 18),
                (Mark::Removed, 19),
                (Mark::Added, 19),
                (Mark::Same, 20),
            ]
        );
        assert_eq!(a.lines[3].text, "    return c.at > now;");
        assert!(files[1].binary && files[1].lines.is_empty());
        assert_eq!(files[1].path, "pic.png");
    }

    /// git ends a `+++` line with a tab when the path holds a space: the file is still named
    /// as the list names it.
    #[test]
    fn a_path_with_a_space_keeps_its_name() {
        let diff = "diff --git a/docs/release notes.md b/docs/release notes.md\n\
                    --- a/docs/release notes.md\t\n\
                    +++ b/docs/release notes.md\t\n\
                    @@ -1 +1 @@\n\
                    -old\n\
                    +new\n";
        let files = split_diff(diff);
        assert_eq!(files[0].path, "docs/release notes.md");
        assert_eq!(files[0].lines.len(), 3);
    }

    fn checkout() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path().join("work");
        std::fs::create_dir(&root).expect("root");
        (dir, root)
    }

    /// A `.command` file from a diff opens as text, never as a program to run.
    #[test]
    fn a_command_file_opens_as_text() {
        let (_dir, root) = checkout();
        std::fs::write(root.join("run.command"), "echo hi\n").expect("file");
        let (program, args) = editor_for(&root, "run.command", Some(3), |_| None).expect("opens");
        assert_eq!(program, PathBuf::from("/usr/bin/open"));
        assert_eq!(args[0], "-t");
        assert!(args[1].ends_with("/work/run.command") && args[1].starts_with('/'));
    }

    /// A file named `-x` reaches the editor as an absolute path, never as a flag.
    #[test]
    fn a_file_named_like_a_flag_is_passed_as_a_path() {
        let (_dir, root) = checkout();
        std::fs::write(root.join("-x"), "x\n").expect("file");
        let zed = |name: &str| (name == "zed").then(|| PathBuf::from("/opt/zed"));
        let (program, args) = editor_for(&root, "-x", Some(7), zed).expect("opens");
        assert_eq!(program, PathBuf::from("/opt/zed"));
        assert_eq!(args.len(), 1);
        assert!(args[0].starts_with('/') && args[0].ends_with("/work/-x:7"));
        let code = |name: &str| (name == "code").then(|| PathBuf::from("/opt/code"));
        let (_, args) = editor_for(&root, "-x", None, code).expect("opens");
        assert_eq!(args[0], "-g");
        assert!(args[1].ends_with("/work/-x"));
    }

    /// A path out of the checkout, by `..` or by a link, is refused.
    #[test]
    fn a_path_out_of_the_checkout_is_refused() {
        let (dir, root) = checkout();
        std::fs::write(dir.path().join("secret"), "s\n").expect("file");
        assert!(editor_for(&root, "../secret", None, |_| None).is_err());
        std::os::unix::fs::symlink(dir.path().join("secret"), root.join("link")).expect("link");
        assert!(editor_for(&root, "link", None, |_| None).is_err());
        assert!(editor_for(&root, "gone.txt", None, |_| None).is_err());
    }

    #[test]
    fn counts_read_as_the_design_writes_them() {
        let file = |status: &str, added, removed| ChangedFile {
            path: "a".into(),
            status: status.into(),
            added,
            removed,
        };
        assert_eq!(
            counts(&file("M", Some(12), Some(1))).as_deref(),
            Some("+12 -1")
        );
        assert_eq!(
            counts(&file("A", Some(12), Some(0))).as_deref(),
            Some("+12")
        );
        assert_eq!(counts(&file("?", None, None)).as_deref(), Some("new"));
        assert_eq!(counts(&file("M", None, None)), None, "binary");
        assert_eq!(counts(&file("M", Some(0), Some(0))), None, "mode only");
    }
}
