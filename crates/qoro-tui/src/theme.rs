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
    /// The product's own mark: the logo orange, one steady wearer on the screen.
    /// An accent spent twice is an accent that points at nothing. (The ember
    /// wheel starts on it transiently while it breathes; that is motion, not a mark.)
    Brand,
    /// An agent's state.
    Working,
    Blocked,
    Done,
    Failed,
    Idle,
    /// The working spinner's head, one step of the brand's own ambers.
    ///
    /// The sidebar steps these five times per loop while its glyph cycles four
    /// circle frames, so the (glyph, step) combination repeats every twenty ticks.
    /// Amber and not violet-pink on purpose: the wheel starts on the brand's own
    /// orange and breathes outward through gold to tan, and aliveness wants its
    /// own channel rather than a louder state colour. 208 is skipped: it belongs
    /// to blocked now, so the wheel steps 216 instead.
    Ember(usize),
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
///
/// Anchored in the product palette and ordered for maximum distance: every
/// neighbour pair round the wheel and back sits at least 89 dE apart, against
/// a 10-12 guideline for glanceable lists, and the closest two in the whole set
/// are 37 apart. Measured in CIELAB over the 256 entries, with an L* floor near
/// 55 and contrast 5+:1 on dark grounds (the old 27 blue failed both, at L* 46
/// and 3.6:1 against STATUS_BG 233; the new floor is 5.3:1 on the same ground).
/// The set leans blue-cyan (33, 51, 117) with lightness doing the telling apart;
/// the thinnest cross-exclusion margin is the yellow 226 against the ember gold
/// 220 (23 apart), carried by position plus motion. Order round the wheel:
/// violet 135, cyan 51, azure 33, lime light 155, sky 117, yellow 226.
/// 33 is the brighter step of the `0044D7` blue family (the anchor 26 is too dim);
/// 226 carries the bonus `F7F245` yellow at a safe distance from lime. The two
/// palette lime-yellows cannot both serve: `C7F245` and `F7F245` land 16 apart
/// in 256, so the wheel spends 155 and 226 instead, same families, honest distance.
/// A space ink always sits on the space name on the row below while a state sits
/// on the first-row dot, so working-adjacent blues never compete for one slot.
const SPACE_INKS: [u8; 6] = [135, 51, 33, 155, 117, 226];

/// The spinner's amber wheel, from the brand accent out toward tan.
///
/// `FD4B03` is 202, which is also [`Role::Brand`]'s ink: the spinner starts on the
/// product's own mark and breathes outward, and a blocked row wears steady orange
/// beside it. 208 would be the next amber step but belongs to blocked, so the wheel
/// steps 214, 216, 220 and 222 instead.
const EMBER_INKS: [u8; 5] = [202, 214, 216, 220, 222];

/// The colour a state earns, by name, for a caller that has the daemon's string.
///
/// Loud on black on purpose: the column is scanned for the one row that needs
/// a person. Blocked is warm orange (208, `FF7700`); failed is vivid magenta
/// (201), the palette's loudest hue outside the warm alarm pair, and always a
/// diamond where blocked is a circle (see the sidebar). Working is bright sky
/// (39, `00BBFF`) so it never reads as done's green (35, `00BB54`). Idle and
/// unobserved stay in the greys, because a colour on every row is a column
/// with no signal in it.
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
/// the 256 colour palette: text `FCF7F1` is 231 (entry 15 is the same white but
/// belongs to the person, so 231 is the nearest legal one), focused stays 255,
/// muted `#666666` is 241, and the accent is the logo orange: `#FF6600` is 202,
/// and the palette's `FD4B03` lands on the same 202, so the mark renders exactly
/// as it always has. The states keep their own hues, because a state is not
/// brand: working `00BBFF` is 39, blocked `FF7700` is 208, done `00BB54` is 35,
/// failed is vivid magenta 201, the palette's loudest hue outside the warm pair.
///
/// The violet anchor `5A00EB` (56) is unspent: violet has no correct steady seat
/// while the mark is orange, and the violet family still shows in the space
/// wheel's 135 step and the tab's pink 212.
///
/// The rule is the one deliberate departure. The brand border is `#1a1a1a`, which is a
/// line you can only see against the brand's own black; a terminal's background is
/// whatever the person set, and at that value the sections of the left column disappear on
/// most of them. 237 is the same intent, a line that is there and never loud, at a value
/// that survives not knowing what is behind it.
const fn ink(role: Role) -> u8 {
    match role {
        Role::Title => 244,
        Role::Text => 231,
        Role::Focused => 255,
        Role::Detail => 245,
        Role::Faint => 241,
        Role::Rule => 237,
        Role::Brand => 202,
        Role::Working => 39,
        Role::Blocked => 208,
        Role::Done => 35,
        Role::Failed => 201,
        Role::Idle => 242,
        Role::Unobserved => 238,
        Role::Ember(n) => EMBER_INKS[n % EMBER_INKS.len()],
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
///
/// The foreground is never white either: a stateless row keeps the quiet detail ink on
/// the raised background, and a row with a state keeps that state's ink (see
/// [`selected_in`]). White text would make the selected row the brightest thing on the
/// screen, which is exactly what the background was chosen not to do.
pub fn selected() -> String {
    format!("\x1b[0;48;5;{};38;5;{}m", SELECTED_BG, ink(Role::Detail))
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

/// The tab with the keyboard: bold pink on the raised background.
///
/// Findable without reading it, and never a white pill: a white block is the brightest
/// thing on the bar, and the selected tab is not the most urgent thing on it.
/// Pink (212, `FF8FD9`) stands clear of the warm alarm pair and every state;
/// amber would read as alarm, so the tab left amber behind.
pub fn tab_on() -> String {
    format!("\x1b[0;1;48;5;{};38;5;{}m", SELECTED_BG, TAB_ON_FG)
}

/// Every other tab: the focused space's own colour, so the bar reads as that
/// space's bar. The bar shows one space's terminals, so one colour lands on all
/// of them and never turns to confetti. The focused tab keeps the pink pill:
/// focus outranks belonging.
pub fn tab_in(role: Role) -> String {
    format!("\x1b[0;38;5;{}m", ink(role))
}

/// Back to the terminal's own colours.
pub const RESET: &str = "\x1b[0m";

/// The raised background of a row that has the keyboard.
const SELECTED_BG: u8 = 236;
/// The raised background of the status bar, a shade below the selected row so the two do
/// not read as the same thing.
const STATUS_BG: u8 = 233;
/// The focused tab's ink: pink, on the selected row's background. Worn exactly
/// once: neither pool holds 212, and no state does. White lives nowhere
/// on the selected chrome: the selected row wears detail ink and a state row wears its
/// state's ink (see [`selected`] and [`selected_in`]).
const TAB_ON_FG: u8 = 212;

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
        .chain((0..EMBER_INKS.len()).map(Role::Ember))
        {
            assert!(ink(role) >= 16, "{role:?} uses a themed colour");
        }
        for bg in [SELECTED_BG, STATUS_BG] {
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
            tab_in(Role::Blocked),
        ];
        escapes.extend([Role::Title, Role::Text, Role::Rule].map(fg));
        for escape in escapes {
            assert!(escape.starts_with("\x1b[0;"), "{escape:?} does not reset");
            assert!(escape.ends_with('m'), "{escape:?} is not a whole escape");
        }
    }

    /// The selected chrome is never white: neither the selected row nor the
    /// selected tab may paint the brightest thing on the screen. 231 counts as
    /// white here too: it is the text ink, and text never leads selection.
    #[test]
    fn nothing_selected_is_white() {
        for escape in [selected(), tab_on()] {
            assert!(
                !escape.contains("38;5;255")
                    && !escape.contains("38;5;254")
                    && !escape.contains("38;5;231"),
                "white leaked into the selected chrome: {escape:?}"
            );
        }
    }

    /// Ordinary text and keyboard focus are two different whites: 231 is the
    /// nearest legal `FCF7F1` (entry 15 is the same white but user-themed) and
    /// 255 already means focused. A stateless row keeps quiet detail ink on the
    /// raised background either way; the background, not the tint, says focus.
    /// 231 reads a touch brighter than 255 in isolation, but the two never
    /// compete without the raised background between them, which is the actual
    /// focus signal on dark and light grounds alike.
    #[test]
    fn text_and_focused_are_not_the_same_white() {
        assert_ne!(ink(Role::Text), ink(Role::Focused));
        assert_eq!(ink(Role::Text), 231);
        assert_eq!(ink(Role::Focused), 255);
    }

    /// A state a person has to act on has to look different from one they do not, or the
    /// column is decoration.
    #[test]
    fn the_states_that_want_a_person_are_not_the_colour_of_the_ones_that_do_not() {
        let calm = [ink(Role::Idle), ink(Role::Unobserved), ink(Role::Detail)];
        for loud in [Role::Blocked, Role::Failed, Role::Working, Role::Done] {
            assert!(!calm.contains(&ink(loud)), "{loud:?} is painted as calm");
        }
        // Far apart by palette and pinned: failed is vivid magenta against the
        // warm alarm pair, and the dot's shape carries with the hue (see the
        // sidebar's failed diamond).
        assert_ne!(ink(Role::Blocked), ink(Role::Failed));
        assert_ne!(ink(Role::Working), ink(Role::Done));
    }

    /// The mark is not a state and the tab is not an identity: brand must differ
    /// from every state ink (the old suite passed even with Brand == Blocked),
    /// and the pools below assert the rest.
    #[test]
    fn brand_is_not_the_colour_of_any_state() {
        let states = [
            Role::Working,
            Role::Blocked,
            Role::Done,
            Role::Failed,
            Role::Idle,
            Role::Unobserved,
        ]
        .map(ink);
        assert!(
            !states.contains(&ink(Role::Brand)),
            "brand wears a state colour"
        );
    }

    /// A space colour that is also a state colour would have a person reading a project as
    /// a state, which is the one confusion this palette exists to prevent.
    /// The mark and the selected tab count as taken too: an identity must never
    /// become the brand or the tab.
    #[test]
    fn a_space_is_never_the_colour_of_a_state_or_of_the_chrome() {
        let mut taken = [
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
            Role::Brand,
        ]
        .map(ink)
        .to_vec();
        taken.push(TAB_ON_FG);
        for n in 0..SPACE_INKS.len() {
            assert!(
                !taken.contains(&ink(Role::Space(n))),
                "space {n} wears a colour that already means something"
            );
        }
    }

    /// Identity and aliveness share no ink: a spinner step the colour of a space
    /// would read as a project, and a space the colour of a spinner step as motion.
    /// The selected tab is neither pool's nor any state's: it must not read as
    /// a project, a spinner step, or a state from the bar.
    #[test]
    fn a_space_is_never_an_ember_step_and_the_tab_is_neither() {
        let ember: Vec<u8> = (0..EMBER_INKS.len()).map(|n| ink(Role::Ember(n))).collect();
        for n in 0..SPACE_INKS.len() {
            assert!(
                !ember.contains(&ink(Role::Space(n))),
                "space {n} wears an ember step"
            );
        }
        for (name, pool) in [
            ("space", SPACE_INKS.to_vec()),
            ("ember", EMBER_INKS.to_vec()),
        ] {
            assert!(
                !pool.contains(&TAB_ON_FG),
                "the selected tab wears a {name} pool colour"
            );
        }
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
            Role::Brand,
        ]
        .map(ink);
        assert!(
            !taken.contains(&TAB_ON_FG),
            "the selected tab wears a taken colour"
        );
    }

    /// A space keeps its colour for as long as it exists, and neighbours differ.
    #[test]
    fn a_space_keeps_one_colour_and_its_neighbours_are_not_it() {
        assert_eq!(space_of(3), space_of(3));
        for id in 1..6i64 {
            assert_ne!(space_of(id), space_of(id + 1), "spaces {id} and {}", id + 1);
        }
    }

    /// Every space wears its own family: six slots, six families, so no two
    /// projects ever share a hue family however the wheel turns. (Measured:
    /// the closest pair in the set is 37 dE apart, neighbours 89+.)
    #[test]
    fn neighbouring_spaces_are_never_of_one_family() {
        fn family(ink: u8) -> &'static str {
            match ink {
                135 => "violet",
                51 => "cyan",
                33 => "azure",
                155 => "lime",
                117 => "sky",
                226 => "yellow",
                _ => "other",
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for n in 0..SPACE_INKS.len() {
            let here = family(ink(Role::Space(n)));
            assert!(seen.insert(here), "family {here} spent twice in the wheel");
            let next = family(ink(Role::Space(n + 1)));
            assert_ne!(here, next, "spaces {n} and {} share a family", n + 1);
        }
    }

    /// The ember wheel is five distinct steps that come back round, so the spinner
    /// breathes out from the brand orange toward tan and back rather than jumping
    /// between two hues.
    #[test]
    fn the_ember_wheel_is_five_distinct_steps_that_wrap() {
        let mut seen = std::collections::BTreeSet::new();
        for n in 0..EMBER_INKS.len() {
            assert!(seen.insert(ink(Role::Ember(n))), "ember {n} repeats a step");
        }
        assert_eq!(ink(Role::Ember(0)), ink(Role::Brand));
        assert_eq!(
            ink(Role::Ember(EMBER_INKS.len())),
            ink(Role::Ember(0)),
            "the wheel does not come back round"
        );
    }

    /// An ember is never a state colour: the spinner's aliveness must not read as
    /// blocked, failed, working or done. Nor as chrome: a spinner step in a panel
    /// grey would read as furniture instead of motion.
    #[test]
    fn an_ember_is_never_the_colour_of_a_state() {
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
        .map(ink)
        .to_vec();
        for n in 0..EMBER_INKS.len() {
            // Ember(0) is Brand's own ink by design (see above); the rest of the
            // wheel must belong to nobody else.
            if n == 0 {
                continue;
            }
            assert!(
                !taken.contains(&ink(Role::Ember(n))),
                "ember {n} wears a taken colour"
            );
        }
    }
}
