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
    /// An accent spent twice is an accent that points at nothing.
    Brand,
    /// The brand one step quieter: a keep awake cup that is full because its whole space is
    /// kept awake rather than this pane.
    BrandCalm,
    /// An agent saved for later: gold, apart from every state's ink and from the brand's
    /// ember (the owner's pick, 2026-10-20).
    Saved,
    /// An agent's state.
    Working,
    Blocked,
    Done,
    Failed,
    Idle,
    /// An idle agent with work still running in the background: not done, not resting.
    /// Derived by the sidebar (the daemon's state word stays `idle`), never sent, so `of`
    /// never returns it. It wears the working ink: it is alive, and the grey of rest would
    /// say otherwise. The picture tells the two apart.
    Background,
    /// A tab the idle reaper slept: the process is gone, the marker stays.
    ///
    /// Calm-bottom, below idle, never attention: the only thing it has to
    /// say is "this one is not running any more". A settled state with a
    /// hue, like done: idle stays the uncoloured background.
    Sleeping,
    /// One step of the sweep that crosses a working agent's activity: the grey ramp
    /// from 244 up to white at 255, level 0 to 11. Grey and not a hue on purpose: the
    /// sweep says "alive", and a colour would say a state it is not. Its darkest step is
    /// still brighter than the quiet row it crosses, so the band always reads.
    Shimmer(u8),
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
    /// The same space's ink one step quieter, worn by the title of an agent in it that is
    /// calm (idle, done, asleep, not yet seen). The space still reads by hue, and the rows
    /// that want a person stand out of it without a second colour.
    SpaceCalm(usize),
    /// The picked avatar's own two inks, worn only on the foot of the column.
    ///
    /// The payload is a 256-palette entry the avatar file vouched for: loading
    /// clamps to the curated product tokens, all at or above 16, so these can
    /// never spend a colour the person themed. Two roles and not one because
    /// the avatar is two inks, hair beside body, and one run cannot wear both.
    AvatarHair(u8),
    /// The body's ink; see [`Role::AvatarHair`].
    AvatarBody(u8),
    /// The brand, working, failed, faint and idle inks for words a person reads: each at
    /// least 4.5 to 1 on the page, the status bar and the selected row of its table, where
    /// the mark inks above are held only to 3 to 1 as glyphs. Painted right of the left
    /// column only: the column is frozen and keeps the mark inks (`left-sidebar.md`).
    BrandText,
    WorkingText,
    FailedText,
    FaintText,
    IdleText,
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

/// The quieter step of [`space_of`], for a calm agent's title in that space.
pub fn space_calm_of(id: i64) -> Role {
    Role::SpaceCalm(id.unsigned_abs() as usize % SPACE_CALM_INKS.len())
}

/// The colour a space wears: the one a person picked for it, when its row in `spaces`
/// names one of the six, else [`space_of`]. A space not in the list, or an ink past the
/// six (a newer daemon's, say), is auto rather than a guess.
///
/// Read from the rows the caller holds rather than from a table kept here, so two
/// machines whose space ids collide never share a pick, and a test cannot race another.
pub fn space_in(spaces: &[qoop_runtime::control::WorkspaceRow], id: i64) -> Role {
    picked(spaces, id).map_or_else(|| space_of(id), Role::Space)
}

/// The quieter step of [`space_in`], for a calm agent's title in that space.
pub fn space_calm_in(spaces: &[qoop_runtime::control::WorkspaceRow], id: i64) -> Role {
    picked(spaces, id).map_or_else(|| space_calm_of(id), Role::SpaceCalm)
}

/// The ink a remote machine wears for how it is reached: done's green while connected,
/// blocked's orange while it connects or does not answer, faint while it is let go.
pub fn link_ink(link: qoop_runtime::control::LinkState) -> Role {
    use qoop_runtime::control::LinkState;
    match link {
        LinkState::Up => Role::Done,
        LinkState::Parked => Role::Faint,
        LinkState::Down => Role::Blocked,
    }
}

/// The ink picked for space `id`, if one of the six was.
fn picked(spaces: &[qoop_runtime::control::WorkspaceRow], id: i64) -> Option<usize> {
    spaces
        .iter()
        .find(|row| row.id == id)
        .and_then(|row| row.ink)
        .map(usize::from)
        .filter(|ink| *ink < SPACE_INKS.len())
}

/// The identity palette. Six, because a seventh distinct hue that is neither a state nor
/// the greys the chrome is built from does not exist in a 256 colour terminal, and a wheel
/// that comes round is better than one that lies.
///
/// Drawn only from the product palette's families that no state spends: violet `5A00EB`,
/// lime `C7F245`, pink `FF8FD9`, yellow `F7F245`, and lighter steps of sky `00BBFF`, each at a
/// step that reads on a dark ground (violet 177, lime 191, cyan 87, pink 218, sky 117,
/// yellow 228). Ordered so the two closest pairs (cyan and sky, 32 dE; lime and yellow, 23)
/// are never neighbours on the wheel. The nearest a space comes to a state is violet to
/// asleep (24) and sky to working (27): a space ink sits on the name and on its agents' titles,
/// a state on the agent's mark, so they never share a cell.
const SPACE_INKS: [u8; 6] = [177, 191, 87, 218, 117, 228];

/// How many colours a space can be given.
pub const SPACE_INK_COUNT: usize = SPACE_INKS.len();

/// The wheel one step quieter, slot for slot: each family at a lower lightness and
/// saturation (violet 134, lime 107, cyan 73, pink 175, sky 67, yellow 143), every one still
/// at least 4.5 to 1 on black because a title is read, not found. Sky steps down to the
/// muted 67 rather than the brighter 74, which sits too close to working's electric sky.
const SPACE_CALM_INKS: [u8; 6] = [134, 107, 73, 175, 67, 143];

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
        "sleeping" => Role::Sleeping,
        _ => Role::Unobserved,
    }
}

/// The 256 colour entry a role is painted in.
///
/// The greys and the accent are the brand's own tokens, matched to the nearest entry in
/// the 256 colour palette: text `FCF7F1` is 231 (entry 15 is the same white but
/// belongs to the person, so 231 is the nearest legal one), focused stays 255,
/// muted `#666666` would be 241 but reads at 3.4:1 on black, so the faint text is one
/// step up, 243 (4.6:1), and the accent is the logo orange: `#FF6600` is 202,
/// and the palette's `FD4B03` lands on the same 202, so the mark renders exactly
/// as it always has. The states keep their own hues, because a state is not
/// brand: working `00BBFF` is 39, blocked `FF7700` is 208, done `00BB54` is 35,
/// failed is vivid magenta 201, the palette's loudest hue outside the warm pair.
///
/// Sleeping wears 140 (`AF87D7`), a muted mauve: measured L* 63 against the
/// L* 55 floor, 7.3:1 on black (inside the states' 4 to 8.7 band), and 42 dE
/// from the closest state ink, the working sky. The violet anchor `5A00EB`
/// (56) stays unspent: at 2.5:1 it fails the contrast the job needs, and a
/// dimmer violet would read as chrome grey rather than as a state.
///
/// The rule is the one deliberate departure. The brand border is `#1a1a1a`, which is a
/// line you can only see against the brand's own black; a terminal's background is
/// whatever the person set, and at that value the sections of the left column disappear on
/// most of them. 237 is the same intent, a line that is there and never loud, at a value
/// that survives not knowing what is behind it.
const fn dark_ink(role: Role) -> u8 {
    match role {
        Role::Title => 244,
        Role::Text => 231,
        Role::Focused => 255,
        Role::Detail => 245,
        Role::Faint => 243,
        Role::Rule => 237,
        Role::Brand => 202,
        Role::BrandCalm => 130,
        Role::Saved => 220,
        Role::Working => 39,
        Role::Blocked => 208,
        Role::Done => 35,
        Role::Failed => 201,
        Role::Idle => 242,
        Role::Background => dark_ink(Role::Working),
        Role::Sleeping => 140,
        Role::Unobserved => 238,
        Role::AvatarHair(ink) => ink,
        Role::AvatarBody(ink) => ink,
        // The brand and failed one step lighter, apart from blocked's 208 and the tab's 212:
        // 202 and 201 are 4.3 and 4.2 to 1 on the selected row.
        Role::BrandText => 209,
        Role::WorkingText => dark_ink(Role::Working),
        Role::FailedText => 207,
        Role::FaintText | Role::IdleText => 247,
        Role::Shimmer(level) => 244 + if level > 11 { 11 } else { level },
        Role::Space(n) => SPACE_INKS[n % SPACE_INKS.len()],
        Role::SpaceCalm(n) => SPACE_CALM_INKS[n % SPACE_CALM_INKS.len()],
    }
}

/// The escape that starts painting in a role, with nothing else carried over.
///
/// Every one of these resets first. The chrome is painted around a pane that a program
/// left in whatever colour and attribute it liked, and a run that does not reset inherits
/// it: measured against a real agent, the search box came out in the agent's own dim grey
/// and read as having dissolved into the output behind it.
pub fn fg(role: Role) -> String {
    fg_for(role, crate::colorlevel::level())
}

/// The escape for a role at an explicit colour level. Pure, so the monochrome
/// path is tested without touching process globals.
///
/// In monochrome there is no colour to spend: bold carries focus (titles and
/// the focused row), everything else is plain. State still reads because the
/// sidebar pairs every state with a shape and a label, never colour alone.
pub fn fg_for(role: Role, level: crate::colorlevel::Level) -> String {
    match level {
        crate::colorlevel::Level::Mono => match role {
            Role::Title | Role::Focused => "\x1b[0;1m".to_string(),
            _ => RESET.to_string(),
        },
        _ => format!("\x1b[0;38;5;{}m", ink(role)),
    }
}

/// A button in a box: the one that answers wears the brand, the others the raised
/// background. In monochrome the answer is reversed and the others bold.
pub fn button(primary: bool) -> String {
    match crate::colorlevel::level() {
        crate::colorlevel::Level::Mono if primary => "\x1b[0;7m".to_string(),
        crate::colorlevel::Level::Mono => "\x1b[0;1m".to_string(),
        _ if primary => format!("\x1b[0;1;48;5;{};38;5;16m", ink(Role::Brand)),
        _ => format!("\x1b[0;48;5;{};38;5;{}m", raised().0, ink(Role::Text)),
    }
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
    selected_for(crate::colorlevel::level())
}

/// The selected row escape at an explicit colour level. Pure, see [`fg_for`].
/// In monochrome the row is bold with no raised background.
pub fn selected_for(level: crate::colorlevel::Level) -> String {
    match level {
        crate::colorlevel::Level::Mono => "\x1b[0;1m".to_string(),
        _ => format!("\x1b[0;48;5;{};38;5;{}m", raised().0, ink(Role::Detail)),
    }
}

/// A selected row that also has a state to say, so the state keeps its colour on the
/// raised background.
pub fn selected_in(role: Role) -> String {
    selected_in_for(role, crate::colorlevel::level())
}

/// A selected row that also has a state to say, at an explicit colour level.
/// Pure, see [`fg_for`]. In monochrome the state is carried by the row's
/// shape and label, so the escape matches [`selected_for`].
pub fn selected_in_for(role: Role, level: crate::colorlevel::Level) -> String {
    let _ = role;
    match level {
        crate::colorlevel::Level::Mono => "\x1b[0;1m".to_string(),
        _ => format!("\x1b[0;48;5;{};38;5;{}m", raised().0, ink(role)),
    }
}

/// The bar at the bottom: raised, like the selected row, and never reverse.
pub fn status() -> String {
    status_for(crate::colorlevel::level())
}

/// The status bar escape at an explicit colour level. Pure, see [`fg_for`].
/// In monochrome the bar is plain; key names stay bold through
/// [`status_key_for`].
pub fn status_for(level: crate::colorlevel::Level) -> String {
    match level {
        crate::colorlevel::Level::Mono => RESET.to_string(),
        _ => format!("\x1b[0;48;5;{};38;5;{}m", raised().1, ink(Role::Detail)),
    }
}

/// A key name inside the status bar, which is the part a person is looking for.
pub fn status_key() -> String {
    status_key_for(crate::colorlevel::level())
}

/// A key name inside the status bar, at an explicit colour level. Pure, see
/// [`fg_for`]. Bold in both modes: in monochrome it is the only thing that
/// marks a key as findable.
pub fn status_key_for(level: crate::colorlevel::Level) -> String {
    match level {
        crate::colorlevel::Level::Mono => "\x1b[0;1m".to_string(),
        _ => format!("\x1b[0;48;5;{};38;5;{}m", raised().1, ink(Role::Focused)),
    }
}

/// A word filled with its role's ink, the dark of the page written on it: the inbox's chosen
/// tab. In monochrome it is reversed, the one fill there is.
pub fn filled(role: Role) -> String {
    filled_for(role, crate::colorlevel::level())
}

/// A filled word at an explicit colour level. Pure, see [`fg_for`].
pub fn filled_for(role: Role, level: crate::colorlevel::Level) -> String {
    match level {
        crate::colorlevel::Level::Mono => "\x1b[0;7m".to_string(),
        _ => format!(
            "\x1b[0;1;48;5;{};38;5;{}m",
            ink(role),
            if light() { 231 } else { 16 }
        ),
    }
}

/// Something in the status bar that wants a person, which is the one thing there allowed
/// to be loud.
pub fn status_in(role: Role) -> String {
    status_in_for(role, crate::colorlevel::level())
}

/// A loud status bar item at an explicit colour level. Pure, see [`fg_for`].
/// In monochrome loudness is bold, matching [`status_key_for`].
pub fn status_in_for(role: Role, level: crate::colorlevel::Level) -> String {
    let _ = role;
    match level {
        crate::colorlevel::Level::Mono => "\x1b[0;1m".to_string(),
        _ => format!("\x1b[0;48;5;{};38;5;{}m", raised().1, ink(role)),
    }
}

/// The name of the tab with the keyboard: bold pink.
///
/// Findable without reading it, and never white: white is the brightest thing on the bar,
/// and the selected tab is not the most urgent thing on it.
/// Pink (212, `FF8FD9`) stands clear of the warm alarm pair and every state;
/// amber would read as alarm, so the tab left amber behind.
pub fn tab_on() -> String {
    tab_on_for(crate::colorlevel::level())
}

/// The focused tab escape at an explicit colour level. Pure, see [`fg_for`].
/// Bold in monochrome: focus outranks belonging with or without colour.
pub fn tab_on_for(level: crate::colorlevel::Level) -> String {
    match level {
        crate::colorlevel::Level::Mono => "\x1b[0;1m".to_string(),
        _ => format!("\x1b[0;1;38;5;{}m", tab_on_fg()),
    }
}

/// Every other tab: the focused space's own colour, so the bar reads as that
/// space's bar. The bar shows one space's terminals, so one colour lands on all
/// of them and never turns to confetti. The focused tab keeps the pink pill:
/// focus outranks belonging.
pub fn tab_in(role: Role) -> String {
    tab_in_for(role, crate::colorlevel::level())
}

/// Any other tab at an explicit colour level. Pure, see [`fg_for`].
pub fn tab_in_for(role: Role, level: crate::colorlevel::Level) -> String {
    let _ = role;
    match level {
        crate::colorlevel::Level::Mono => RESET.to_string(),
        _ => format!("\x1b[0;38;5;{}m", ink(role)),
    }
}

/// A colour swatch in a space's settings: space ink `n` as the background, its number on
/// it in black on the dark palette's inks and white on the light palette's, which are the
/// darker ones.
pub fn swatch(n: usize) -> String {
    swatch_for(n, crate::colorlevel::level())
}

/// A swatch at an explicit colour level. Pure, see [`fg_for`]. In monochrome there is no
/// colour to show, and the number and the pick's brackets say it all.
pub fn swatch_for(n: usize, level: crate::colorlevel::Level) -> String {
    match level {
        crate::colorlevel::Level::Mono => RESET.to_string(),
        _ => format!(
            "\x1b[0;48;5;{};38;5;{}m",
            ink(Role::Space(n)),
            if light() { 231 } else { 16 }
        ),
    }
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

/// Whether the chrome is drawn for a light background.
///
/// One switch for the whole interface, read by every escape above. Set where the interface
/// decides which background it is on (the config, then what the terminal says), never by a
/// painter, so a frame is always drawn in one palette.
static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Draw for a light background, or a dark one. The frame that follows repaints every row
/// whose escapes changed, which is all of them when this did.
pub fn set_light(on: bool) {
    LIGHT.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the chrome is drawn for a light background.
pub fn light() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_LIGHT.with(std::cell::Cell::get) {
        return on;
    }
    LIGHT.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// A test's own palette, on its own thread: the switch above is the whole process's.
    pub(crate) static TEST_LIGHT: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// The background colour the terminal answered with, packed with a presence bit: zero is
/// not asked or no answer. Set beside [`set_light`], read by whatever must paint in it.
static GROUND: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Keep the terminal's background colour, or forget it.
pub fn set_ground(rgb: Option<[u8; 3]>) {
    let packed = rgb.map_or(0, |[r, g, b]| {
        1 << 24 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
    });
    GROUND.store(packed, std::sync::atomic::Ordering::Relaxed);
}

/// The terminal's own background as a truecolour foreground escape, when it said what that
/// is and truecolour reaches it: exact, so a cell painted in it reads as no paint at all.
/// Anything less exact would show as a patch, so then nothing.
pub fn ground_fg() -> Option<String> {
    let packed = GROUND.load(std::sync::atomic::Ordering::Relaxed);
    let exact = crate::colorlevel::level() == crate::colorlevel::Level::True;
    (packed != 0 && exact).then(|| {
        let [_, r, g, b] = packed.to_be_bytes();
        format!("\x1b[38;2;{r};{g};{b}m")
    })
}

pub use qoop_domain::terminal::is_light_background;

/// The first guess, from `COLORFGBG` (`fg;bg`, sometimes `fg;default;bg`), which some
/// terminals set and which costs no round trip: a background of 7 or 9 to 15 is a light
/// one, 0 to 6 or 8 a dark one, anything else says nothing.
///
/// The return is three states, and the distinction matters. `None` is unknown, not
/// dark: the caller keeps dark as the default (which is what a light Terminal.app
/// window gets until the device probe in `colorlevel` lands, documented there),
/// but an unknown must never be mistaken for a measured dark further down the line.
pub fn colorfgbg_is_light(value: &str) -> Option<bool> {
    match value.rsplit(';').next()?.trim().parse::<u8>().ok()? {
        7 | 9..=15 => Some(true),
        0..=6 | 8 => Some(false),
        _ => None,
    }
}

/// The entry a role is painted in, in the palette in effect.
fn ink(role: Role) -> u8 {
    if light() {
        light_ink(role)
    } else {
        dark_ink(role)
    }
}

/// The entry a role is painted in, for a screen drawn as cells (the chat view).
pub fn indexed(role: Role) -> u8 {
    ink(role)
}

/// The raised background of a block that stands off the pane, in the palette in effect.
pub fn raised_bg() -> u8 {
    raised().0
}

/// The raised backgrounds, selected row then status bar, in the palette in effect.
fn raised() -> (u8, u8) {
    if light() {
        (LIGHT_SELECTED_BG, LIGHT_STATUS_BG)
    } else {
        (SELECTED_BG, STATUS_BG)
    }
}

fn tab_on_fg() -> u8 {
    if light() { LIGHT_TAB_ON_FG } else { TAB_ON_FG }
}

/// The same roles for a light background.
///
/// Chosen by measuring each candidate in the 256 palette against white rather than by
/// darkening the dark table: text at least 4.5 to 1, a state, a space, a sweep step, the
/// mark and the tab at least 3 to 1 (they are glyphs a person finds, not sentences they
/// read), the rule exempt because a rule is only there to be barely seen. The dark table's
/// hue for each job is kept where the palette has a dark enough step of it: working a
/// blue, blocked a burnt orange, done a green, failed a magenta, sleeping a mauve.
const fn light_ink(role: Role) -> u8 {
    match role {
        Role::Title => 241,
        Role::Text => 235,
        Role::Focused => 16,
        Role::Detail => 240,
        Role::Faint => 243,
        Role::Rule => 250,
        Role::Brand => 202,
        Role::BrandCalm => 173,
        Role::Saved => 136,
        Role::Working => 32,
        Role::Blocked => 130,
        Role::Done => 28,
        Role::Failed => 164,
        Role::Idle => 242,
        Role::Background => light_ink(Role::Working),
        Role::Sleeping => 97,
        Role::Unobserved => 244,
        Role::AvatarHair(ink) => light_pet(ink),
        Role::AvatarBody(ink) => light_pet(ink),
        // Amber 94, not blocked's 130: a brand word must not read as a blocked state.
        Role::BrandText => 94,
        Role::WorkingText => 25,
        Role::FailedText => 127,
        Role::FaintText | Role::IdleText => 241,
        // On white the sweep darkens toward black rather than brightening toward white: the
        // quiet row is a light grey here, and emphasis on a light page is a darker ink.
        Role::Shimmer(level) => 244 - if level > 11 { 11 } else { level },
        Role::Space(n) => LIGHT_SPACE_INKS[n % LIGHT_SPACE_INKS.len()],
        Role::SpaceCalm(n) => LIGHT_SPACE_CALM_INKS[n % LIGHT_SPACE_CALM_INKS.len()],
    }
}

/// The identity wheel on white: the same six families, the darker steps that read as text,
/// at least 4.5 to 1 on white and on the selected row (violet 92, olive 58, deep teal 23,
/// deep pink 125, deep sky 24, amber 94), so a title in its space's colour is read, not
/// found. The lime and yellow families have one cube entry between them that dark (58), so
/// lime takes it and yellow steps to amber 94, the nearest a space comes to a state on
/// white: 20 dE from blocked's 130, about what deep pink stands from the selected tab (18).
const LIGHT_SPACE_INKS: [u8; 6] = [92, 58, 23, 125, 24, 94];

/// The light wheel's quieter steps, slot for slot: each lighter than its base on white, so a
/// calm agent reads quieter than one that is working or wants a person (violet 98, lime 64,
/// teal 29, dusty rose 95, bright sky 27, olive gold 100). Four hold the 4.5 to 1 a title
/// needs. Lime and yellow cannot: the cube has nothing lighter than 58 and 94 in those
/// families that still reads at 4.5, so their calm steps are the old bases, 64 at 4.2 and
/// 100 at 3.8, above the 3 to 1 of a glyph a person finds.
const LIGHT_SPACE_CALM_INKS: [u8; 6] = [98, 64, 29, 95, 27, 100];

/// The slots of [`LIGHT_SPACE_CALM_INKS`] held to 3 to 1 rather than 4.5: lime and yellow.
#[cfg(test)]
const LIGHT_CALM_UNDER_TEXT: [usize; 2] = [1, 5];

/// Raised backgrounds on white: the selected row a shade under the page, the status bar
/// a shade lighter still, the same order of depth as the dark table.
const LIGHT_SELECTED_BG: u8 = 254;
const LIGHT_STATUS_BG: u8 = 255;

/// The focused tab: a deep pink, still apart from failed's magenta and every pool.
const LIGHT_TAB_ON_FG: u8 = 162;

/// An avatar ink as it should be drawn in the palette in effect.
///
/// The pet's hair is picked from light, loud inks (a yellow, a lime) that vanish on white,
/// so on a light background each is drawn one step darker in its own family. The body's
/// azure and the eyes (cream on the body, a black slit) sit on the body and not on the
/// page, so they keep their inks.
pub fn pet(ink: u8) -> u8 {
    if light() { light_pet(ink) } else { ink }
}

const fn light_pet(ink: u8) -> u8 {
    match ink {
        212 => 162,
        214 => 136,
        226 => 100,
        155 => 64,
        208 => 166,
        35 => 28,
        135 => 92,
        39 => 32,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoop_runtime::avatar;

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
            Role::Sleeping,
            Role::Unobserved,
        ]
        .into_iter()
        .chain((0..SPACE_INKS.len()).map(Role::Space))
        .chain((0..SPACE_CALM_INKS.len()).map(Role::SpaceCalm))
        .chain((0..=11).map(Role::Shimmer))
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
        let calm = [
            ink(Role::Idle),
            ink(Role::Sleeping),
            ink(Role::Unobserved),
            ink(Role::Detail),
        ];
        for loud in [Role::Blocked, Role::Failed, Role::Working, Role::Done] {
            assert!(!calm.contains(&ink(loud)), "{loud:?} is painted as calm");
        }
        // Far apart by palette and pinned: failed is vivid magenta against the
        // warm alarm pair, and the dot's shape carries with the hue (see the
        // sidebar's failed diamond).
        assert_ne!(ink(Role::Blocked), ink(Role::Failed));
        assert_ne!(ink(Role::Working), ink(Role::Done));
    }

    /// Sleeping is a state with a measured seat: muted mauve 140 (L* 63,
    /// 7.3:1 on black, 42 dE from the closest state ink, the working sky),
    /// named by `of`, distinct from every state, chrome and space ink.
    /// An unknown state still falls back to Unobserved: a newer daemon may
    /// say more than this build understands.
    /// Background work is derived, so no state word maps to it, and it spends no ink of its
    /// own: it is the working sky in both palettes.
    #[test]
    fn background_wears_the_working_ink_and_no_state_word_names_it() {
        assert_eq!(ink(Role::Background), ink(Role::Working));
        assert_eq!(dark_ink(Role::Background), 39);
        assert_eq!(light_ink(Role::Background), light_ink(Role::Working));
        for word in [
            "working", "starting", "idle", "done", "blocked", "failed", "sleeping",
        ] {
            assert_ne!(of(word), Role::Background);
        }
    }

    #[test]
    fn sleeping_has_its_own_measured_colour_and_the_unknown_falls_back() {
        assert_eq!(of("sleeping"), Role::Sleeping);
        assert_eq!(ink(Role::Sleeping), 140);
        assert_eq!(of("whatever-comes-next"), Role::Unobserved);
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
            !taken.contains(&ink(Role::Sleeping)),
            "sleeping wears a taken colour"
        );
        for n in 0..SPACE_INKS.len() {
            assert_ne!(
                ink(Role::Space(n)),
                ink(Role::Sleeping),
                "space {n} wears the sleeping colour"
            );
        }
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
            Role::Sleeping,
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
            Role::Sleeping,
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
        taken.extend(SPACE_INKS);
        for n in 0..SPACE_INKS.len() {
            assert!(
                !taken.contains(&ink(Role::SpaceCalm(n))),
                "calm space {n} wears a colour that already means something"
            );
        }
        taken.truncate(taken.len() - SPACE_INKS.len());
        taken.extend(SPACE_CALM_INKS);
        for n in 0..SPACE_INKS.len() {
            assert!(
                !taken.contains(&ink(Role::Space(n))),
                "space {n} wears a colour that already means something"
            );
        }
    }

    /// Identity and aliveness share no ink: a sweep step the colour of a space would
    /// read as a project, and a space the colour of a sweep step as motion. The
    /// selected tab is neither pool's nor any state's: it must not read as a project,
    /// a sweep, or a state from the bar.
    #[test]
    fn a_space_is_never_a_sweep_step_and_the_tab_is_neither() {
        let sweep: Vec<u8> = (0..=11).map(|n| ink(Role::Shimmer(n))).collect();
        for n in 0..SPACE_INKS.len() {
            assert!(
                !sweep.contains(&ink(Role::Space(n))),
                "space {n} wears a sweep step"
            );
            assert!(
                !sweep.contains(&ink(Role::SpaceCalm(n))),
                "calm space {n} wears a sweep step"
            );
        }
        for (name, pool) in [
            ("space", SPACE_INKS.to_vec()),
            ("calm space", SPACE_CALM_INKS.to_vec()),
            ("sweep", sweep.clone()),
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
            Role::Sleeping,
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

    /// A picked colour wins over the one the id gives, and no pick, a space not in the
    /// list, or an ink past the six all fall back to it.
    #[test]
    fn a_picked_colour_wins_and_anything_else_is_auto() {
        let row = |id, ink| qoop_runtime::control::WorkspaceRow {
            dir: None,
            keep_awake: false,
            qass_skip: false,
            id,
            name: String::new(),
            terminals: 0,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink,
            link: None,
            goal: None,
        };
        let spaces = [row(1, Some(4)), row(2, None), row(3, Some(7))];
        assert_eq!(space_in(&spaces, 1), Role::Space(4));
        assert_eq!(space_calm_in(&spaces, 1), Role::SpaceCalm(4));
        for id in [2, 3, 9] {
            assert_eq!(space_in(&spaces, id), space_of(id), "space {id}");
            assert_eq!(space_calm_in(&spaces, id), space_calm_of(id), "space {id}");
        }
    }

    /// Every space wears its own family: six slots, six families, so no two
    /// projects ever share a hue family however the wheel turns.
    #[test]
    fn neighbouring_spaces_are_never_of_one_family() {
        fn family(ink: u8) -> &'static str {
            match ink {
                177 => "violet",
                191 => "lime",
                87 => "cyan",
                218 => "pink",
                117 => "sky",
                228 => "yellow",
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

    /// The sweep climbs the grey ramp to white in twelve distinct steps, every one
    /// brighter than the quiet row it crosses, and a level past the top stays white.
    #[test]
    fn the_sweep_climbs_distinct_greys_above_the_quiet_row_to_white() {
        let steps: Vec<u8> = (0..=11).map(|n| ink(Role::Shimmer(n))).collect();
        assert!(steps.windows(2).all(|w| w[0] < w[1]), "{steps:?}");
        assert!(
            steps[0] > ink(Role::Faint),
            "the darkest step must still read"
        );
        assert_eq!(steps[11], 255);
        assert_eq!(ink(Role::Shimmer(200)), 255);
    }

    /// A 256 palette entry as rgb, the way xterm defines the cube and the greys.
    fn rgb(entry: u8) -> [f64; 3] {
        if entry >= 232 {
            let v = f64::from(8 + 10 * (entry - 232));
            return [v, v, v];
        }
        let levels = [0.0, 95.0, 135.0, 175.0, 215.0, 255.0];
        let i = usize::from(entry - 16);
        [levels[i / 36], levels[(i / 6) % 6], levels[i % 6]]
    }

    /// WCAG contrast between two palette entries.
    fn contrast(a: u8, b: u8) -> f64 {
        contrast_rgb(rgb(a), rgb(b))
    }

    /// WCAG contrast between two colours.
    fn contrast_rgb(a: [f64; 3], b: [f64; 3]) -> f64 {
        let luminance = |colour: [f64; 3]| {
            let [r, g, b] = colour.map(|c| {
                let c = c / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            });
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// The text roles are read, so each holds 4.5 to 1 on every ground it is painted on: the
    /// page (black and white, and the common #1d1f21 and #fafafa terminal grounds), the
    /// status bar and the selected row, in both tables.
    #[test]
    fn every_text_role_reads_on_every_ground() {
        let text = [
            Role::BrandText,
            Role::WorkingText,
            Role::FailedText,
            Role::FaintText,
            Role::IdleText,
        ];
        let grounds = [
            (
                "dark",
                dark_ink as fn(Role) -> u8,
                [
                    rgb(16),
                    [29.0, 31.0, 33.0],
                    rgb(SELECTED_BG),
                    rgb(STATUS_BG),
                ],
            ),
            (
                "light",
                light_ink,
                [
                    rgb(231),
                    [250.0; 3],
                    rgb(LIGHT_SELECTED_BG),
                    rgb(LIGHT_STATUS_BG),
                ],
            ),
        ];
        for (table, ink, grounds) in grounds {
            for role in text {
                for ground in grounds {
                    let ratio = contrast_rgb(rgb(ink(role)), ground);
                    assert!(
                        ratio >= 4.5,
                        "{table} {role:?} is {ratio:.2}:1 on {ground:?}"
                    );
                }
            }
        }
        // Brand words never wear a state's ink: light blocked is 130, dark blocked 208.
        assert_ne!(light_ink(Role::BrandText), light_ink(Role::Blocked));
        assert_ne!(dark_ink(Role::BrandText), dark_ink(Role::Blocked));
    }

    /// The light table, measured against white (231): text at least 4.5 to 1, everything
    /// a person finds rather than reads at least 3 to 1, the rule exempt by name. And on
    /// the raised backgrounds, each ink that is painted on one holds the same line there.
    #[test]
    fn every_light_ink_reads_on_a_white_background() {
        const WHITE: u8 = 231;
        let text = [
            Role::Title,
            Role::Text,
            Role::Focused,
            Role::Detail,
            Role::Faint,
        ];
        // A title is read, so its space ink holds the text floor, on the page and, for the
        // keyboard's row, on the selected row. The calm steps hold it too, but for the two
        // families the cube cannot give a lighter readable step (see the table).
        let spaces = (0..LIGHT_SPACE_INKS.len()).map(Role::Space);
        let calm = (0..LIGHT_SPACE_CALM_INKS.len())
            .filter(|n| !LIGHT_CALM_UNDER_TEXT.contains(n))
            .map(Role::SpaceCalm);
        for role in text.into_iter().chain(spaces.clone()).chain(calm) {
            let ratio = contrast(light_ink(role), WHITE);
            assert!(ratio >= 4.5, "{role:?} is {ratio:.2}:1 on white");
        }
        for role in spaces {
            let ratio = contrast(light_ink(role), LIGHT_SELECTED_BG);
            assert!(ratio >= 4.5, "{role:?} on the selected row is {ratio:.2}:1");
        }
        // Quieter is the point of the calm step: on white it sits clearly under its base.
        for n in 0..LIGHT_SPACE_INKS.len() {
            let (base, calm) = (
                contrast(light_ink(Role::Space(n)), WHITE),
                contrast(light_ink(Role::SpaceCalm(n)), WHITE),
            );
            assert!(
                calm * 1.2 <= base,
                "calm space {n} is {calm:.2} beside {base:.2}"
            );
            let floor = if LIGHT_CALM_UNDER_TEXT.contains(&n) {
                3.0
            } else {
                4.5
            };
            assert!(calm >= floor, "calm space {n} is {calm:.2}:1 on white");
        }
        let states = [
            Role::Working,
            Role::Blocked,
            Role::Done,
            Role::Failed,
            Role::Idle,
            Role::Sleeping,
            Role::Unobserved,
        ];
        let found = states
            .into_iter()
            .chain([Role::Brand])
            .chain((0..LIGHT_SPACE_INKS.len()).map(Role::Space))
            .chain((1..=9).map(Role::Shimmer))
            .chain(avatar::HAIR_INKS.map(Role::AvatarHair));
        for role in found {
            let ratio = contrast(light_ink(role), WHITE);
            assert!(ratio >= 3.0, "{role:?} is {ratio:.2}:1 on white");
        }
        for (ink, on, floor) in [
            (light_ink(Role::Detail), LIGHT_SELECTED_BG, 4.5),
            (light_ink(Role::Detail), LIGHT_STATUS_BG, 4.5),
            (light_ink(Role::Focused), LIGHT_STATUS_BG, 4.5),
            (LIGHT_TAB_ON_FG, LIGHT_SELECTED_BG, 3.0),
        ] {
            let ratio = contrast(ink, on);
            assert!(ratio >= floor, "{ink} on {on} is {ratio:.2}:1");
        }
        for role in states {
            let ratio = contrast(light_ink(role), LIGHT_SELECTED_BG);
            assert!(ratio >= 3.0, "{role:?} on the selected row is {ratio:.2}:1");
        }
    }

    /// A swatch's number reads on its ink: black on every dark space ink, white on every
    /// light one, at least 3 to 1, a glyph a person finds.
    #[test]
    fn a_swatchs_number_reads_on_its_ink() {
        for n in 0..6 {
            let dark = contrast(16, SPACE_INKS[n]);
            assert!(dark >= 3.0, "dark swatch {n} is {dark:.2}:1");
            let light = contrast(231, LIGHT_SPACE_INKS[n]);
            assert!(light >= 3.0, "light swatch {n} is {light:.2}:1");
        }
        assert_eq!(swatch_for(2, crate::colorlevel::Level::Mono), RESET);
    }

    /// The dark table, measured against black (16), the ground most dark terminals sit on:
    /// the same floors as the light table, text at least 4.5 to 1, everything a person
    /// finds rather than reads at least 3 to 1. Exempt by name: the rule, and unobserved,
    /// the not yet seen agent, kept a step under idle's grey so the two stay apart (idle is
    /// 242 at 4.0:1; there is no grey between it and 3:1 that still reads as quieter).
    #[test]
    fn every_dark_ink_reads_on_a_black_background() {
        const BLACK: u8 = 16;
        for role in [
            Role::Title,
            Role::Text,
            Role::Focused,
            Role::Detail,
            Role::Faint,
        ]
        .into_iter()
        .chain((0..SPACE_CALM_INKS.len()).map(Role::SpaceCalm))
        {
            let ratio = contrast(dark_ink(role), BLACK);
            assert!(ratio >= 4.5, "{role:?} is {ratio:.2}:1 on black");
        }
        // Quieter is the point of the calm step: on black it sits clearly under its base.
        for n in 0..SPACE_INKS.len() {
            let (base, calm) = (
                contrast(dark_ink(Role::Space(n)), BLACK),
                contrast(dark_ink(Role::SpaceCalm(n)), BLACK),
            );
            assert!(
                calm * 1.3 <= base,
                "calm space {n} is {calm:.2} beside {base:.2}"
            );
        }
        let found = [
            Role::Working,
            Role::Blocked,
            Role::Done,
            Role::Failed,
            Role::Idle,
            Role::Sleeping,
            Role::Brand,
        ]
        .into_iter()
        .chain((0..SPACE_INKS.len()).map(Role::Space))
        .chain(avatar::HAIR_INKS.map(Role::AvatarHair));
        for role in found {
            let ratio = contrast(dark_ink(role), BLACK);
            assert!(ratio >= 3.0, "{role:?} is {ratio:.2}:1 on black");
        }
    }

    /// The light table keeps the dark one's rules: nothing themed, the states that want a
    /// person apart from the calm ones, spaces apart from states, chrome, the tab and the
    /// sweep.
    #[test]
    fn the_light_table_keeps_the_rules_of_the_dark_one() {
        let chrome = [
            Role::Title,
            Role::Text,
            Role::Focused,
            Role::Detail,
            Role::Faint,
            Role::Rule,
            Role::Brand,
        ];
        let states = [
            Role::Working,
            Role::Blocked,
            Role::Done,
            Role::Failed,
            Role::Idle,
            Role::Sleeping,
            Role::Unobserved,
        ];
        let mut taken: Vec<u8> = chrome.into_iter().chain(states).map(light_ink).collect();
        for &ink in &taken {
            assert!(ink >= 16, "{ink} is a themed colour");
        }
        let mut distinct = states.map(light_ink).to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), states.len(), "two states share an ink");
        taken.push(LIGHT_TAB_ON_FG);
        let sweep: Vec<u8> = (1..=9).map(|n| light_ink(Role::Shimmer(n))).collect();
        for space in LIGHT_SPACE_INKS.into_iter().chain(LIGHT_SPACE_CALM_INKS) {
            assert!(
                !taken.contains(&space),
                "space {space} already means something"
            );
            assert!(!sweep.contains(&space), "space {space} is a sweep step");
        }
        let mut wheel = LIGHT_SPACE_INKS.to_vec();
        wheel.extend(LIGHT_SPACE_CALM_INKS);
        wheel.sort_unstable();
        wheel.dedup();
        assert_eq!(wheel.len(), 12, "two light space steps share an ink");
        assert!(!sweep.contains(&LIGHT_TAB_ON_FG));
    }

    #[test]
    fn a_background_is_light_by_its_luminance_and_colorfgbg_is_a_first_guess() {
        assert!(is_light_background([255, 255, 255]));
        assert!(is_light_background([250, 244, 230]));
        assert!(!is_light_background([0, 0, 0]));
        assert!(!is_light_background([40, 42, 54]));
        assert!(
            !is_light_background([0, 0, 255]),
            "a saturated blue is dark"
        );
        assert_eq!(colorfgbg_is_light("0;15"), Some(true));
        assert_eq!(colorfgbg_is_light("0;default;7"), Some(true));
        assert_eq!(colorfgbg_is_light("15;0"), Some(false));
        assert_eq!(colorfgbg_is_light("7;8"), Some(false));
        assert_eq!(colorfgbg_is_light("default;default"), None);
        assert_eq!(colorfgbg_is_light(""), None);
    }

    /// Monochrome chrome carries no SGR colour anywhere: no foreground, no
    /// background, only reset and bold. Focus stays findable through bold.
    #[test]
    fn monochrome_chrome_emits_no_colour_and_keeps_focus_bold() {
        use crate::colorlevel::Level::Mono;
        let escapes = vec![
            fg_for(Role::Title, Mono),
            fg_for(Role::Text, Mono),
            fg_for(Role::Focused, Mono),
            fg_for(Role::Blocked, Mono),
            selected_for(Mono),
            selected_in_for(Role::Failed, Mono),
            status_for(Mono),
            status_key_for(Mono),
            status_in_for(Role::Blocked, Mono),
            tab_on_for(Mono),
            tab_in_for(Role::Space(0), Mono),
        ];
        for escape in &escapes {
            assert!(
                !escape.contains("38;") && !escape.contains("48;"),
                "colour leaked into monochrome chrome: {escape:?}"
            );
        }
        for escape in [
            fg_for(Role::Title, Mono),
            fg_for(Role::Focused, Mono),
            selected_for(Mono),
            status_key_for(Mono),
            tab_on_for(Mono),
        ] {
            assert!(
                escape.contains(";1m"),
                "focus lost its bold in monochrome: {escape:?}"
            );
        }
    }
}
