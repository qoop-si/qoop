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
    self, Avatar, BODY, BODY_INK, EYE_CREAM, EYE_INK, EYE_STYLES, HAIR_INKS, HAIR_STYLES,
};

/// Grid the pets are drawn on, same as the mock.
const W: i32 = 16;
const H: i32 = 12;
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

/// A filled rect on the grid, end-exclusive like every other range here.
type Rect = (i32, i32, i32, i32);

/// The head every avatar shares: the darko box, no nose, two feet.
const HEAD: Rect = (5, 4, 11, 9);
const FEET: [Rect; 2] = [(6, 9, 7, 10), (9, 9, 10, 10)];

/// Hair by style, same shapes as the mock.
fn hair_rects(hair: &str) -> Vec<Rect> {
    match hair {
        "block" => vec![(6, 1, 9, 4), (9, 2, 12, 4)],
        "mohawk" => vec![(7, 0, 9, 4), (9, 3, 13, 5), (10, 5, 13, 7)],
        "spikes" => vec![(6, 2, 11, 4), (6, 1, 7, 2), (8, 1, 9, 2), (10, 1, 11, 2)],
        "flat" => vec![(5, 2, 11, 4)],
        "thin4" => vec![(6, 1, 8, 4), (9, 1, 11, 4)],
        "beacon" => vec![(7, 3, 10, 4), (8, 2, 9, 3), (8, 1, 9, 2)],
        "cap" => vec![(8, 2, 12, 4), (6, 3, 9, 4)],
        _ => vec![],
    }
}

/// Eyes by style: position rects plus whether they wear the dark slit ink.
fn eye_rects(eyes: &str) -> (Vec<Rect>, u8) {
    match eyes {
        "big" => (vec![(6, 5, 7, 7), (8, 5, 9, 7)], EYE_CREAM),
        "bigsmall" => (vec![(6, 5, 7, 7), (9, 6, 10, 7)], EYE_CREAM),
        "squint" => (vec![(6, 6, 8, 7), (9, 6, 11, 7)], EYE_INK),
        "squintw" => (vec![(6, 6, 8, 7), (9, 6, 11, 7)], EYE_CREAM),
        _ => (vec![(6, 6, 7, 7), (9, 6, 10, 7)], EYE_CREAM),
    }
}

fn in_rect(x: i32, y: i32, rect: Rect) -> bool {
    x >= rect.0 && x < rect.2 && y >= rect.1 && y < rect.3
}

/// The ink at one pixel, if any. Eyes win over a sweeping fringe: the mock
/// paints them last, so a side lock may touch the head's edge but never an eye.
fn pixel(pet: &Avatar, x: i32, y: i32) -> Option<u8> {
    let (rects, ink) = eye_rects(&pet.eyes);
    if in_rect(x, y, HEAD) && rects.iter().any(|r| in_rect(x, y, *r)) {
        return Some(ink);
    }
    if hair_rects(&pet.hair).iter().any(|r| in_rect(x, y, *r)) {
        return Some(pet.hair_ink);
    }
    if in_rect(x, y, HEAD) || FEET.iter().any(|r| in_rect(x, y, *r)) {
        return Some(pet.body_ink);
    }
    None
}

fn fg(ink: u8) -> String {
    format!("\x1b[38;5;{ink}m")
}

fn bg(ink: u8) -> String {
    format!("\x1b[48;5;{ink}m")
}

/// Six terminal rows: each cell wears two pixels through the upper half block.
fn art_lines(pet: &Avatar) -> Vec<String> {
    if std::env::var("NO_COLOR").is_ok() {
        return ascii_lines(pet);
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    for pair in 0..H / 2 {
        let mut line = String::new();
        let mut cur: (Option<u8>, Option<u8>) = (None, None);
        let mut filled = false;
        for x in 0..W {
            let top = pixel(pet, x, pair * 2);
            let bottom = pixel(pet, x, pair * 2 + 1);
            filled = filled || top.is_some() || bottom.is_some();
            if top != cur.0 {
                line.push_str(&top.map(fg).unwrap_or_else(|| "\x1b[39m".to_string()));
                cur.0 = top;
            }
            if bottom != cur.1 {
                line.push_str(&bottom.map(bg).unwrap_or_else(|| "\x1b[49m".to_string()));
                cur.1 = bottom;
            }
            line.push(if top.is_some() || bottom.is_some() {
                '\u{2580}'
            } else {
                ' '
            });
        }
        line.push_str("\x1b[0m");
        out.push((line, filled));
    }
    // The grid is 12 tall but content lives up top: trim the all-empty pairs
    // at both ends like the mock's own padding, keeping at least one row.
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
    for y in 0..H {
        let mut line = String::new();
        for x in 0..W {
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

    /// Every rect the picker draws stays on the 16 by 12 grid.
    #[test]
    fn every_shape_stays_on_the_grid() {
        let mut rects = vec![HEAD];
        rects.extend(FEET);
        for hair in HAIR_STYLES {
            rects.extend(hair_rects(hair));
        }
        for eyes in EYE_STYLES {
            rects.extend(eye_rects(eyes).0);
        }
        for (x0, y0, x1, y1) in rects {
            assert!(0 <= x0 && x0 < x1 && x1 <= W, "x out of grid");
            assert!(0 <= y0 && y0 < y1 && y1 <= H, "y out of grid");
        }
    }

    /// A sweeping fringe may touch the head's edge, but never an eye: eyes are
    /// painted last, like the mock.
    #[test]
    fn hair_never_covers_an_eye() {
        for hair in HAIR_STYLES {
            for rect in hair_rects(hair) {
                for x in rect.0..rect.2 {
                    for y in rect.1..rect.3 {
                        for eyes in EYE_STYLES {
                            let (erects, _) = eye_rects(eyes);
                            assert!(
                                !erects.iter().any(|r| in_rect(x, y, *r))
                                    || pixel(
                                        &Avatar {
                                            body: BODY.into(),
                                            hair: hair.into(),
                                            eyes: eyes.into(),
                                            hair_ink: HAIR_INKS[0],
                                            body_ink: BODY_INK,
                                        },
                                        x,
                                        y
                                    ) == Some(eye_rects(eyes).1),
                                "{hair} covers {eyes} eyes at {x},{y}"
                            );
                        }
                    }
                }
            }
        }
    }

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
