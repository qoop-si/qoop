//! Every character beyond ASCII in qoop's source must be one a fresh machine draws: a glyph in
//! the default fonts of a new Mac and a new Linux desktop, or one of the icons in qoop's own
//! font, which the interface draws only where that font is in place. A glyph outside both is
//! a box or a blank on somebody's screen, which is how the icons went missing on a second Mac.
//!
//! Measured 2026-10-13: CoreText's fallback from Monaco 12 (iTerm2's default
//! profile), SF Mono 11 (Terminal's) and Menlo, counting only fonts under `/System` and
//! `/Library`, and `fc-list ':charset=<hex>'` on Ubuntu 24.04 with its default fonts
//! (fonts-dejavu-core, fonts-ubuntu), found every glyph in [`SAFE`] and every codepoint of
//! the three whole blocks in [`SAFE_BLOCKS`]. A new glyph joins only after the same check on
//! both systems.

use std::path::{Path, PathBuf};

/// Single glyphs qoop draws, measured on both systems.
const SAFE: &str = "«·»×’•…‹›←↑→↓↵⋯▲▸▼▾◆○●✓";

/// Box Drawing, Block Elements and Braille Patterns, every codepoint measured on both.
const SAFE_BLOCKS: [(char, char); 2] = [('\u{2500}', '\u{259f}'), ('\u{2800}', '\u{28ff}')];

/// Characters a file only compares against what another program wrote, strips or measures,
/// and never draws.
const MATCHED: &[(&str, &str)] = &[
    ("qoop-tui/src/notify.rs", "\u{9f}"),
    ("qoop-runtime/src/idle.rs", "⏺◯"),
    // The trust menu's pointers, looked for on an agent's screen, never drawn.
    ("qoop-detect/src/lib.rs", "\u{276f}\u{203a}"),
    ("qoop-runtime/src/asks.rs", "üı\u{307}ö\u{201d}"),
    (
        "qoop-store/src/task.rs",
        "\u{200b}\u{200f}\u{202a}\u{202e}\u{2060}\u{206f}\u{feff}\u{e0000}\u{e007f}",
    ),
    (
        "qoop-tui/src/link.rs",
        "\u{200b}\u{200f}\u{202a}\u{202e}\u{2066}\u{2069}",
    ),
    (
        "qoop-tui/src/width.rs",
        "\u{200d}\u{202a}\u{202e}\u{2066}\u{2069}\u{20e3}\u{fe0e}\u{fe0f}\u{1f1e6}\u{1f1ff}\u{e0020}\u{e007f}",
    ),
    ("qoop-tui/src/config.rs", "\u{feff}"),
];

/// Files that make a character from a number, and why what they make is safe.
const COMPUTED: &[(&str, &str)] = &[
    (
        "qoop-tui/src/sidebar.rs",
        "0x2800 plus at most eight dot bits: Braille",
    ),
    (
        "qoop-terminal/src/keyboard.rs",
        "decodes a key typed, never drawn by qoop",
    ),
];

/// Where the icons live, the one file whose private use characters are drawn.
const ICONS: &str = "qoop-tui/src/icons.rs";

/// Crates nobody sees the output of.
const UNSEEN: [&str; 2] = ["qoop-testkit", "qoop-bench"];

fn safe(ch: char) -> bool {
    SAFE.contains(ch) || SAFE_BLOCKS.iter().any(|(lo, hi)| (*lo..=*hi).contains(&ch))
}

fn icon(ch: char) -> bool {
    qoop_tui::icons::GLYPHS.iter().any(|(_, g)| *g == ch)
}

/// A non-ASCII character in a literal, with its line.
#[derive(Debug, PartialEq)]
struct Lit {
    line: usize,
    ch: char,
}

/// The characters beyond ASCII in the string and char literals of Rust source, `\u{..}`
/// escapes decoded, comments and `#[cfg(test)]` items left out.
fn rust_literals(src: &str) -> Vec<Lit> {
    rust_lex(src).0
}

/// The literals, and the code outside literals, comments and test items.
fn rust_lex(src: &str) -> (Vec<Lit>, String) {
    let c: Vec<char> = src.chars().collect();
    let n = c.len();
    let at = |i: usize| c.get(i).copied().unwrap_or('\0');
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut lits: Vec<(usize, Lit)> = Vec::new();
    let mut code = vec![false; n];
    let mut line = 1;
    let mut i = 0;
    // `\u{..}` at `j` (pointing at `u`): the character and the index after `}`.
    let unicode = |j: usize| -> (Option<char>, usize) {
        let end = (j..n).find(|&k| c[k] == '}').unwrap_or(n);
        let hex: String = c[j + 2..end].iter().filter(|h| **h != '_').collect();
        (
            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32),
            end + 1,
        )
    };
    while i < n {
        let ch = c[i];
        if ch == '/' && at(i + 1) == '/' {
            while i < n && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && at(i + 1) == '*' {
            let mut depth = 0;
            loop {
                if i >= n {
                    break;
                }
                if c[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    line += usize::from(c[i] == '\n');
                    i += 1;
                }
            }
            continue;
        }
        let word_start = i == 0 || !ident(c[i - 1]);
        // Raw strings: r"..", r#".."#, br"..".
        if word_start && (ch == 'r' || (ch == 'b' && at(i + 1) == 'r')) {
            let mut j = i + if ch == 'b' { 2 } else { 1 };
            let hashes = (j..n).take_while(|&k| c[k] == '#').count();
            j += hashes;
            if at(j) == '"' {
                j += 1;
                while j < n {
                    if c[j] == '"' && (1..=hashes).all(|k| at(j + k) == '#') {
                        j += 1 + hashes;
                        break;
                    }
                    line += usize::from(c[j] == '\n');
                    if !c[j].is_ascii() {
                        lits.push((j, Lit { line, ch: c[j] }));
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        if ch == '"' || (word_start && ch == 'b' && at(i + 1) == '"') {
            let mut j = i + if ch == 'b' { 2 } else { 1 };
            while j < n && c[j] != '"' {
                if c[j] == '\\' {
                    if at(j + 1) == 'u' && at(j + 2) == '{' {
                        let (got, next) = unicode(j + 1);
                        if let Some(g) = got.filter(|g| !g.is_ascii()) {
                            lits.push((j, Lit { line, ch: g }));
                        }
                        j = next;
                        continue;
                    }
                    line += usize::from(at(j + 1) == '\n');
                    j += 2;
                    continue;
                }
                line += usize::from(c[j] == '\n');
                if !c[j].is_ascii() {
                    lits.push((j, Lit { line, ch: c[j] }));
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if ch == '\'' {
            // A char literal, or a lifetime or label (`'a`, `'outer:`), which has no closing quote.
            if at(i + 1) == '\\' {
                let mut j = i + 2;
                if at(j) == 'u' && at(j + 1) == '{' {
                    let (got, next) = unicode(j);
                    if let Some(g) = got.filter(|g| !g.is_ascii()) {
                        lits.push((i, Lit { line, ch: g }));
                    }
                    j = next;
                } else {
                    j += 1;
                }
                i = (j..n).find(|&k| c[k] == '\'').map_or(n, |k| k + 1);
                continue;
            }
            if at(i + 2) == '\'' {
                if !at(i + 1).is_ascii() {
                    lits.push((
                        i,
                        Lit {
                            line,
                            ch: at(i + 1),
                        },
                    ));
                }
                i += 3;
                continue;
            }
        }
        line += usize::from(ch == '\n');
        code[i] = true;
        i += 1;
    }
    // `#[cfg(test)]` and `#[cfg(all(test, ..))]` items: up to the `;` or the `}` that closes
    // the first `{`, counting only code.
    let mut skipped: Vec<(usize, usize)> = Vec::new();
    let text: String = c.iter().collect();
    let offsets: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    for pattern in ["#[cfg(test)]", "#[cfg(all(test"] {
        for (byte, _) in text.match_indices(pattern) {
            let start = offsets.partition_point(|&b| b < byte);
            if !code[start] {
                continue;
            }
            let mut depth = 0;
            let mut k = start + pattern.chars().count();
            while k < n {
                if code[k] {
                    match c[k] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        ';' if depth == 0 => break,
                        _ => {}
                    }
                }
                k += 1;
            }
            skipped.push((start, k));
        }
    }
    let live = |at: usize| !skipped.iter().any(|(s, e)| (*s..=*e).contains(&at));
    let code_text = (0..n)
        .filter(|&k| code[k] && live(k))
        .map(|k| c[k])
        .collect();
    let lits = lits
        .into_iter()
        .filter(|(at, _)| live(*at))
        .map(|(_, lit)| lit)
        .collect();
    (lits, code_text)
}

/// The same for Swift: string literals with `\u{..}` escapes, comments left out. Swift has
/// no char literal, and the notch nests no string inside an interpolation.
fn swift_literals(src: &str) -> Vec<Lit> {
    let mut out = Vec::new();
    for (n, text) in src.lines().enumerate() {
        let c: Vec<char> = text.chars().collect();
        let mut inside = false;
        let mut i = 0;
        while i < c.len() {
            match (inside, c[i], c.get(i + 1).copied()) {
                (false, '/', Some('/')) => break,
                (_, '"', _) => inside = !inside,
                (true, '\\', Some('u')) if c.get(i + 2) == Some(&'{') => {
                    let end = (i..c.len()).find(|&k| c[k] == '}').unwrap_or(c.len());
                    let hex: String = c[i + 3..end].iter().collect();
                    if let Some(g) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        && !g.is_ascii()
                    {
                        out.push(Lit { line: n + 1, ch: g });
                    }
                    i = end;
                }
                (true, '\\', _) => i += 1,
                (true, ch, _) if !ch.is_ascii() => out.push(Lit { line: n + 1, ch }),
                _ => {}
            }
            i += 1;
        }
    }
    out
}

/// Each finding as one line: where, the codepoint, and why it is refused.
fn violations(rel: &str, src: &str) -> Vec<String> {
    let lits = if rel.ends_with(".swift") {
        swift_literals(src)
    } else {
        rust_literals(src)
    };
    let matched = MATCHED
        .iter()
        .find(|(file, _)| *file == rel)
        .map_or("", |(_, chars)| *chars);
    let mut out = Vec::new();
    for Lit { line, ch } in lits {
        let ok = if icon(ch) {
            rel == ICONS
        } else {
            safe(ch) || matched.contains(ch)
        };
        if !ok {
            let why = if icon(ch) {
                "an icon outside icons.rs: draw it through `icons::on()` and its fallback"
            } else {
                "not measured in the default fonts of a fresh Mac and Ubuntu: measure it (see \
                 this file's header) and add it to SAFE, or add it to MATCHED if it is never drawn"
            };
            out.push(format!("{rel}:{line} {ch:?} U+{:04X}: {why}", ch as u32));
        }
    }
    if rust_code_makes_chars(rel, src) && !COMPUTED.iter().any(|(file, _)| *file == rel) {
        out.push(format!(
            "{rel}: makes a character from a number, which this test cannot read: keep it in a \
             safe block and add the file to COMPUTED with why"
        ));
    }
    out
}

fn rust_code_makes_chars(rel: &str, src: &str) -> bool {
    rel.ends_with(".rs") && {
        let code = rust_lex(src).1;
        code.contains("char::from_u32") || code.contains("char::from_digit")
    }
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            sources(&path, out);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_glyph_qoop_draws_is_one_a_fresh_machine_has() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).expect("crates").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !UNSEEN.contains(&name.as_str()) {
            sources(&entry.path().join("src"), &mut files);
        }
    }
    files.push(crates.join("qoop-cli/notch/main.swift"));
    assert!(files.len() > 100, "only {} files found", files.len());
    let mut found = Vec::new();
    let mut seen_icons = 0;
    for path in &files {
        let rel = path
            .strip_prefix(&crates)
            .expect("under crates")
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(path).expect("source");
        if rel == ICONS {
            seen_icons = rust_literals(&src).iter().filter(|l| icon(l.ch)).count();
        }
        found.extend(violations(&rel, &src));
    }
    // A lexer that skipped everything would pass, so it must see the icons where they are.
    assert_eq!(
        seen_icons,
        qoop_tui::icons::GLYPHS.len(),
        "the icons in {ICONS}"
    );
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}

/// The lexer on the shapes that would fool it.
#[test]
fn the_lexer_reads_literals_and_only_literals() {
    let chars = |src: &str| -> String { rust_literals(src).iter().map(|l| l.ch).collect() };
    assert_eq!(chars(r#"let a = "☕";"#), "☕");
    assert_eq!(chars(r"let a = '\u{2615}';"), "☕");
    assert_eq!(chars(r#"let a = "x\u{2615}";"#), "☕");
    assert_eq!(chars(r###"let a = r#"☕ "quoted""#;"###), "☕");
    assert_eq!(chars("let a = \"line \\\n   ☕\";"), "☕");
    assert_eq!(chars(r#"let a = "// ☕";"#), "☕");
    assert_eq!(chars("// ☕\n/* /* ☕ */ ☕ */ let a = 1;"), "");
    assert_eq!(chars(r#"if c == '"' { return "☕"; }"#), "☕");
    assert_eq!(chars(r#"let q = '\''; let s = "☕";"#), "☕");
    assert_eq!(
        chars("fn f<'a>(x: &'a str) -> &'a str { 'outer: loop { break 'outer; } \"☕\" }"),
        "☕"
    );
    assert_eq!(chars(r#"let b = b"x"; let c = b'y'; let d = "☕";"#), "☕");
    assert_eq!(
        chars(
            "fn a() -> &'static str { \"·\" }\n#[cfg(test)]\nmod tests { const X: &str = \"☕\"; }\nfn b() {}"
        ),
        "·"
    );
    assert_eq!(
        chars("#[cfg(test)]\nconst X: &str = \"☕\";\nconst Y: &str = \"…\";"),
        "…"
    );
    let lits = rust_literals("let a = 1;\nlet b = \"\n☕\";");
    assert_eq!(lits, [Lit { line: 3, ch: '☕' }]);
    let swift: String = swift_literals("let a = \"☕ \\(x)\" // ☕\nlet b = \"\\u{2615}\"")
        .iter()
        .map(|l| l.ch)
        .collect();
    assert_eq!(swift, "☕☕");
}

/// What the guard refuses: an unmeasured glyph, an icon outside its file, a computed char in
/// a file nobody vouched for; and what it lets through.
#[test]
fn the_guard_refuses_an_unmeasured_glyph() {
    assert_eq!(violations("qoop-tui/src/x.rs", r#"let a = "☕";"#).len(), 1);
    assert!(violations("qoop-tui/src/x.rs", r#"let a = "─│⠒ ← ✓";"#).is_empty());
    let cup = format!("let a = '{}';", qoop_tui::icons::CUP);
    assert_eq!(violations("qoop-tui/src/sidebar.rs", &cup).len(), 1);
    assert!(violations(ICONS, &cup).is_empty());
    assert_eq!(violations("qoop-tui/src/x.rs", "let a = \"⏺\";").len(), 1);
    assert!(violations("qoop-runtime/src/idle.rs", "let a = \"⏺\";").is_empty());
    assert_eq!(
        violations("qoop-tui/src/x.rs", "let a = char::from_u32(0x2615);").len(),
        1
    );
    // Past a test item in the middle of a file too, and never in a comment or a test.
    let middle = "#[cfg(test)]\nfn t() {}\nfn f() { char::from_u32(9); }";
    assert_eq!(violations("qoop-tui/src/x.rs", middle).len(), 1);
    let quiet = "// char::from_u32\n#[cfg(test)]\nmod tests { fn t() { char::from_u32(9); } }";
    assert!(violations("qoop-tui/src/x.rs", quiet).is_empty());
    assert_eq!(
        violations("qoop-cli/notch/x.swift", "let a = \"☕\"").len(),
        1
    );
}
