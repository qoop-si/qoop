//! A note to qoop's maker about qass's own way of working, in a fixed form that is checked the
//! same way on the machine and again on the hub.
//!
//! The form is reduced, not proven free of data: printable ASCII, the free text made only of
//! common English words (SCOWL, names dropped) and qoop's own words, numbers only in `often`,
//! and spelled out separators refused. A name, a path, a host or an encoded string fails on the
//! word list; a false refusal costs a rewrite. The machine also refuses the person's own names
//! ([`own_name_problems`]), which it reads locally and never sends.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub const AREAS: [&str; 5] = ["watcher", "skill", "command", "screen", "install"];
/// The agents a note may name; any other is sent as `other`.
pub const AGENTS: [&str; 8] = [
    "claude", "codex", "gemini", "opencode", "cursor", "amp", "goose", "other",
];
/// The largest note, as [`Note::text`] writes it.
pub const MAX_BYTES: usize = 1024;

/// SCOWL sizes up to 50 with their inflections, lower case, names dropped
/// (`scripts/report-words.py`), one a line after its notice.
const WORDS: &str = include_str!("report-words.txt");

/// qoop's own words, and the common ones of its trade the list lacks. Exempt from the own-name
/// rule, so a space named `qass` or `main` does not ban the word.
const GLOSSARY: &[&str] = &[
    "agent",
    "ascii",
    "background",
    "batch",
    "board",
    "bracketed",
    "busy",
    "cli",
    "config",
    "ctrl",
    "daemon",
    "default",
    "esc",
    "footer",
    "generation",
    "helper",
    "hook",
    "idle",
    "json",
    "keystroke",
    "lane",
    "main",
    "menu",
    "monitor",
    "paste",
    "prompt",
    "qass",
    "qoop",
    "remote",
    "repo",
    "restart",
    "sidebar",
    "skill",
    "snapshot",
    "space",
    "stage",
    "stderr",
    "stdin",
    "stdout",
    "subagent",
    "tab",
    "timeout",
    "timestamp",
    "toml",
    "transcript",
    "tui",
    "unicode",
    "wake",
    "watcher",
    "worker",
    "worktree",
];

/// Common words refused all the same: they spell out a separator or a number, and numbers
/// live only in `often` (`one` and `two` stay: prose needs them, and they spell no port).
const BANNED: &[&str] = &[
    "slash",
    "backslash",
    "tilde",
    "dot",
    "point",
    "period",
    "colon",
    "stroke",
    "dash",
    "hyphen",
    "underscore",
    "port",
    "zero",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "billion",
];

/// The free text fields, each with its longest length; `fix` alone may be empty.
const TEXT: [(&str, usize); 3] = [("what", 200), ("cost", 160), ("fix", 200)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub area: String,
    pub what: String,
    pub cost: String,
    pub often: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
    /// The qoop version and the agent qass runs on: filled by the code, never by the model.
    pub version: String,
    pub agent: String,
}

impl Note {
    /// One `field: value` line each, `fix` left out when empty: what is kept, shown and sent.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (key, value) in self.fields() {
            if !(key == "fix" && value.is_empty()) {
                out.push_str(&format!("{key}: {value}\n"));
            }
        }
        out
    }

    /// Read [`Note::text`]'s lines; a model writes the first five, the code adds the rest.
    pub fn parse(text: &str) -> Result<Note, String> {
        let mut note = Note::default();
        let mut seen = HashSet::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Some((key, value)) = line.split_once(':') else {
                return Err("each line is `field: value`".into());
            };
            let key = key.trim();
            let slot = match key {
                "area" => &mut note.area,
                "what" => &mut note.what,
                "cost" => &mut note.cost,
                "often" => &mut note.often,
                "fix" => &mut note.fix,
                "version" => &mut note.version,
                "agent" => &mut note.agent,
                _ => {
                    return Err(format!(
                        "unknown field `{}`; the fields are area, what, cost, often, fix",
                        shown(key)
                    ));
                }
            };
            if !seen.insert(key.to_string()) {
                return Err(format!("`{key}` is given twice"));
            }
            *slot = value.trim().to_string();
        }
        Ok(note)
    }

    fn fields(&self) -> [(&'static str, &str); 7] {
        [
            ("area", &self.area),
            ("what", &self.what),
            ("cost", &self.cost),
            ("often", &self.often),
            ("fix", &self.fix),
            ("version", &self.version),
            ("agent", &self.agent),
        ]
    }

    fn get(&self, key: &str) -> &str {
        self.fields()
            .into_iter()
            .find(|(k, _)| *k == key)
            .map_or("", |(_, v)| v)
    }
}

/// Every rule `note` breaks; empty when it passes. The words name what failed without
/// repeating a character outside the rules.
pub fn problems(note: &Note) -> Vec<String> {
    let mut out = Vec::new();
    if !AREAS.contains(&note.area.as_str()) {
        out.push(format!("area: one of {}", AREAS.join(", ")));
    }
    for (key, max) in TEXT {
        text_problems(key, note.get(key), max, &mut out);
    }
    if !often_ok(&note.often) {
        out.push("often: `<count> times in <number> minutes|hours|days|weeks`".into());
    }
    let version = &note.version;
    if version.len() > 20
        || version.split('.').count() != 3
        || !version
            .split('.')
            .all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
    {
        out.push("version: the qoop version, `x.y.z`".into());
    }
    if !AGENTS.contains(&note.agent.as_str()) {
        out.push(format!("agent: one of {}", AGENTS.join(", ")));
    }
    if note.text().len() > MAX_BYTES {
        out.push(format!("the note is over {MAX_BYTES} bytes"));
    }
    out
}

/// Every place the free text names one of `names` (the person's user, host, folders, spaces,
/// tabs, remotes...), matched with separators taken out: `qoop-tui`, `qoop tui` and `qooptui`
/// are one. A name under four letters counts only as a whole word.
pub fn own_name_problems(note: &Note, names: &[String]) -> Vec<String> {
    let own: HashSet<String> = names
        .iter()
        .map(|n| squash(n))
        .filter(|n| n.len() >= 2 && !glossary(n))
        .collect();
    let mut out = Vec::new();
    for (key, _) in TEXT {
        let words: Vec<String> = parts(note.get(key))
            .map(|(_, w)| w.to_ascii_lowercase())
            .collect();
        // `web one` is the host `web1`: the two number words a note may hold, also as digits.
        let digits = |w: &String| match w.as_str() {
            "one" => "1".to_string(),
            "two" => "2".to_string(),
            w => w.to_string(),
        };
        for start in 0..words.len() {
            for len in 1..=3.min(words.len() - start) {
                let run = &words[start..start + len];
                let joined = run.concat();
                let as_digits: String = run.iter().map(digits).collect();
                if (len == 1 || joined.len() >= 4)
                    && (own.contains(&joined) || own.contains(&as_digits))
                {
                    out.push(format!(
                        "{key}: `{}` names something of yours",
                        words[start..start + len].join(" ")
                    ));
                }
            }
        }
    }
    out.dedup();
    out
}

/// A banned word in any of its forms: `ports`, `port's`, `sevens`, `nineties`, `twentieth`.
fn banned(word: &str) -> bool {
    const ORDINALS: [&str; 10] = [
        "third", "fifth", "eighth", "ninth", "twelfth", "dozen", "nought", "naught", "aught", "nil",
    ];
    let word = word.trim_end_matches("'s").trim_end_matches('\'');
    let forms = [
        Some(word.to_string()),
        word.strip_suffix('s').map(str::to_string),
        word.strip_suffix("es").map(str::to_string),
        word.strip_suffix("th").map(str::to_string),
        word.strip_suffix("ths").map(str::to_string),
        word.strip_suffix("ieth").map(|w| format!("{w}y")),
        word.strip_suffix("ieths").map(|w| format!("{w}y")),
        word.strip_suffix("ies").map(|w| format!("{w}y")),
        word.strip_suffix("ed").map(str::to_string),
    ];
    forms
        .into_iter()
        .flatten()
        .any(|f| BANNED.contains(&f.as_str()) || ORDINALS.contains(&f.as_str()))
}

/// `I` and its contractions, the one word written with a capital inside a sentence.
fn first_person(word: &str) -> bool {
    matches!(word, "I" | "I'm" | "I've" | "I'll" | "I'd")
}

fn text_problems(key: &str, value: &str, max: usize, out: &mut Vec<String>) {
    if value.is_empty() {
        if key != "fix" {
            out.push(format!("{key}: empty"));
        }
        return;
    }
    if value.len() > max {
        out.push(format!("{key}: over {max} characters"));
    }
    if let Some(c) = value
        .chars()
        .find(|c| !(c.is_ascii_alphabetic() || " ,.;()'?!-".contains(*c)))
    {
        out.push(if c.is_ascii_digit() {
            format!("{key}: no digits; numbers go in `often`")
        } else if c.is_ascii_graphic() {
            format!("{key}: `{c}` is not allowed; letters, spaces and , . ; ( ) ' ? ! - only")
        } else {
            format!("{key}: a character outside letters, spaces and , . ; ( ) ' ? ! -")
        });
        return;
    }
    for (start, word) in parts(value) {
        let lower = word.to_ascii_lowercase();
        let capital = !first_person(word)
            && (word.bytes().skip(1).any(|b| b.is_ascii_uppercase())
                || (word.as_bytes()[0].is_ascii_uppercase() && !sentence_start(value, start)));
        if capital {
            out.push(format!(
                "{key}: `{word}` has a capital letter inside a sentence"
            ));
        } else if banned(&lower) {
            out.push(format!("{key}: `{word}` is not allowed"));
        } else if !glossary(&lower) && !common(&lower) && !first_person(word) {
            out.push(format!("{key}: `{word}` is not a common English word"));
        }
    }
}

/// The words of `text` and where each starts: letters only, so a hyphen or an apostrophe
/// splits one. The part after an apostrophe of a contraction (`isn't`, `it's`) is kept
/// with it when the whole is a common word.
fn parts(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        // A contraction: letters, one apostrophe, letters, and the whole a listed word.
        if i + 1 < bytes.len() && bytes[i] == b'\'' && bytes[i + 1].is_ascii_alphabetic() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            let whole = text[start..j].to_ascii_lowercase();
            if common(&whole) || glossary(&whole) || first_person(&text[start..j]) {
                i = j;
            }
        }
        out.push((start, &text[start..i]));
    }
    out.into_iter()
}

/// The first word of a field or of a sentence in it.
fn sentence_start(text: &str, at: usize) -> bool {
    text[..at]
        .trim_end()
        .chars()
        .last()
        .is_none_or(|c| matches!(c, '.' | '?' | '!'))
}

fn often_ok(often: &str) -> bool {
    let w: Vec<&str> = often.split(' ').collect();
    let number = |s: &str, max: usize| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes().all(|b| b.is_ascii_digit())
            && !s.starts_with('0')
    };
    w.len() == 5
        && number(w[0], 3)
        && matches!(w[1], "time" | "times")
        && w[2] == "in"
        && number(w[3], 3)
        && matches!(
            w[4],
            "minute" | "minutes" | "hour" | "hours" | "day" | "days" | "week" | "weeks"
        )
}

/// A `what` as notes are grouped by it, on the hub and when a machine looks for a note it kept
/// already: lower case, one space between words, no stop at the end.
pub fn group_key(what: &str) -> String {
    what.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', '?', ';', ','])
        .to_ascii_lowercase()
}

/// A word the free text may use: common English or qoop's own.
pub fn common_word(word: &str) -> bool {
    glossary(word) || common(word)
}

fn glossary(word: &str) -> bool {
    let word = word.strip_suffix("'s").unwrap_or(word);
    let stem = word
        .strip_suffix("es")
        .filter(|s| GLOSSARY.contains(s))
        .or_else(|| word.strip_suffix('s'))
        .unwrap_or(word);
    GLOSSARY.contains(&word) || GLOSSARY.contains(&stem)
}

fn common(word: &str) -> bool {
    static LIST: OnceLock<HashSet<&'static str>> = OnceLock::new();
    LIST.get_or_init(|| {
        WORDS
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    })
    .contains(word)
}

/// Lower case letters and digits only: how a name and a run of words are compared.
fn squash(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A key as an error may repeat it: printable ASCII, cut short.
fn shown(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(20)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(what: &str, cost: &str, often: &str, fix: &str) -> Note {
        Note {
            area: "watcher".into(),
            what: what.into(),
            cost: cost.into(),
            often: often.into(),
            fix: fix.into(),
            version: "0.9.5".into(),
            agent: "claude".into(),
        }
    }

    fn ok(what: &str) -> Note {
        note(
            what,
            "each wake reads the whole context again",
            "3 times in 2 hours",
            "",
        )
    }

    /// The plan's example, written within the rules by the adversarial reviewer.
    #[test]
    fn the_plans_example_passes_and_round_trips() {
        let n = note(
            "a remote with no open tab is reported as gone again at every restart of the watcher",
            "each restart wakes the assistant for nothing, and every wake reads its whole context again",
            "39 times in 7 hours",
            "save the empty snapshot once it was said, so a restart has nothing new to say",
        );
        assert_eq!(problems(&n), Vec::<String>::new());
        assert_eq!(Note::parse(&n.text()), Ok(n));
    }

    /// Round 1 of the review: each of these got through a blacklist.
    #[test]
    fn names_paths_hosts_and_encodings_fail() {
        for what in [
            "the watcher failed for john at his desk",
            "it broke in the project of r3cep",
            "a file under slash users slash someone was gone",
            "the hub at qoop dot si did not answer",
            "the build on buildbox timed out",
            "it said aGVsbG8gd29ybGQ in the log",
            "the tab named Acme kept waking",
            "a stale lock in google drive",
            "sync failed from london",
            "the turkey office machine slept",
            "the mark of the client was lost",
            "bekleme cok uzun surdu",
            "port eight was busy",
            "it was fine until the deploy at seven",
            "the agent saw acmecorp as a stranger",
            "the tab of zentrix slept",
        ] {
            assert!(!problems(&ok(what)).is_empty(), "{what}");
        }
    }

    #[test]
    fn hidden_and_foreign_characters_fail_without_being_repeated() {
        for what in [
            "the wat\u{200b}cher slept",
            "the watcher \u{202e}slept",
            "the watcher printed \u{1b}[31m",
            "bekleme çok uzun",
            "see /tmp for it",
            "mail me at a@b",
            "it took 5 tries",
        ] {
            let p = problems(&ok(what));
            assert!(!p.is_empty(), "{what}");
            assert!(p.iter().all(|e| e.is_ascii()), "{p:?}");
        }
    }

    #[test]
    fn often_is_a_count_and_a_span_only() {
        for good in [
            "1 time in 1 hour",
            "38 times in 7 hours",
            "999 times in 999 days",
        ] {
            assert!(often_ok(good), "{good}");
        }
        for bad in [
            "38 times in 7 hours at night",
            "at 3am",
            "038 times in 7 hours",
            "38 times between 1 and 4",
            "38 times in 7 months",
            "1000 times in 1 day",
            "lots",
        ] {
            assert!(!often_ok(bad), "{bad}");
        }
    }

    /// Round 4 of the review: each of these passed the first build.
    #[test]
    fn spelled_letters_number_forms_and_a_long_version_fail() {
        for what in [
            "the tab of r e see e p on gee i t h u bee slept",
            "the remote at the sevens and nines ports slept",
            "it stopped on the twentieth try, then the ninth",
            "the dashes and slashes of the name were lost",
            "a dozen tabs woke",
            "the nine's port's slept",
            "it slept in the nineties",
        ] {
            assert!(!problems(&ok(what)).is_empty(), "{what}");
        }
        let mut n = ok("the watcher slept");
        n.version = "1234567.12345678.12".into();
        assert_eq!(problems(&n), ["version: the qoop version, `x.y.z`"]);
    }

    #[test]
    fn i_may_be_written_inside_a_sentence() {
        assert_eq!(
            problems(&ok("the watcher slept when I'm away, and I saw it later")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_capital_only_starts_a_sentence() {
        assert!(problems(&ok("The watcher slept. It woke again.")).is_empty());
        assert!(!problems(&ok("the Watcher slept")).is_empty());
        assert!(!problems(&ok("the wATCHER slept")).is_empty());
    }

    #[test]
    fn contractions_hyphens_and_the_glossary() {
        assert_eq!(
            problems(&ok(
                "it isn't clear when qass's subagents end; the worktree stays"
            )),
            Vec::<String>::new()
        );
        assert_eq!(
            problems(&ok("a long-lived daemon restarts")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn fixed_fields_and_size() {
        let mut n = ok("the watcher slept");
        n.area = "bug".into();
        n.version = "1.2".into();
        n.agent = "me".into();
        assert_eq!(problems(&n).len(), 3, "{:?}", problems(&n));
        let mut n = ok("the watcher slept");
        n.what = "a ".repeat(101);
        assert!(problems(&n).iter().any(|p| p.contains("over 200")));
        n.cost = String::new();
        assert!(problems(&n).iter().any(|p| p == "cost: empty"));
    }

    #[test]
    fn notes_alike_share_a_group_key() {
        assert_eq!(
            group_key("The  watcher slept."),
            group_key("the watcher slept")
        );
        assert_ne!(group_key("the watcher slept"), group_key("a tab woke"));
    }

    #[test]
    fn parse_refuses_unknown_and_repeated_fields() {
        assert!(Note::parse("area: skill\nname: x\n").is_err());
        assert!(Note::parse("area: skill\narea: watcher\n").is_err());
        assert!(Note::parse("just a line\n").is_err());
    }

    #[test]
    fn own_names_fail_with_separators_taken_out() {
        let names = vec![
            "qoop-tui".to_string(),
            "us".into(),
            "Pat's laptop".into(),
            "main".into(),
            "harbor".into(),
            "web1".into(),
        ];
        let p = |what: &str| own_name_problems(&ok(what), &names);
        assert!(!p("the watcher in the harbor tab slept").is_empty());
        // A glossary word or a short common word inside another is not a name match.
        assert!(p("the space slept, then the menu came back for us").len() == 1);
        assert!(p("the main space slept").is_empty());
        assert!(!p("the web one remote slept").is_empty());
        let one = vec!["one".to_string()];
        assert!(!own_name_problems(&ok("the tab one slept"), &one).is_empty());
    }
}

#[cfg(test)]
mod corpus {
    use super::*;

    /// Thirty problems qass met in its own way of working (the quiet watcher plan, its night
    /// logs and its groups), written as notes: every one must pass.
    const NOTES: [[&str; 5]; 30] = [
        [
            "watcher",
            "every line the watcher prints wakes the assistant, even a line that only says an agent is working",
            "each wake reads the whole context again for nothing to do",
            "120 times in 9 hours",
            "hold lines that need no action and say them together later",
        ],
        [
            "watcher",
            "the monitor stops after half an hour and the assistant must start it again",
            "each start is a wake with nothing new in it",
            "17 times in 9 hours",
            "start the next watch before the old one ends",
        ],
        [
            "watcher",
            "a remote with no open tab is reported as gone again at every restart of the watcher",
            "each restart wakes the assistant for nothing, and every wake reads its whole context again",
            "39 times in 7 hours",
            "save the empty snapshot once it was said, so a restart has nothing new to say",
        ],
        [
            "watcher",
            "a link to a remote drops for a few seconds and comes back, and both are said at once",
            "two wakes for a change that never needed an answer",
            "22 times in 1 day",
            "say a link is down only when it stays down for a while",
        ],
        [
            "watcher",
            "an agent the assistant itself just prompted reports working, and that wakes the assistant again",
            "the assistant reads its own action back as news",
            "60 times in 8 hours",
            "remember who sent the last prompt and keep that echo quiet",
        ],
        [
            "skill",
            "an agent stops at a menu that asks to allow a command, and the assistant waits for it",
            "the work stands still until the owner looks",
            "6 times in 2 days",
            "tell each agent to answer its own menus by the rule it was given",
        ],
        [
            "watcher",
            "an agent hits its usage limit and sits idle until the limit resets",
            "hours of work are lost while another account has room",
            "3 times in 5 days",
            "switch the account at once when the screen says the limit is reached",
        ],
        [
            "skill",
            "every worker starts on the most expensive model, even for a small fix",
            "simple work costs as much as hard work",
            "14 times in 3 days",
            "pick a cheaper model for small and plain tasks",
        ],
        [
            "skill",
            "a worker starts on a vague request and builds the wrong thing",
            "the work is thrown away and started again",
            "4 times in 6 days",
            "ask the owner the open questions before the worker starts",
        ],
        [
            "command",
            "an agent does not see which item it is on or which step comes next",
            "steps are left unchecked and the board says less than the truth",
            "11 times in 2 days",
            "say the item, its steps and the next step with each request",
        ],
        [
            "command",
            "an item still says it waits on the owner after the owner answered",
            "the owner is asked again for something already answered",
            "5 times in 1 day",
            "move the stage back once the answer arrives",
        ],
        [
            "screen",
            "an item falls back to the first lane when its agent finished and its tab closed",
            "finished work looks like work nobody started",
            "3 times in 1 day",
            "keep the item in progress until the assistant closes it",
        ],
        [
            "skill",
            "the goals file grows past what the assistant can read in one go",
            "each wake reads a long file that is mostly old",
            "1 time in 1 day",
            "keep the next card first and move old lines out",
        ],
        [
            "watcher",
            "the watcher says an agent is idle the moment it pauses to think",
            "a false alarm wakes the assistant and it checks for nothing",
            "30 times in 6 hours",
            "wait a little before calling an agent idle",
        ],
        [
            "command",
            "the board file is written again on every wake even when nothing changed",
            "the assistant reads a file it already knows",
            "45 times in 9 hours",
            "write the board only when it changed",
        ],
        [
            "skill",
            "the assistant pastes a long brief into a worker instead of a short pointer to a file",
            "the worker's context fills with text it could read on its own",
            "8 times in 2 days",
            "write the brief to a file and send its name",
        ],
        [
            "watcher",
            "a closed tab is said twice, once as closed and once as gone",
            "two wakes for one event",
            "9 times in 1 day",
            "say a closed tab once",
        ],
        [
            "install",
            "a new build is installed while an agent still runs the old one",
            "the agent's hooks report to a binary that changed under it",
            "2 times in 1 week",
            "install once, after every pending change is merged",
        ],
        [
            "skill",
            "the assistant asks the owner a question with no headline, and the question is lost in a long message",
            "the owner answers late or not at all",
            "5 times in 3 days",
            "lead with a headline, then the question whole",
        ],
        [
            "watcher",
            "the daemon restarts and the watcher reports every agent as new",
            "a burst of wakes with nothing new to do",
            "2 times in 4 days",
            "compare with the last snapshot before saying an agent is new",
        ],
        [
            "command",
            "a task stays open after its tab closed, and the board counts it as live",
            "the owner sees work in progress that ended",
            "7 times in 3 days",
            "end a task when its tab closes",
        ],
        [
            "skill",
            "the assistant starts a second worker on an item that already has one",
            "two agents change the same files and one of them is thrown away",
            "2 times in 5 days",
            "check the item for a live agent before starting one",
        ],
        [
            "watcher",
            "a sleeping tab that wakes for a moment is said as busy",
            "a wake for an agent that has nothing to do",
            "15 times in 1 day",
            "say a tab is busy only once it starts real work",
        ],
        [
            "screen",
            "the settings page does not fit a small pane and its last rows are cut",
            "the owner cannot see or change those settings there",
            "1 time in 1 day",
            "let the page scroll to the rows it cuts",
        ],
        [
            "command",
            "an agent cannot tell a check failed because the command printed nothing",
            "the agent marks the step done though it never passed",
            "3 times in 2 days",
            "make the check say what it ran and what it saw",
        ],
        [
            "skill",
            "the assistant reads the same long log on every wake to find the last event",
            "each wake costs more than the work it starts",
            "50 times in 9 hours",
            "keep a short note of the last event read",
        ],
        [
            "watcher",
            "a worker that waits on its own subagent is reported as idle",
            "the assistant prompts a worker that was busy",
            "6 times in 1 day",
            "count a subagent wait as work",
        ],
        [
            "command",
            "a prompt sent to a tab that just closed fails without a word",
            "the assistant thinks the agent got its request",
            "2 times in 3 days",
            "fail loudly when the tab is gone",
        ],
        [
            "install",
            "the skill files are installed while the assistant runs from them",
            "the assistant reads a half written file",
            "1 time in 2 weeks",
            "write each file beside the old one and rename it into place",
        ],
        [
            "skill",
            "the assistant writes a long report after every small step",
            "the owner reads more than the news is worth",
            "20 times in 1 day",
            "say a small step in one line and keep the long report for the end",
        ],
    ];

    #[test]
    fn every_real_problem_written_as_a_note_passes() {
        let mut failed = Vec::new();
        for [area, what, cost, often, fix] in NOTES {
            let n = Note {
                area: area.into(),
                what: what.into(),
                cost: cost.into(),
                often: often.into(),
                fix: fix.into(),
                version: "0.9.5".into(),
                agent: "claude".into(),
            };
            failed.extend(problems(&n));
        }
        assert_eq!(failed, Vec::<String>::new());
    }
}
