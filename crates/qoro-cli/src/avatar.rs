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
    self, Avatar, BODY, BODY_INK, EYE_STYLES, EYE_TITLES, GRID_H, GRID_W, HAIR_INK_TITLES,
    HAIR_INKS, HAIR_STYLES, HAIR_TITLES, Paint, art_cells, bg, fg, paint, pixel,
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
    let help = "usage: qoro avatar [pick|show [--json]|options --json|set [--hair H] [--hair-ink N] [--eyes E]]";
    match args.get(2).map(String::as_str) {
        Some("pick") => pick(),
        Some("show") if args.get(3).map(String::as_str) == Some("--json") => show_json(),
        Some("show") => show(),
        Some("options") if args.get(3).map(String::as_str) == Some("--json") => options_json(),
        Some("set") => {
            set(&args[3..]).map_err(|why| std::io::Error::other(format!("{why}; {help}")))
        }
        None if !std::io::stdin().is_terminal() => show(),
        None => pick(),
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; {help}"
        ))),
    }
}

/// Every part a pet may have, for a picker that is not this terminal one: each hair and
/// eye style by id and title, each hair colour by its ink, title and `#rrggbb`.
fn options_json() -> std::io::Result<()> {
    let named = |ids: &[&str], titles: &[&str]| -> Vec<serde_json::Value> {
        ids.iter()
            .zip(titles)
            .map(|(id, title)| serde_json::json!({ "id": id, "title": title }))
            .collect()
    };
    let inks: Vec<serde_json::Value> = HAIR_INKS
        .iter()
        .zip(HAIR_INK_TITLES)
        .map(|(ink, title)| serde_json::json!({ "ink": ink, "title": title, "hex": avatar::ink_hex(*ink) }))
        .collect();
    let value = serde_json::json!({
        "hair": named(&HAIR_STYLES, &HAIR_TITLES),
        "hair_ink": inks,
        "eyes": named(&EYE_STYLES, &EYE_TITLES),
    });
    println!("{value}");
    Ok(())
}

/// Change some parts of the pet and keep the rest; with none picked yet, the parts start
/// from the first curated one. Only parts the picker itself offers are taken.
fn set(args: &[String]) -> Result<(), String> {
    let (hair, ink, eyes) = CURATED[0];
    let mut pet = avatar::load().unwrap_or(Avatar {
        body: BODY.to_string(),
        hair: hair.to_string(),
        eyes: eyes.to_string(),
        hair_ink: ink,
        body_ink: BODY_INK,
    });
    if args.is_empty() {
        return Err("name at least one part".to_string());
    }
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--hair" => pet.hair = value.clone(),
            "--eyes" => pet.eyes = value.clone(),
            "--hair-ink" => {
                pet.hair_ink = value
                    .parse()
                    .map_err(|_| format!("--hair-ink {value} is not a number"))?
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    if !pet.valid() {
        return Err(
            "that is not a part the picker offers (qoro avatar options --json)".to_string(),
        );
    }
    avatar::save(&pet).map_err(|e| e.to_string())?;
    println!("{}", pet.name());
    Ok(())
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

/// The picked avatar as pixels, for whatever draws it outside a terminal: `pixels` is
/// the whole grid, rows top first, each pixel a `#rrggbb` or `null`. With none picked
/// yet it prints `null`, so a reader tells "no pet" from "could not ask".
fn show_json() -> std::io::Result<()> {
    let value = avatar::load().map(|pet| {
        let pixels: Vec<Vec<Option<String>>> = (0..GRID_H)
            .map(|y| {
                (0..GRID_W)
                    .map(|x| pixel(&pet, x, y).map(avatar::ink_hex))
                    .collect()
            })
            .collect();
        serde_json::json!({
            "name": pet.name(),
            "hair": pet.hair,
            "hair_ink": pet.hair_ink,
            "eyes": pet.eyes,
            "pixels": pixels,
        })
    });
    println!("{}", value.unwrap_or(serde_json::Value::Null));
    Ok(())
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
pub(crate) fn read_line(input: &mut impl Read) -> std::io::Result<Option<String>> {
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
            for &cell in row {
                filled = filled || cell != (None, None);
                // The picker cannot know the terminal's background, so a top alone stays
                // the upper half block; everything else keeps its lit top a background.
                let (glyph, fore, back) = match paint(cell) {
                    Paint::Blank => (' ', cur.0, None),
                    Paint::Fill(n) => (' ', cur.0, Some(n)),
                    Paint::Lower { ink, over } => ('\u{2584}', Some(ink), over),
                    Paint::Upper(n) => ('\u{2580}', Some(n), None),
                };
                if fore != cur.0 {
                    line.push_str(&fore.map(fg).unwrap_or_else(|| "\x1b[39m".to_string()));
                    cur.0 = fore;
                }
                if back != cur.1 {
                    line.push_str(&back.map(bg).unwrap_or_else(|| "\x1b[49m".to_string()));
                    cur.1 = back;
                }
                line.push(glyph);
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
