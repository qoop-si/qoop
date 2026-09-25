//! `qoro avatar`: pick the terminal pet, show it, nothing else.
//!
//! The body is fixed (the darko box) and so are the colours: every option wears
//! product tokens only, which is why this screen offers no colour picker. A batch
//! shows six, `r` deals a fresh random batch, a number keeps one, `q` quits
//! without touching anything. The choice lands in `avatar.json` under the
//! person's own configuration, where the sidebar reads it back.
//!
//! The pets are drawn on the same 16 by 12 grid as the design mock, two pixels
//! per terminal row through the upper half block, so what is picked is what the
//! mock showed. Piped input works the same as typed input, and EOF quits: a
//! gate that runs this with no terminal attached must terminate on its own.

use std::io::{IsTerminal, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use qoro_runtime::avatar::{
    self, Avatar, BODY, BODY_INK, EYE_STYLES, GRID_H, GRID_W, HAIR_INKS, HAIR_STYLES, art_cells,
    bg, fg, pixel,
};

/// How many pets a batch holds.
const BATCH: usize = 6;

/// The first batch: the picks that survived the mock rounds, fixed.
const CURATED: [(&str, u8, &str); BATCH] = [
    ("block", 212, "big"),
    ("mohawk", 212, "big"),
    ("spikes", 214, "bigsmall"),
    ("flat", 39, "squint"),
    ("cap", 226, "small"),
    ("beacon", 35, "small"),
];

/// `qoro avatar ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("pick") => pick(),
        Some("show") => show(),
        None if !std::io::stdin().is_terminal() => show(),
        None => pick(),
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; usage: qoro avatar [pick|show]"
        ))),
    }
}

/// Print the current avatar, or that there is none yet.
fn show() -> std::io::Result<()> {
    match avatar::load() {
        Some(found) => {
            println!("{}", found.name());
            print!("{}", render(&found, true));
            std::io::stdout().flush()
        }
        None => {
            println!("no avatar picked yet (qoro avatar pick to choose one)");
            Ok(())
        }
    }
}

/// The picker loop: batches until a number is kept or it is quit.
fn pick() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut batch = curated_batch();
    loop {
        print!("{batch}");
        print!("pick 1-6, r for a fresh batch, q to quit\n> ");
        std::io::stdout().flush()?;
        let line = read_line(&mut input)?;
        let Some(line) = line else {
            return Ok(());
        };
        match parse_choice(&line) {
            Choice::Keep(n) => {
                let kept = batch.into_pets()[n - 1].clone();
                avatar::save(&kept)?;
                println!("kept {}", kept.name());
                return Ok(());
            }
            Choice::Fresh => batch = random_batch(),
            Choice::Quit => return Ok(()),
            Choice::Invalid => continue,
        }
    }
}

/// A line of stdin, without the newline. `None` on EOF.
fn read_line(input: &mut impl Read) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => {
                if bytes.is_empty() {
                    return Ok(None);
                }
                break;
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                bytes.push(byte[0]);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Some(String::from_utf8_lossy(&bytes).trim().to_string()))
}

enum Choice {
    Keep(usize),
    Fresh,
    Quit,
    Invalid,
}

/// A number keeps, `r` deals again, `q` and EOF quit, the rest re-prompts.
fn parse_choice(line: &str) -> Choice {
    match line.trim().to_lowercase().as_str() {
        "q" | "quit" | "n" => Choice::Quit,
        "r" | "random" | "more" => Choice::Fresh,
        other => match other.parse::<usize>() {
            Ok(n) if (1..=BATCH).contains(&n) => Choice::Keep(n),
            _ => Choice::Invalid,
        },
    }
}

struct Batch {
    pets: Vec<Avatar>,
}

impl Batch {
    fn into_pets(self) -> Vec<Avatar> {
        self.pets
    }
}

impl std::fmt::Display for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "your avatar (body is fixed, six per batch):")?;
        let arts: Vec<Vec<String>> = self.pets.iter().map(art_lines).collect();
        for row in 0..BATCH / 2 {
            let base = row * 2;
            let left = &self.pets[base];
            let right = self.pets.get(base + 1);
            writeln!(
                f,
                "[{}] {:<16}  {}",
                base + 1,
                truncate(&left.name(), 16),
                right
                    .map(|p| format!("[{}] {}", base + 2, p.name()))
                    .unwrap_or_default()
            )?;
            for line in 0..arts[base]
                .len()
                .max(arts.get(base + 1).map(|a| a.len()).unwrap_or(0))
            {
                write!(
                    f,
                    "{}",
                    arts[base].get(line).map(String::as_str).unwrap_or("")
                )?;
                if let Some(art) = arts.get(base + 1) {
                    write!(f, "  {}", art.get(line).map(String::as_str).unwrap_or(""))?;
                }
                writeln!(f)?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

fn truncate(name: &str, width: usize) -> String {
    if name.len() <= width {
        name.to_string()
    } else {
        name.chars().take(width).collect()
    }
}

fn curated_batch() -> Batch {
    Batch {
        pets: CURATED
            .iter()
            .map(|(hair, ink, eyes)| Avatar {
                body: BODY.into(),
                hair: hair.to_string(),
                eyes: eyes.to_string(),
                hair_ink: *ink,
                body_ink: BODY_INK,
            })
            .collect(),
    }
}

fn random_batch() -> Batch {
    let mut rng = Rng::new();
    let mut pets = Vec::with_capacity(BATCH);
    for _ in 0..BATCH {
        pets.push(Avatar {
            body: BODY.into(),
            hair: HAIR_STYLES[rng.below(HAIR_STYLES.len())].into(),
            eyes: EYE_STYLES[rng.below(EYE_STYLES.len())].into(),
            hair_ink: HAIR_INKS[rng.below(HAIR_INKS.len())],
            body_ink: BODY_INK,
        });
    }
    Batch { pets }
}

/// A tiny generator: the batch must differ run to run, and there is no rand crate.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37);
        Rng(nanos ^ (std::process::id() as u64).wrapping_mul(0x9e3779b9))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Terminal rows for the picker: the shared cells with escapes, trimmed of
/// all-empty pairs at both ends like the mock's own padding.
fn art_lines(pet: &Avatar) -> Vec<String> {
    if std::env::var("NO_COLOR").is_ok() {
        return ascii_lines(pet);
    }
    let mut out: Vec<(String, bool)> = art_cells(pet)
        .iter()
        .map(|row| {
            let mut line = String::new();
            let mut cur: (Option<u8>, Option<u8>) = (None, None);
            let mut filled = false;
            for (top, bottom) in row {
                filled = filled || top.is_some() || bottom.is_some();
                if *top != cur.0 {
                    line.push_str(&(*top).map(fg).unwrap_or_else(|| "\x1b[39m".to_string()));
                    cur.0 = *top;
                }
                if *bottom != cur.1 {
                    line.push_str(&(*bottom).map(bg).unwrap_or_else(|| "\x1b[49m".to_string()));
                    cur.1 = *bottom;
                }
                line.push(if top.is_some() || bottom.is_some() {
                    '\u{2580}'
                } else {
                    ' '
                });
            }
            line.push_str("\x1b[0m");
            (line, filled)
        })
        .collect();
    while out.len() > 1 && !out.first().map(|(_, f)| *f).unwrap_or(true) {
        out.remove(0);
    }
    while out.len() > 1 && !out.last().map(|(_, f)| *f).unwrap_or(true) {
        out.pop();
    }
    out.into_iter().map(|(line, _)| line).collect()
}

/// The honest fallback: one hash per filled pixel, no colour.
fn ascii_lines(pet: &Avatar) -> Vec<String> {
    let mut out = Vec::new();
    for y in 0..GRID_H {
        let mut line = String::new();
        for x in 0..GRID_W {
            line.push(if pixel(pet, x, y).is_some() { '#' } else { ' ' });
        }
        out.push(line);
    }
    while out.len() > 1 && out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out
}

/// The whole pet as text, for `show`.
fn render(pet: &Avatar, _colour: bool) -> String {
    art_lines(pet).join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The curated batch is six valid avatars with six different names.
    #[test]
    fn the_curated_batch_is_six_valid_pets() {
        let batch = curated_batch();
        assert_eq!(batch.pets.len(), BATCH);
        for pet in &batch.pets {
            assert!(pet.valid(), "{pet:?} is not a valid avatar");
        }
    }

    /// Random batches stay inside the curated sets.
    #[test]
    fn random_batches_stay_inside_the_sets() {
        for _ in 0..20 {
            for pet in random_batch().pets {
                assert!(pet.valid(), "{pet:?} escaped the curated sets");
            }
        }
    }

    /// A number keeps, `r` deals again, `q` quits, the rest re-prompts.
    #[test]
    fn choices_read_as_the_prompt_says() {
        assert!(matches!(parse_choice("3"), Choice::Keep(3)));
        assert!(matches!(parse_choice(" 6 "), Choice::Keep(6)));
        assert!(matches!(parse_choice("r"), Choice::Fresh));
        assert!(matches!(parse_choice("q"), Choice::Quit));
        assert!(matches!(parse_choice("0"), Choice::Invalid));
        assert!(matches!(parse_choice("7"), Choice::Invalid));
        assert!(matches!(parse_choice("x"), Choice::Invalid));
    }
}
