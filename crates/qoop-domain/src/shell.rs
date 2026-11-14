//! Words typed into somebody's shell, written so every shell a person runs reads them back
//! exactly: sh, dash, bash, zsh and fish.
//!
//! Single quotes alone are not that. POSIX shells read nothing inside them, but fish still
//! reads `\'` and `\\` there, so a backslash in a word typed into a fish prompt could end
//! the quote early and run the rest. So a `'` or a `\` is never put inside quotes: each is
//! written outside them, after a backslash, which all of these shells read as that one
//! character. Every other character sits inside single quotes, and a quoted run is never
//! followed straight by another (zsh's `RC_QUOTES` reads `''` inside quotes as a quote).
//!
//! The words are typed, so the shell's line editor reads them before the parser does: a
//! control character other than a newline is a key there (`ctrl-u` wipes the line typed so
//! far, and what follows runs as a line of its own), so each becomes a space.

/// The characters a word may be made of and still go bare: nothing any of the shells reads
/// as more than itself in the middle of a word. `~` is left out (every shell expands a
/// leading one), and a bare word never starts with `=` (zsh's path expansion) or `%`
/// (fish's `%self`).
fn bare(word: &str) -> bool {
    !word.starts_with(['=', '%'])
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c))
}

/// One word, as every shell reads it back.
pub fn quote(word: &str) -> String {
    let word = word.replace(|c: char| c.is_control() && c != '\n', " ");
    if word.is_empty() {
        return "''".to_string();
    }
    if bare(&word) {
        return word;
    }
    let mut out = String::with_capacity(word.len() + 2);
    let mut inside = false;
    for c in word.chars() {
        let escaped = c == '\'' || c == '\\';
        if escaped == inside {
            // Leaving a quoted run before an escaped character, or opening one after it.
            out.push('\'');
            inside = !inside;
        }
        if escaped {
            out.push('\\');
        }
        out.push(c);
    }
    if inside {
        out.push('\'');
    }
    out
}

/// A whole command line: each word quoted, one space between.
pub fn line<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|word| quote(word.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[test]
    fn plain_words_go_bare_and_the_rest_is_quoted() {
        assert_eq!(quote("feat/x-1"), "feat/x-1");
        assert_eq!(quote("--resume"), "--resume");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("'"), r"\'");
        assert_eq!(quote(r"a\b"), r"'a'\\'b'");
        assert_eq!(quote(r"ends with \"), r"'ends with '\\");
        assert_eq!(quote(r"a\'; touch x; #"), r"'a'\\\''; touch x; #'");
        assert_eq!(quote("--x=1"), "--x=1");
        assert_eq!(quote("50%"), "50%");
        assert_eq!(quote("=ls"), "'=ls'");
        assert_eq!(quote("%self"), "'%self'");
        assert_eq!(quote("~"), "'~'");
        assert_eq!(line(&["claude", "fix it"]), "claude 'fix it'");
    }

    /// A typed word holds no key but a newline: `ctrl-u` would wipe the line typed so far and
    /// run what follows it.
    #[test]
    fn a_typed_word_holds_no_key_but_a_newline() {
        assert_eq!(
            quote("a\u{15}touch pwn\r\t\u{3}\u{1b}[A\n"),
            "'a touch pwn    [A\n'"
        );
        assert_eq!(quote("a\u{15}touch pwn\r"), "'a touch pwn '");
    }

    /// The shells on this machine to check against: `sh`, `bash`, `zsh` (also with
    /// `RC_QUOTES`, which a person's `.zshrc` may turn on) and `fish`, each when installed.
    fn shells() -> Vec<Vec<String>> {
        let found = |name: &str| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(name))
                    .find(|p| p.is_file())
            })
        };
        let mut out = vec![vec!["/bin/sh".to_string()]];
        for name in ["bash", "zsh", "fish"] {
            if let Some(path) = found(name) {
                out.push(vec![path.display().to_string()]);
            }
        }
        if let Some(zsh) = found("zsh") {
            out.push(vec![
                zsh.display().to_string(),
                "-o".into(),
                "rcquotes".into(),
            ]);
        }
        out
    }

    /// What `shell` prints for `printf %s <word>`: the word as that shell read it.
    fn read_back(shell: &[String], quoted: &str, cwd: &Path) -> Vec<u8> {
        let out = Command::new(&shell[0])
            .args(&shell[1..])
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .current_dir(cwd)
            .output()
            .expect("the shell runs");
        assert!(
            out.status.success(),
            "{shell:?} refused {quoted:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "qoop-shell-quote-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch folder");
        dir
    }

    /// Every hostile word comes back byte for byte from every shell installed here, and none
    /// of the commands hidden in them runs: each would make the file `pwned` in the folder
    /// the shell runs in.
    #[test]
    fn every_shell_reads_every_word_back_and_runs_nothing() {
        let dir = scratch();
        let pwned = dir.join("pwned");
        let words = [
            "it's",
            r"a\'; touch pwned; #",
            r"ends with \",
            r"\",
            r"\\",
            "'",
            "''",
            r"\'",
            r"'\",
            r"a'b\c'd",
            r#"x\"; touch pwned; ""#,
            "say \"hi\"",
            "$(touch pwned)",
            "`touch pwned`",
            "$HOME ${HOME} $'\\x41'",
            "a\nb; touch pwned",
            "line\\\n; touch pwned",
            "; touch pwned",
            "&& touch pwned",
            "| touch pwned",
            "%self =ls ~ *",
            "{a,b} (x) [y] #z",
            "!x !! ^a^b",
            "(touch pwned)",
            "x\n'; touch pwned; #",
            "=touch",
            "--x=1",
            "d\u{fc}zelt",
            "",
            " ",
        ];
        let shells = shells();
        for shell in &shells {
            for word in words {
                let quoted = quote(word);
                assert_eq!(
                    read_back(shell, &quoted, &dir),
                    word.as_bytes(),
                    "{shell:?} read {quoted:?} as something else"
                );
                assert!(
                    !pwned.exists(),
                    "{shell:?} ran a command hidden in {word:?}"
                );
            }
        }
        // A whole line, as New work and a resume type them.
        let argv = ["claude", r"fix it's \ path", "$(touch pwned)", "--x=1"];
        for shell in &shells {
            let out = Command::new(&shell[0])
                .args(&shell[1..])
                .arg("-c")
                .arg(format!("printf '%s|' {}", line(&argv)))
                .current_dir(&dir)
                .output()
                .expect("the shell runs");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("{}|", argv.join("|")),
                "{shell:?}"
            );
            assert!(!pwned.exists(), "{shell:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
