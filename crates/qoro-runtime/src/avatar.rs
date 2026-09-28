//! The person's picked terminal pet: what it is, where it lives, and nothing else.
//!
//! The picker (`qoro avatar pick` in qoro-cli) writes one small JSON file under the
//! person's own configuration, beside the hooks and the agent-detection overrides.
//! The sidebar reads it back to sign its foot with the avatar's two inks. Reading
//! is fail-closed like the setup ledger: a missing file, garbage in it, or values
//! outside the curated sets below is no avatar, never an error and never a guess.
//!
//! The inks are 256-palette entries, all at or above 16: the first sixteen belong
//! to the program in the pane and furniture never spends them (see theme).

use std::path::PathBuf;

/// Override for tests and scripts: read and write this path instead of the default.
pub const AVATAR_ENV: &str = "QORO_AVATAR_FILE";

/// The one body. There was a builder with five bodies once; the choice that
/// survived is the darko box, and the setup screen offers no body picker.
pub const BODY: &str = "darko-box";
/// Its ink: azure 33, the brighter step of the `0044D7` blue family (theme).
pub const BODY_INK: u8 = 33;
/// Cream eye: text 231. Dark slit: black 16, the first unthemed entry.
pub const EYE_CREAM: u8 = 231;
pub const EYE_INK: u8 = 16;

/// The hair the setup screen offers, in picker order.
pub const HAIR_STYLES: [&str; 8] = [
    "none", "block", "mohawk", "spikes", "flat", "thin4", "beacon", "cap",
];
/// Display names for the picker, same order as above.
pub const HAIR_TITLES: [&str; 8] = [
    "Bare", "Block", "Mohawk", "Spikes", "Flat Top", "Thin 4", "Beacon", "Side Cap",
];
/// The only inks a hair may wear, all product tokens, none below 16.
pub const HAIR_INKS: [u8; 8] = [212, 214, 226, 155, 208, 35, 135, 39];
/// Display names for the inks, same order as above.
pub const HAIR_INK_TITLES: [&str; 8] = [
    "Pink", "Gold", "Yellow", "Lime", "Blocked", "Done", "Violet", "Sky",
];
/// The eyes the setup screen offers, in picker order.
pub const EYE_STYLES: [&str; 5] = ["small", "big", "bigsmall", "squint", "squintw"];
/// Display names for the eyes, same order as above.
pub const EYE_TITLES: [&str; 5] = ["Small", "Big", "Odd", "Squint", "Wide Squint"];

/// The pixel grid every pet is drawn on, same as the design mock.
pub const GRID_W: i32 = 16;
pub const GRID_H: i32 = 12;
/// Half-block art rows the sidebar paints: pairs 0 to 4. Pair 5 is always
/// empty, and leading empty pairs stay blank so the pet stands on the foot.
pub const ART_ROWS: usize = 5;
/// Art columns: the full grid width, which is why the panel needs a wide column.
pub const ART_COLS: usize = 16;

/// A filled rect on the grid, end-exclusive like every other range here.
pub type Rect = (i32, i32, i32, i32);

/// The head every avatar shares: the darko box, no nose, two feet.
pub const HEAD: Rect = (5, 4, 11, 9);
/// The feet, worn at the bottom row.
pub const FEET: [Rect; 2] = [(6, 9, 7, 10), (9, 9, 10, 10)];

/// Hair by style, same shapes as the mock. A sweeping fringe may touch the
/// head's edge; eyes are painted last and always win.
pub fn hair_rects(hair: &str) -> Vec<Rect> {
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

/// Eyes by style: position rects plus the ink they wear.
pub fn eye_rects(eyes: &str) -> (Vec<Rect>, u8) {
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

/// The ink at one pixel, if any.
pub fn pixel(pet: &Avatar, x: i32, y: i32) -> Option<u8> {
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

/// One terminal cell of half-block art: the two pixels it wears, top first.
pub type Cell = (Option<u8>, Option<u8>);

/// Foreground and background escapes for one ink, shared by the picker and
/// the sidebar overlay so the same pet paints the same everywhere.
pub fn fg(ink: u8) -> String {
    format!("\x1b[38;5;{ink}m")
}

/// See [`fg`].
pub fn bg(ink: u8) -> String {
    format!("\x1b[48;5;{ink}m")
}

/// The colour xterm paints an ink in, as `#rrggbb`, for whatever draws the pet
/// outside a terminal. Every ink here is at or above 16, the part of the palette
/// with one agreed formula: a 6x6x6 cube, then a 24 step grey ramp.
pub fn ink_hex(ink: u8) -> String {
    let (r, g, b) = if ink >= 232 {
        let v = 8 + (ink - 232) * 10;
        (v, v, v)
    } else {
        let step = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
        let n = ink.saturating_sub(16);
        (step(n / 36), step(n / 6 % 6), step(n % 6))
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The pet as five rows of sixteen half-block cells, top row first. Fixed
/// size so the sidebar can reserve exactly this much room every frame.
pub fn art_cells(pet: &Avatar) -> [[Cell; ART_COLS]; ART_ROWS] {
    let mut out = [[(None, None); ART_COLS]; ART_ROWS];
    for (pair, row) in out.iter_mut().enumerate() {
        for (x, cell) in row.iter_mut().enumerate() {
            let x = x as i32;
            *cell = (
                pixel(pet, x, pair as i32 * 2),
                pixel(pet, x, pair as i32 * 2 + 1),
            );
        }
    }
    out
}

/// What `qoro avatar pick` writes and the sidebar reads.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Avatar {
    pub body: String,
    pub hair: String,
    pub eyes: String,
    pub hair_ink: u8,
    pub body_ink: u8,
}

impl Avatar {
    /// The name the picker prints, from the parts: "Mohawk Pink".
    pub fn name(&self) -> String {
        let hair = HAIR_STYLES
            .iter()
            .position(|h| *h == self.hair)
            .map(|i| HAIR_TITLES[i])
            .unwrap_or("Bare");
        let ink = HAIR_INKS
            .iter()
            .position(|n| *n == self.hair_ink)
            .map(|i| HAIR_INK_TITLES[i])
            .unwrap_or("Pink");
        if self.hair == "none" {
            return "Bare".to_string();
        }
        format!("{hair} {ink}")
    }

    /// True only for values the picker itself can produce. Anything else is
    /// treated as no avatar: a hand-edited file must not smuggle a themed
    /// colour or an unknown shape into the furniture.
    pub fn valid(&self) -> bool {
        self.body == BODY
            && self.body_ink == BODY_INK
            && HAIR_STYLES.contains(&self.hair.as_str())
            && EYE_STYLES.contains(&self.eyes.as_str())
            && HAIR_INKS.contains(&self.hair_ink)
    }
}

/// Where the avatar lives: the override when set, else `qoro/avatar.json`
/// under the person's own configuration, beside the hooks directory.
pub fn avatar_path() -> PathBuf {
    match std::env::var(AVATAR_ENV) {
        Ok(path) if !path.is_empty() => return PathBuf::from(path),
        _ => {}
    }
    crate::paths::config_dir().join("avatar.json")
}

/// Read and validate, from an explicit path so tests never touch a home.
pub fn load_from(path: &std::path::Path) -> Option<Avatar> {
    let text = std::fs::read_to_string(path).ok()?;
    let avatar: Avatar = serde_json::from_str(&text).ok()?;
    avatar.valid().then_some(avatar)
}

/// Read and validate, from [`avatar_path`].
pub fn load() -> Option<Avatar> {
    load_from(&avatar_path())
}

/// Write, creating the directory. The file is small enough to write whole.
pub fn save_to(path: &std::path::Path, avatar: &Avatar) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(avatar).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

/// Write to [`avatar_path`].
pub fn save(avatar: &Avatar) -> std::io::Result<()> {
    save_to(&avatar_path(), avatar)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inks come out the colours xterm paints them: cube corners, a mid step, the
    /// grey ramp at both ends.
    #[test]
    fn inks_are_the_colours_xterm_paints() {
        assert_eq!(ink_hex(16), "#000000");
        assert_eq!(ink_hex(33), "#0087ff");
        assert_eq!(ink_hex(35), "#00af5f");
        assert_eq!(ink_hex(231), "#ffffff");
        assert_eq!(ink_hex(232), "#080808");
        assert_eq!(ink_hex(255), "#eeeeee");
    }

    fn sample() -> Avatar {
        Avatar {
            body: BODY.into(),
            hair: "mohawk".into(),
            eyes: "big".into(),
            hair_ink: 212,
            body_ink: BODY_INK,
        }
    }

    /// A picker result round-trips through the file it writes.
    #[test]
    fn a_picked_avatar_survives_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("avatar.json");
        save_to(&path, &sample()).expect("save");
        assert_eq!(load_from(&path), Some(sample()));
    }

    /// Missing, garbage, and hand-edited files are no avatar, never an error.
    #[test]
    fn a_missing_or_broken_file_is_no_avatar() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(load_from(&dir.path().join("absent.json")), None);
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").expect("write");
        assert_eq!(load_from(&bad), None);
        let mut edited = sample();
        edited.hair_ink = 5;
        let themed = dir.path().join("themed.json");
        save_to(&themed, &edited).expect("save");
        assert_eq!(
            load_from(&themed),
            None,
            "a themed colour must not reach the furniture"
        );
        let mut renamed = sample();
        renamed.hair = "dragon".into();
        let strange = dir.path().join("strange.json");
        save_to(&strange, &renamed).expect("save");
        assert_eq!(load_from(&strange), None);
    }

    /// The curated inks never spend a colour the user has themed.
    #[test]
    fn no_curated_ink_is_a_themed_colour() {
        for ink in HAIR_INKS.into_iter().chain([BODY_INK, EYE_CREAM, EYE_INK]) {
            assert!(ink >= 16, "curated ink {ink} belongs to the pane program");
        }
    }

    /// The picker name reads off the parts.
    #[test]
    fn the_name_comes_from_the_parts() {
        assert_eq!(sample().name(), "Mohawk Pink");
        let mut bare = sample();
        bare.hair = "none".into();
        assert_eq!(bare.name(), "Bare");
    }

    /// Every shape stays on the 16 by 12 grid.
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
            assert!(0 <= x0 && x0 < x1 && x1 <= GRID_W, "x out of grid");
            assert!(0 <= y0 && y0 < y1 && y1 <= GRID_H, "y out of grid");
        }
    }

    /// Art is always five rows of sixteen cells, and every ink on it comes
    /// from the curated sets, whatever the picker dealt.
    #[test]
    fn art_cells_are_fixed_size_and_curated_inks() {
        for hair in HAIR_STYLES {
            for ink in HAIR_INKS {
                for eyes in EYE_STYLES {
                    let pet = Avatar {
                        body: BODY.into(),
                        hair: hair.into(),
                        eyes: eyes.into(),
                        hair_ink: ink,
                        body_ink: BODY_INK,
                    };
                    let art = art_cells(&pet);
                    assert_eq!(art.len(), ART_ROWS);
                    let mut filled = 0;
                    for row in &art {
                        assert_eq!(row.len(), ART_COLS);
                        for (top, bottom) in row {
                            for cell in [top, bottom].into_iter().flatten() {
                                filled += 1;
                                assert!(
                                    HAIR_INKS.contains(cell)
                                        || *cell == BODY_INK
                                        || *cell == EYE_CREAM
                                        || *cell == EYE_INK,
                                    "ink {cell} escaped the curated sets"
                                );
                            }
                        }
                    }
                    assert!(filled > 0, "{hair}/{eyes} paints nothing");
                }
            }
        }
    }

    /// The sample pet wears its mohawk on top and its feet at the bottom.
    #[test]
    fn the_sample_pet_reads_top_to_bottom() {
        let art = art_cells(&sample());
        assert!(art[0].iter().any(|(top, _)| *top == Some(212)));
        assert!(art[4].iter().any(|cell| cell.0 == Some(BODY_INK)));
    }
}
