//! Shared with the desktop's qass page: how it says qass is not running, and the reader of the
//! rules qass has written.

use crate::answers::Lessons;

/// "last seen running at 17:02", or "not seen running since this window opened" with no time:
/// how the desktop says qass is not running.
pub fn seen_running(at: Option<&str>) -> String {
    match at {
        Some(at) => format!("last seen running at {at}"),
        None => "not seen running since this window opened".to_string(),
    }
}

/// The rules in the lessons file at `path`: a file that is not there is an empty count (qass
/// has taught nobody anything yet), one that cannot be read is `None`. Only the first 1 MiB is
/// read. The desktop's qass page reads it here.
pub fn rules_read_at(path: &std::path::Path) -> Option<Lessons> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Lessons::default()),
        Err(_) => return None,
    };
    let mut text = String::new();
    file.take(1 << 20).read_to_string(&mut text).ok()?;
    Some(crate::answers::lessons_of(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lessons file: missing is none written yet, a folder where it should be (unreadable)
    /// is none, and one that is there is counted.
    #[test]
    fn the_rules_file_missing_unreadable_and_present() {
        let dir = std::env::temp_dir().join(format!("qoop-rules-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        assert_eq!(
            rules_read_at(&dir.join("nothing.md")),
            Some(Lessons::default()),
            "no file: none written yet"
        );
        assert_eq!(rules_read_at(&dir), None, "a folder cannot be read");
        let file = dir.join("lessons.md");
        std::fs::write(
            &file,
            "- a release -> escalate (owner)\n- a test run -> do it (E7)\n- a bump -> wait (E9)\n",
        )
        .expect("file");
        assert_eq!(
            rules_read_at(&file),
            Some(Lessons {
                yours: 1,
                alone: 1,
                all: 3
            })
        );
        std::fs::remove_file(&file).expect("clean");
        std::fs::remove_dir(&dir).expect("clean");
    }
}
