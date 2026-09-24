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
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .ok()
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("qoro").join("avatar.json")
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
}
