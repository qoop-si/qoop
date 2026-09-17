//! What the chrome looks like, in one place.
//!
//! Before this module the interface had three escapes in it: dim for the whole tab bar,
//! reverse for the selected row and the status line, and the eight basic colours for a
//! state. That is what a screenshot of it showed: a flat grey column, two heavy white
//! blocks, and no colour anywhere until an agent happened to be in a state, which on a
//! fresh session is never. Everything looked equally important, which is the same as
//! nothing being important.
//!
//! So the chrome names its roles here and the painters ask for a role. The values are the
//! 256 colour palette, which is what every terminal this runs in has had for twenty years
//! and what the pane itself already emits for the agent's own output. The one thing not
//! spent is the first sixteen entries: those are the ones a person has themed, so they
//! belong to the program in the pane and never to the furniture around it.

/// A piece of the interface, named for what it is rather than for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A panel's own title.
    Title,
    /// An ordinary row.
    Text,
    /// A row that has the keyboard.
    Focused,
    /// Secondary detail on a row: the program, the branch, how long it has waited.
    Detail,
    /// A count, a fold line, a hint. Present, and never read unless looked for.
    Faint,
    /// The rules between the panels and beside the pane.
    Rule,
    /// The product's own mark. One thing on the screen wears it, and nothing else does:
    /// an accent spent twice is an accent that points at nothing.
    Brand,
    /// An agent's state.
    Working,
    Blocked,
    Done,
    Failed,
    Idle,
    /// A terminal nothing has said anything about yet.
    Unobserved,
    /// Which space a thing belongs to, as a colour rather than as a word.
    ///
    /// Identity and not state. The states say what is happening and there are five of
    /// them; this says where, and there are as many as a person has projects. A row in the
    /// agents panel carries its space's colour on the space's own name, so which project an
    /// agent belongs to is something the eye gets for free instead of a word it has to read
    /// on every one of nine rows.
    Space(usize),
}

/// The colour a space is given, from its own number, so it keeps it for as long as it
/// exists and two spaces never share one until there are more spaces than colours.
///
/// Kept clear of the state inks below. A person has to be able to tell "this is a space" apart
/// from "this is blocked" at a glance, and they are told apart twice: by position, since the
/// space colour is on the name and the state colour is on the word flush right, and by hue.
pub fn space_of(id: i64) -> Role {
    Role::Space(id.unsigned_abs() as usize % SPACE_INKS.len())
}

/// The identity palette. Six, because a seventh distinct hue that is neither a state nor
/// the greys the chrome is built from does not exist in a 256 colour terminal, and a wheel
/// that comes round is better than one that lies.
const SPACE_INKS: [u8; 6] = [111, 176, 215, 150, 117, 181];

/// The colour a state earns, by name, for a caller that has the daemon's string.
///
/// Deliberately few and deliberately warm only where it matters: a person scanning this
/// column is looking for the one row that needs them, and a column where every row is a
/// different colour is a column with no signal in it.
pub fn of(state: &str) -> Role {
    match state {
        "working" | "starting" => Role::Working,
        "blocked" => Role::Blocked,
        "done" => Role::Done,
        "failed" => Role::Failed,
        "idle" => Role::Idle,
        _ => Role::Unobserved,
    }
}

/// The 256 colour entry a role is painted in.
///
/// The greys and the accent are the brand's own tokens, matched to the nearest entry in
/// the 256 colour palette: text `#e5e5e5` is 254, muted `#666666` is 241, and the accent
/// `#FF6600` is 202, which is `#ff5f00` and the closest there is. The states keep their
/// own hues, because a state is not brand.
///
/// The rule is the one deliberate departure. The brand border is `#1a1a1a`, which is a
/// line you can only see against the brand's own black; a terminal's background is
/// whatever the person set, and at that value the sections of the left column disappear on
/// most of them. 237 is the same intent, a line that is there and never loud, at a value
/// that survives not knowing what is behind it.
const fn ink(role: Role) -> u8 {
    match role {
        Role::Title => 244,
        Role::Text => 254,
        Role::Focused => 255,
        Role::Detail => 245,
        Role::Faint => 241,
        Role::Rule => 237,
        Role::Brand => 202,
        Role::Working => 80,
        Role::Blocked => 179,
        Role::Done => 108,
        Role::Failed => 174,
        Role::Idle => 245,
        Role::Unobserved => 240,
        Role::Space(n) => SPACE_INKS[n % SPACE_INKS.len()],
    }
}

/// The escape that starts painting in a role, with nothing else carried over.
///
/// Every one of these resets first. The chrome is painted around a pane that a program
/// left in whatever colour and attribute it liked, and a run that does not reset inherits
/// it: measured against a real agent, the search box came out in the agent's own dim grey
/// and read as having dissolved into the output behind it.
pub fn fg(role: Role) -> String {
    format!("\x1b[0;38;5;{}m", ink(role))
}

/// The escape for a row that has the keyboard: its own ink on a raised background.
///
/// A background and not reverse video. Reverse paints the full width of the column in the
/// foreground colour, which is a white block an eye lands on before anything else on the
/// screen, and the selected row is not the most urgent thing on it.
pub fn selected() -> String {
    format!("\x1b[0;48;5;{};38;5;{}m", SELECTED_BG, ink(Role::Focused))
}

/// A selected row that also has a state to say, so the state keeps its colour on the
/// raised background.
pub fn selected_in(role: Role) -> String {
    format!("\x1b[0;48;5;{};38;5;{}m", SELECTED_BG, ink(role))
}

/// The bar at the bottom: raised, like the selected row, and never reverse.
pub fn status() -> String {
    format!("\x1b[0;48;5;{};38;5;{}m", STATUS_BG, ink(Role::Detail))
}

/// A key name inside the status bar, which is the part a person is looking for.
pub fn status_key() -> String {
    format!("\x1b[0;48;5;{};38;5;{}m", STATUS_BG, ink(Role::Focused))
}

/// Something in the status bar that wants a person, which is the one thing there allowed
/// to be loud.
pub fn status_in(role: Role) -> String {
    format!("\x1b[0;48;5;{};38;5;{}m", STATUS_BG, ink(role))
}

/// The tab with the keyboard: a light pill with dark text, which is the one high contrast
/// thing on the bar and therefore findable without reading it.
pub fn tab_on() -> String {
    format!("\x1b[0;1;48;5;{};38;5;{}m", TAB_ON_BG, TAB_ON_FG)
}

/// Every other tab.
pub fn tab_off() -> String {
    fg(Role::Detail)
}

/// Back to the terminal's own colours.
pub const RESET: &str = "\x1b[0m";

/// The raised background of a row that has the keyboard.
const SELECTED_BG: u8 = 236;
/// The raised background of the status bar, a shade below the selected row so the two do
/// not read as the same thing.
const STATUS_BG: u8 = 233;
/// The focused tab's pill.
const TAB_ON_BG: u8 = 254;
const TAB_ON_FG: u8 = 233;

#[cfg(test)]
mod tests {
    use super::*;

    /// The first sixteen entries are the ones a person has themed, and they belong to the
    /// program in the pane. Furniture that used them would change colour with somebody's
    /// theme and stop being furniture.
    #[test]
    fn no_role_spends_a_colour_the_user_has_themed() {
        for role in [
            Role::Title,
            Role::Text,
            Role::Focused,
            Role::Detail,
            Role::Faint,
            Role::Rule,
            Role::Working,
            Role::Blocked,
            Role::Done,
            Role::Failed,
            Role::Idle,
            Role::Unobserved,
        ]
        .into_iter()
        .chain((0..SPACE_INKS.len()).map(Role::Space))
        {
            assert!(ink(role) >= 16, "{role:?} uses a themed colour");
        }
        for bg in [SELECTED_BG, STATUS_BG, TAB_ON_BG, TAB_ON_FG] {
            assert!(bg >= 16, "a background uses a themed colour");
        }
    }

    /// Every escape this module hands out resets first, because all of them are painted
    /// over a pane left in somebody else's colour.
    #[test]
    fn every_escape_resets_before_it_paints() {
        let mut escapes = vec![
            selected(),
            selected_in(Role::Blocked),
            status(),
            status_key(),
            status_in(Role::Blocked),
            tab_on(),
            tab_off(),
        ];
        escapes.extend([Role::Title, Role::Text, Role::Rule].map(fg));
        for escape in escapes {
            assert!(escape.starts_with("\x1b[0;"), "{escape:?} does not reset");
            assert!(escape.ends_with('m'), "{escape:?} is not a whole escape");
        }
    }

    /// A state a person has to act on has to look different from one they do not, or the
    /// column is decoration.
    #[test]
    fn the_states_that_want_a_person_are_not_the_colour_of_the_ones_that_do_not() {
        let calm = [ink(Role::Idle), ink(Role::Unobserved), ink(Role::Detail)];
        for loud in [Role::Blocked, Role::Failed, Role::Working, Role::Done] {
            assert!(!calm.contains(&ink(loud)), "{loud:?} is painted as calm");
        }
        assert_ne!(ink(Role::Blocked), ink(Role::Failed));
        assert_ne!(ink(Role::Working), ink(Role::Done));
    }

    /// A space colour that is also a state colour would have a person reading a project as
    /// a state, which is the one confusion this palette exists to prevent.
    #[test]
    fn a_space_is_never_the_colour_of_a_state_or_of_the_chrome() {
        let taken = [
            Role::Working,
            Role::Blocked,
            Role::Done,
            Role::Failed,
            Role::Idle,
            Role::Unobserved,
            Role::Title,
            Role::Text,
            Role::Focused,
            Role::Detail,
            Role::Faint,
            Role::Rule,
        ]
        .map(ink);
        for n in 0..SPACE_INKS.len() {
            assert!(
                !taken.contains(&ink(Role::Space(n))),
                "space {n} wears a colour that already means something"
            );
        }
    }

    /// A space keeps its colour for as long as it exists, and neighbours differ.
    #[test]
    fn a_space_keeps_one_colour_and_its_neighbours_are_not_it() {
        assert_eq!(space_of(3), space_of(3));
        for id in 1..6i64 {
            assert_ne!(space_of(id), space_of(id + 1), "spaces {id} and {}", id + 1);
        }
    }
}
