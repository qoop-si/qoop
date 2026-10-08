//! Real pictures in a text multiplexer: capability, placeholder, reconciliation.
//!
//! The daemon delivers image envelopes beside the grid (`StoredImage`); this module
//! decides what the outer terminal gets. Three cases, decided per frame:
//!
//! * The outer terminal speaks the image's protocol: the envelope goes out verbatim,
//!   positioned over the focused pane. The text frame never sees it, so the row diff
//!   cannot slice inside base64.
//! * It does not: a dim placeholder line names the picture (`[image 800x600, 123KB]`)
//!   where it would have been. A path the agent printed stays in the scrollback like
//!   any other text; the placeholder is the picture's stand-in, not the path's.
//! * Nothing arrived: nothing is drawn, and no code path may emit image bytes.
//!
//! Placement is reconciled per frame, not at call sites. Kitty placements live outside
//! the grid and survive repaints, so every way the screen can change (focus, resize,
//! scrollback, hide) must delete the stale placement. One function sees them all: it
//! compares what is on the outer screen (`shown`) with what should be (`desired`) and
//! emits deletes for the difference. Neither iTerm2 nor sixel has a remote delete,
//! so their placements are forgotten rather than removed, which the plan documents
//! as a known protocol limit.

use std::collections::BTreeMap;

use qoop_runtime::daemon::{ImageProtocol, StoredImage};

/// What the outer terminal can draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicsCap {
    pub kitty: bool,
    pub iterm2: bool,
    /// Sixel is never in the default allowlist: no outer sixel support is
    /// verified on a device yet. `QOOP_GRAPHICS=sixel` or `all` turns it on,
    /// and then envelopes pass through verbatim like the other two.
    pub sixel: bool,
}

impl GraphicsCap {
    pub fn off() -> Self {
        Self {
            kitty: false,
            iterm2: false,
            sixel: false,
        }
    }

    pub fn allows(self, protocol: ImageProtocol) -> bool {
        match protocol {
            ImageProtocol::Kitty => self.kitty,
            ImageProtocol::ITerm2 => self.iterm2,
            ImageProtocol::Sixel => self.sixel,
        }
    }
}

/// What the outer terminal speaks, read once at startup.
///
/// An explicit override first (`QOOP_GRAPHICS=kitty|iterm2|sixel|all|off`), because
/// terminal identification lies: under ssh or tmux the variables name the outer
/// multiplexer rather than the emulator behind it, and a wrong positive prints raw
/// base64. Without an override, a conservative allowlist: Kitty where the
/// variables say kitty or Ghostty (both document the protocol), iTerm2 inline
/// where they say iTerm, and Kitty there too, because iTerm2 documents the Kitty
/// protocol beside its own. WezTerm gets Kitty and iTerm2: its features page
/// documents both, and the Kitty specification names it as an implementer.
/// (Kitty graphics historically needed `enable_kitty_graphics` in the WezTerm
/// config; a WezTerm that swallows the envelope still shows the picture's
/// absence rather than garbage, and one variable turns the whole thing off.)
pub fn detect() -> GraphicsCap {
    match qoop_runtime::legacy::var("QOOP_GRAPHICS")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "off" | "none" => return GraphicsCap::off(),
        "kitty" => {
            return GraphicsCap {
                kitty: true,
                iterm2: false,
                sixel: false,
            };
        }
        "iterm2" | "iterm" => {
            return GraphicsCap {
                kitty: false,
                iterm2: true,
                sixel: false,
            };
        }
        "sixel" => {
            return GraphicsCap {
                kitty: false,
                iterm2: false,
                sixel: true,
            };
        }
        "all" | "both" => {
            return GraphicsCap {
                kitty: true,
                iterm2: true,
                sixel: true,
            };
        }
        _ => {}
    }
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    let program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    let iterm = program.contains("iterm");
    let wezterm = program.contains("wezterm") || term.starts_with("wezterm");
    GraphicsCap {
        kitty: term.contains("kitty")
            || program.contains("kitty")
            || program.contains("ghostty")
            || iterm
            || wezterm,
        iterm2: iterm || wezterm,
        sixel: false,
    }
}

/// The placeholder line for a picture the outer terminal cannot draw.
///
/// Dimensions come from the envelope's own metadata when parseable, the byte size
/// always. English: this is screen UI, and UI ships English.
pub fn placeholder(image: &StoredImage) -> String {
    let size = if image.bytes.len() < 1024 {
        format!("{}B", image.bytes.len())
    } else {
        format!("{}KB", image.bytes.len() / 1024)
    };
    match dimensions(image) {
        Some((w, h)) => format!("[image {w}x{h}, {size}]"),
        None => format!("[image {size}]"),
    }
}

/// Width and height out of the envelope header, when the sender stated them.
///
/// Kitty states `s=` (width) and `v=` (height) in the comma list before the first
/// `;`. iTerm2 states `width=` and `height=` among the colon list after `File=`.
/// Sixel states pixel width and height as the third and fourth numbers after the
/// `"` raster introducer (`"Pan;Pad;Ph;Pv`). Anything unparseable reads as
/// unstated rather than as zero.
fn dimensions(image: &StoredImage) -> Option<(u32, u32)> {
    match image.protocol {
        ImageProtocol::Kitty => {
            // `ESC _ G` is three bytes, then `k=v,k=v,... ; payload ST`.
            let body = image.bytes.get(3..)?;
            let params = body.split(|&b| b == b';').next()?;
            let mut dims = [None, None];
            for pair in params.split(|&b| b == b',') {
                let mut kv = pair.splitn(2, |&b| b == b'=');
                let (k, v) = (kv.next()?, kv.next()?);
                let slot = match k {
                    b"s" => &mut dims[0],
                    b"v" => &mut dims[1],
                    _ => continue,
                };
                *slot = number(v);
            }
            Some((dims[0]?, dims[1]?))
        }
        ImageProtocol::ITerm2 => {
            let text = std::str::from_utf8(&image.bytes).ok()?;
            let file = text.split("File=").nth(1)?;
            let params = file.split(':').next().unwrap_or(file);
            let mut dims = [None, None];
            for pair in params.split(';') {
                let mut kv = pair.splitn(2, '=');
                let (k, v) = (kv.next()?, kv.next()?);
                let slot = match k {
                    "width" => &mut dims[0],
                    "height" => &mut dims[1],
                    _ => continue,
                };
                // Values carry units (`100px`, `50%`) or `auto`: leading digits only.
                let digits: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
                *slot = digits.parse().ok();
            }
            Some((dims[0]?, dims[1]?))
        }
        ImageProtocol::Sixel => {
            let text = std::str::from_utf8(&image.bytes).ok()?;
            let raster = text.split('"').nth(1)?;
            let mut fields = raster.split(';');
            let (w, h) = (fields.nth(2)?, fields.next()?);
            let digits = |v: &str| {
                let head: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
                if head.is_empty() || head.len() > 5 {
                    None
                } else {
                    head.parse::<u32>().ok()
                }
            };
            Some((digits(w)?, digits(h)?))
        }
    }
}

/// ASCII digits to a bounded number. Unbounded `parse` on hostile input is how a
/// picture line becomes a panic; five digits are more than any screen holds.
fn number(raw: &[u8]) -> Option<u32> {
    if raw.is_empty() || raw.len() > 5 || !raw.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(raw).ok()?.parse().ok()
}

/// Whether an envelope is shaped well enough to emit.
///
/// The sniffer only completes well-formed envelopes, but the daemon and the
/// socket stand between it and this client. A malformed envelope inside the
/// synchronized update could break its atomicity or walk the cursor, so shape
/// is checked again at the last moment: anything else reads as absent, and the
/// placeholder is what the client draws instead.
fn is_placeable(image: &StoredImage) -> bool {
    // Between the introducer and the terminator only printable ASCII: base64 and the
    // `key=value;` arguments are nothing else. A CAN or SUB there aborts the sequence in the
    // outer terminal and prints the rest, and an 8-bit C1 byte can open one the control
    // string filter never saw. CR and LF are allowed: `base64` wraps its output, and a
    // terminal ignores them inside the string.
    let (open, close) = match image.protocol {
        ImageProtocol::Kitty => (3, 2),
        ImageProtocol::ITerm2 => (2, if image.bytes.ends_with(b"\x07") { 1 } else { 2 }),
        // `ESC P` introducer, `ST` terminator. Sixel data lives in the same
        // printable range the shared check below enforces.
        ImageProtocol::Sixel => (2, 2),
    };
    let printable = image
        .bytes
        .get(open..image.bytes.len().saturating_sub(close))
        .is_some_and(|body| {
            body.iter()
                .all(|b| (0x20..0x7f).contains(b) || *b == b'\r' || *b == b'\n')
        });
    printable
        && match image.protocol {
            ImageProtocol::Kitty => {
                image.bytes.len() >= 7
                    && image.bytes.starts_with(b"\x1b_G")
                    && image.bytes.ends_with(b"\x1b\\")
                    && image.bytes[3..image.bytes.len() - 2].contains(&b';')
            }
            ImageProtocol::ITerm2 => {
                image.bytes.starts_with(b"\x1b]1337;File=")
                    && (image.bytes.ends_with(b"\x07") || image.bytes.ends_with(b"\x1b\\"))
            }
            ImageProtocol::Sixel => {
                image.bytes.starts_with(b"\x1bP")
                    && image.bytes.ends_with(b"\x1b\\")
                    && image
                        .bytes
                        .get(2..image.bytes.len() - 2)
                        .is_some_and(|body| {
                            // Parameters, then the `q` that opens sixel data.
                            let mut parts = body.splitn(2, |&b| b == b'q');
                            let params = parts.next().unwrap_or_default();
                            params.iter().all(|b| (0x30..=0x3b).contains(b))
                                && parts.next().is_some()
                        })
            }
        }
}

/// One thing the image block must do on the outer terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageOp {
    /// Remove a Kitty placement by id. Neither iTerm2 nor sixel has a remote
    /// delete, so their placements are forgotten, never removed.
    Delete { id: u64 },
    /// Draw an envelope with its top-left cell at (`row`, `col`), one based.
    Place {
        row: usize,
        col: usize,
        bytes: Vec<u8>,
    },
}

/// What is on the outer screen, per terminal: the image id and its protocol.
/// The protocol decides whether a stale placement can be deleted (Kitty) or only
/// forgotten (iTerm2).
pub type Shown = BTreeMap<u32, (u64, ImageProtocol)>;

/// Reconcile one frame: what must be deleted and placed for the outer screen to
/// show exactly `desired`.
///
/// * `image`: the focused terminal's newest envelope, if it has one.
/// * `cap`: what the outer terminal draws. What it cannot draw is never emitted;
///   the placeholder covers that case.
/// * `anchored`: scrolled back. The screen shows history, which holds no pictures,
///   so every placement comes down until the view returns to live.
/// * `resized`: the outer window changed under the placements. Cell-anchored
///   overlays do not survive a reflow where they were, so all come down and the
///   current one goes back up.
/// * `at`: the one based cell a new placement goes to, or `None` when there is no
///   pane for it (nothing open).
///
/// Returns the ops and the new `shown`. A terminal with no image and no placement
/// contributes nothing: the common frame carries no image bytes at all.
pub fn plan(
    terminal: u32,
    image: Option<&StoredImage>,
    shown: &Shown,
    cap: GraphicsCap,
    anchored: bool,
    resized: bool,
    at: Option<(usize, usize)>,
) -> (Vec<ImageOp>, Shown) {
    let desired: Option<(u64, ImageProtocol)> = match (anchored, image) {
        (false, Some(img)) if terminal != 0 && cap.allows(img.protocol) && is_placeable(img) => {
            Some((img.id, img.protocol))
        }
        _ => None,
    };
    let mut ops = Vec::new();
    let mut next = if resized { Shown::new() } else { shown.clone() };
    // Stale placements come down first, so a replacement never shares the screen
    // with what it replaces.
    for (t, (id, protocol)) in shown {
        let stale = resized || Some((*t, (*id, *protocol))) != desired.map(|d| (terminal, d));
        if stale {
            if *protocol == ImageProtocol::Kitty {
                ops.push(ImageOp::Delete { id: *id });
            }
            next.remove(t);
        }
    }
    if let Some((id, protocol)) = desired
        && next.get(&terminal) != Some(&(id, protocol))
        && let Some(img) = image
        && let Some((row, col)) = at
    {
        ops.push(ImageOp::Place {
            row,
            col,
            bytes: img.bytes.clone(),
        });
        next.insert(terminal, (id, protocol));
    }
    (ops, next)
}

/// Delete a Kitty placement by id: `ESC _ G a=d,d=I,i=<id> ESC \`.
pub fn delete_for(id: u64) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id}\x1b\\").into_bytes()
}

const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";
const HIDE: &[u8] = b"\x1b[?25l";
const SHOW: &[u8] = b"\x1b[?25h";

/// The image block: every op inside one synchronized update, the cursor restored
/// after it. Empty ops emit nothing, so a frame with no picture work does not
/// touch the cursor at all.
///
/// Adjacent to the text frame's own block in the same flush rather than inside
/// it: the text block is built as a string the row diff owns, and image bytes
/// are not valid string content to diff. Two blocks in one flush render without
/// a visible tear between them; the text block stays exactly what it was.
pub fn block(ops: &[ImageOp], cursor: ((usize, usize), bool)) -> Vec<u8> {
    if ops.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    out.extend_from_slice(SYNC_BEGIN);
    out.extend_from_slice(HIDE);
    for op in ops {
        match op {
            ImageOp::Delete { id } => out.extend_from_slice(&delete_for(*id)),
            ImageOp::Place { row, col, bytes } => {
                out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
                out.extend_from_slice(bytes);
            }
        }
    }
    out.extend_from_slice(format!("\x1b[{};{}H", cursor.0.0, cursor.0.1).as_bytes());
    out.extend_from_slice(if cursor.1 { SHOW } else { HIDE });
    out.extend_from_slice(SYNC_END);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitty(id: u64, params: &str, payload: &str) -> StoredImage {
        StoredImage {
            terminal: 1,
            id,
            protocol: ImageProtocol::Kitty,
            bytes: format!("\x1b_G{params};{payload}\x1b\\").into_bytes(),
        }
    }

    fn iterm(id: u64) -> StoredImage {
        StoredImage {
            terminal: 1,
            id,
            protocol: ImageProtocol::ITerm2,
            bytes: b"\x1b]1337;File=width=800px;height=600px;inline=1:Zm9v\x07".to_vec(),
        }
    }

    fn sixel(id: u64) -> StoredImage {
        StoredImage {
            terminal: 1,
            id,
            protocol: ImageProtocol::Sixel,
            bytes: b"\x1bP1;1;800;600q\"1;1;800;600#0;2;0;0;0$-\x1b\\".to_vec(),
        }
    }

    fn cap() -> GraphicsCap {
        GraphicsCap {
            kitty: true,
            iterm2: true,
            sixel: true,
        }
    }

    #[test]
    fn a_new_image_is_placed_and_remembered() {
        let img = kitty(4, "f=100,s=800,v=600", "eA==");
        let (ops, next) = plan(
            1,
            Some(&img),
            &Shown::new(),
            cap(),
            false,
            false,
            Some((20, 5)),
        );
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            ImageOp::Place {
                row: 20,
                col: 5,
                ..
            }
        ));
        assert_eq!(next.get(&1), Some(&(4, ImageProtocol::Kitty)));
    }

    #[test]
    fn the_same_image_twice_emits_nothing() {
        let img = kitty(4, "f=100", "eA==");
        let shown: Shown = [(1, (4, ImageProtocol::Kitty))].into_iter().collect();
        let (ops, next) = plan(1, Some(&img), &shown, cap(), false, false, Some((20, 5)));
        assert!(ops.is_empty());
        assert_eq!(next, shown);
    }

    #[test]
    fn a_replacement_deletes_before_it_places() {
        let img = kitty(5, "f=100", "eA==");
        let shown: Shown = [(1, (4, ImageProtocol::Kitty))].into_iter().collect();
        let (ops, next) = plan(1, Some(&img), &shown, cap(), false, false, Some((20, 5)));
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0], ImageOp::Delete { id: 4 });
        assert!(matches!(&ops[1], ImageOp::Place { .. }));
        assert_eq!(next.get(&1), Some(&(5, ImageProtocol::Kitty)));
    }

    #[test]
    fn hiding_deletes_and_forgets() {
        // Focus moved away: no image desired, the placement comes down.
        let shown: Shown = [(1, (4, ImageProtocol::Kitty))].into_iter().collect();
        let (ops, next) = plan(2, None, &shown, cap(), false, false, Some((20, 5)));
        assert_eq!(ops, vec![ImageOp::Delete { id: 4 }]);
        assert!(next.is_empty());
    }

    #[test]
    fn scrollback_brings_every_placement_down() {
        let img = kitty(4, "f=100", "eA==");
        let shown: Shown = [(1, (4, ImageProtocol::Kitty))].into_iter().collect();
        let (ops, next) = plan(1, Some(&img), &shown, cap(), true, false, Some((20, 5)));
        assert_eq!(ops, vec![ImageOp::Delete { id: 4 }]);
        assert!(next.is_empty());
    }

    #[test]
    fn resize_deletes_all_and_places_the_current_one_again() {
        let img = kitty(4, "f=100", "eA==");
        let shown: Shown = [(1, (4, ImageProtocol::Kitty))].into_iter().collect();
        let (ops, next) = plan(1, Some(&img), &shown, cap(), false, true, Some((20, 5)));
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0], ImageOp::Delete { id: 4 });
        assert!(matches!(&ops[1], ImageOp::Place { .. }));
        assert_eq!(next.get(&1), Some(&(4, ImageProtocol::Kitty)));
    }

    #[test]
    fn an_iterm2_placement_is_forgotten_never_deleted() {
        // No remote delete exists for iTerm2 inline images: the stale one cannot
        // be removed, so it is dropped from tracking and the new one goes up.
        let img = iterm(6);
        let shown: Shown = [(1, (5, ImageProtocol::ITerm2))].into_iter().collect();
        let (ops, next) = plan(1, Some(&img), &shown, cap(), false, false, Some((20, 5)));
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], ImageOp::Place { .. }));
        assert_eq!(next.get(&1), Some(&(6, ImageProtocol::ITerm2)));
    }

    #[test]
    fn what_the_outer_terminal_cannot_draw_is_never_emitted() {
        let img = kitty(4, "f=100", "eA==");
        let off = GraphicsCap::off();
        let (ops, next) = plan(
            1,
            Some(&img),
            &Shown::new(),
            off,
            false,
            false,
            Some((20, 5)),
        );
        assert!(ops.is_empty());
        assert!(next.is_empty());
    }

    #[test]
    fn no_pane_no_placement_but_stale_still_comes_down() {
        let shown: Shown = [(1, (4, ImageProtocol::Kitty))].into_iter().collect();
        let (ops, next) = plan(0, None, &shown, cap(), false, false, None);
        assert_eq!(ops, vec![ImageOp::Delete { id: 4 }]);
        assert!(next.is_empty());
    }

    #[test]
    fn the_placeholder_names_dimensions_and_size() {
        let img = kitty(1, "f=100,s=800,v=600", &"eA==".repeat(3000));
        assert_eq!(
            placeholder(&img),
            format!("[image 800x600, {}KB]", img.bytes.len() / 1024)
        );
        // Dimensions first in the control list: no leading parameter to hide behind.
        let first = kitty(1, "s=320,v=200,f=100", "eA==");
        assert_eq!(
            placeholder(&first),
            format!("[image 320x200, {}B]", first.bytes.len())
        );
        let bare = kitty(1, "f=100", "eA==");
        assert_eq!(placeholder(&bare), format!("[image {}B]", bare.bytes.len()));
        let it = iterm(2);
        assert_eq!(
            placeholder(&it),
            format!("[image 800x600, {}B]", it.bytes.len())
        );
    }

    #[test]
    fn a_sixel_image_names_pixel_dimensions_serves_a_placeholder_and_places_on_cap() {
        let img = sixel(7);
        assert_eq!(
            placeholder(&img),
            format!("[image 800x600, {}B]", img.bytes.len())
        );
        // Off by default: no outer sixel support is verified on a device.
        let (ops, next) = plan(
            1,
            Some(&img),
            &Shown::new(),
            GraphicsCap::off(),
            false,
            false,
            Some((20, 5)),
        );
        assert!(ops.is_empty());
        assert!(next.is_empty());
        // On: the envelope passes through verbatim like the other two.
        let (ops, next) = plan(
            1,
            Some(&img),
            &Shown::new(),
            cap(),
            false,
            false,
            Some((20, 5)),
        );
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            ImageOp::Place {
                row: 20,
                col: 5,
                ..
            }
        ));
        assert_eq!(next.get(&1), Some(&(7, ImageProtocol::Sixel)));
    }

    #[test]
    fn a_sixel_placement_is_forgotten_never_deleted() {
        // No remote delete exists for sixel either: the stale one cannot be
        // removed, so it is dropped from tracking and the new one goes up.
        let img = sixel(8);
        let shown: Shown = [(1, (7, ImageProtocol::Sixel))].into_iter().collect();
        let (ops, next) = plan(1, Some(&img), &shown, cap(), false, false, Some((20, 5)));
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], ImageOp::Place { .. }));
        assert_eq!(next.get(&1), Some(&(8, ImageProtocol::Sixel)));
    }

    #[test]
    fn hostile_dimensions_read_as_unstated() {
        let img = kitty(1, "f=100,s=999999,v=abc", "eA==");
        assert!(placeholder(&img).starts_with("[image "));
        assert!(!placeholder(&img).contains("999999"));
    }

    #[test]
    fn an_empty_plan_emits_no_bytes() {
        assert!(block(&[], ((1, 1), true)).is_empty());
    }

    #[test]
    fn a_malformed_envelope_is_never_emitted() {
        let mut bad = kitty(4, "f=100", "eA==");
        bad.bytes = b"\x1b_Gf=100;eA==".to_vec();
        let (ops, next) = plan(
            1,
            Some(&bad),
            &Shown::new(),
            cap(),
            false,
            false,
            Some((20, 5)),
        );
        assert!(ops.is_empty());
        assert!(next.is_empty());
        let mut bad_iterm = iterm(5);
        bad_iterm.bytes = b"\x1b]1337;File=inline=1:Zm9v".to_vec();
        let (ops, _) = plan(
            1,
            Some(&bad_iterm),
            &Shown::new(),
            cap(),
            false,
            false,
            Some((20, 5)),
        );
        assert!(ops.is_empty());
        // A control byte in the body would abort the sequence in the outer terminal and
        // print what follows, or open one of its own.
        for inside in [b'\x18', b'\x1a', 0x9d] {
            let mut hostile = iterm(6);
            hostile.bytes = [
                b"\x1b]1337;File=inline=1:Zm9v".as_slice(),
                &[inside],
                b"x\x07",
            ]
            .concat();
            let (ops, _) = plan(
                1,
                Some(&hostile),
                &Shown::new(),
                cap(),
                false,
                false,
                Some((20, 5)),
            );
            assert!(ops.is_empty(), "{inside:#x} reached the outer terminal");
        }
    }

    #[test]
    fn a_block_carries_ops_inside_one_synchronized_update() {
        let ops = vec![
            ImageOp::Delete { id: 4 },
            ImageOp::Place {
                row: 20,
                col: 5,
                bytes: b"\x1b_Gf=100;eA==\x1b\\".to_vec(),
            },
        ];
        let out = block(&ops, ((3, 7), true));
        assert!(out.starts_with(b"\x1b[?2026h"));
        assert!(out.ends_with(b"\x1b[?2026l"));
        assert!(out.windows(7).any(|w| w == b"\x1b[20;5H"));
        assert!(out.windows(6).any(|w| w == b"\x1b[3;7H"));
        assert!(out.ends_with(b"\x1b[3;7H\x1b[?25h\x1b[?2026l"));
    }

    /// Capability detection reads the override first and the allowlist second,
    /// in one test because both mutate the process environment.
    #[test]
    fn capability_detection_honours_override_then_allowlist() {
        let key = "QOOP_GRAPHICS";
        for (value, want) in [
            ("off", GraphicsCap::off()),
            (
                "kitty",
                GraphicsCap {
                    kitty: true,
                    iterm2: false,
                    sixel: false,
                },
            ),
            (
                "iterm2",
                GraphicsCap {
                    kitty: false,
                    iterm2: true,
                    sixel: false,
                },
            ),
            (
                "sixel",
                GraphicsCap {
                    kitty: false,
                    iterm2: false,
                    sixel: true,
                },
            ),
            (
                "all",
                GraphicsCap {
                    kitty: true,
                    iterm2: true,
                    sixel: true,
                },
            ),
        ] {
            // SAFETY: only this test reads or writes QOOP_GRAPHICS, TERM and
            // TERM_PROGRAM in this binary, so no other thread sees them change.
            unsafe { std::env::set_var(key, value) };
            assert_eq!(detect(), want, "override {value}");
        }
        // SAFETY: as above, no other test touches this variable.
        unsafe { std::env::remove_var(key) };
        // Without an override the allowlist decides. iTerm2 speaks both protocols
        // (its own inline images and Kitty graphics), so it gets both; anything
        // unrecognised gets nothing rather than base64 garbage.
        for (term, program, want) in [
            (
                "xterm-256color",
                "iTerm.app",
                GraphicsCap {
                    kitty: true,
                    iterm2: true,
                    sixel: false,
                },
            ),
            (
                "xterm-ghostty",
                "ghostty",
                GraphicsCap {
                    kitty: true,
                    iterm2: false,
                    sixel: false,
                },
            ),
            (
                "xterm-kitty",
                "",
                GraphicsCap {
                    kitty: true,
                    iterm2: false,
                    sixel: false,
                },
            ),
            // WezTerm documents Kitty graphics and iTerm2 inline images on its
            // features page, and the Kitty specification names it as an
            // implementer. Sixel stays override only: marked experimental there.
            (
                "wezterm",
                "WezTerm",
                GraphicsCap {
                    kitty: true,
                    iterm2: true,
                    sixel: false,
                },
            ),
            ("xterm-256color", "", GraphicsCap::off()),
        ] {
            // SAFETY: as above, no other test touches these variables.
            unsafe { std::env::set_var("TERM", term) };
            unsafe { std::env::set_var("TERM_PROGRAM", program) };
            assert_eq!(detect(), want, "terminal {term}/{program}");
        }
        // SAFETY: as above, no other test touches this variable.
        unsafe { std::env::remove_var("TERM_PROGRAM") };
    }
}
