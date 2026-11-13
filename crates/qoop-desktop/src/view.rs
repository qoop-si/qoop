//! The window: the panes, the keyboard and the input method.
//!
//! Flags:
//!   --panes N             terminals to show, 1 to 4 (default 4)
//!   --cols C --rows R     the grid each pane opens at (default 80x24)
//!   --font-size PX        6 to 72, default 12
//!   --option-as-meta      Option sends ESC plus the key instead of composing characters
//!
//! A `bench` build takes the measurement flags too (see `bench.rs`).

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, Context, EntityInputHandler, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    Pixels, Render, UTF16Selection, Window, WindowBounds, WindowOptions, div, font, point,
    prelude::*, px, size,
};
use qoop_domain::Viewport;
use qoop_runtime::control::TerminalStream;
use qoop_runtime::daemon::SnapshotWire;
use tokio::sync::mpsc::UnboundedSender;

use crate::element::{BACKGROUND, Cells, FOREGROUND, TerminalElement, rgb};
use crate::keys::{self, Press};
use crate::link::{self, Down, Latest, Up};
use crate::probe;

/// Between the panes: the ground a step lighter.
const DIVIDER: [u8; 3] = [0x26, 0x26, 0x2a];

#[derive(Clone, Debug)]
pub struct Options {
    pub panes: usize,
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub option_is_meta: bool,
    #[cfg(feature = "bench")]
    pub bench: crate::bench::Options,
}

impl Options {
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut o = Options {
            panes: 4,
            cols: 80,
            rows: 24,
            font_size: 12.,
            option_is_meta: false,
            #[cfg(feature = "bench")]
            bench: Default::default(),
        };
        while let Some(flag) = args.next() {
            let mut value = || args.next().ok_or(format!("{flag} needs a value"));
            let number = |v: String| v.parse::<u64>().map_err(|e| format!("{flag}: {e}"));
            let cells = |v: u64| u16::try_from(v).map_err(|_| format!("{flag}: too large"));
            match flag.as_str() {
                "--panes" => o.panes = number(value()?)?.clamp(1, 4) as usize,
                "--cols" => o.cols = cells(number(value()?)?)?.max(1),
                "--rows" => o.rows = cells(number(value()?)?)?.max(1),
                "--font-size" => o.font_size = number(value()?)?.clamp(6, 72) as f32,
                "--option-as-meta" => o.option_is_meta = true,
                #[cfg(feature = "bench")]
                other if o.bench.flag(other, &mut value)? => {}
                other => return Err(format!("unknown flag {other}")),
            }
        }
        Ok(o)
    }
}

pub struct Pane {
    pub terminal: Option<TerminalStream>,
    pub screen: Option<Rc<SnapshotWire>>,
    sent: Rc<Cell<(u16, u16)>>,
    viewport: Rc<Cell<Option<Viewport>>>,
}

impl Pane {
    fn empty() -> Self {
        Pane {
            terminal: None,
            screen: None,
            sent: Rc::new(Cell::new((0, 0))),
            viewport: Rc::new(Cell::new(None)),
        }
    }
}

pub struct Spike {
    panes: Vec<Pane>,
    focused: usize,
    focus: FocusHandle,
    cells: Cells,
    up: UnboundedSender<Up>,
    latest: Latest,
    /// The link is connected; when not, the panes keep the last screens, faded.
    connected: bool,
    option_is_meta: bool,
    /// Text the input method is composing (a dead key's accent, an IME preedit). Not drawn
    /// yet; what it commits goes to the terminal.
    marked: Option<String>,
    #[cfg(feature = "bench")]
    driver: crate::bench::Driver,
    _visibility: gpui::Subscription,
}

impl Spike {
    fn terminals(&self) -> Vec<TerminalStream> {
        self.panes.iter().filter_map(|p| p.terminal).collect()
    }

    fn apply(&mut self, down: Down, window: &mut Window, cx: &mut Context<Self>) {
        match down {
            Down::Terminals(terminals) => {
                self.connected = true;
                for (i, pane) in self.panes.iter_mut().enumerate() {
                    let terminal = terminals.get(i).copied();
                    if pane.terminal != terminal {
                        pane.screen = None;
                        pane.viewport.set(None);
                    }
                    pane.terminal = terminal;
                    // A new connection, maybe a new daemon: say every size again.
                    pane.sent.set((0, 0));
                }
            }
            Down::Screens => {
                let screens =
                    std::mem::take(&mut *self.latest.lock().unwrap_or_else(|e| e.into_inner()));
                for (terminal, screen) in screens {
                    if let Some(pane) = self.panes.iter_mut().find(|p| p.terminal == Some(terminal))
                    {
                        pane.screen = Some(Rc::new(screen));
                    }
                }
                let _ = self.up.send(Up::Taken);
                #[cfg(feature = "bench")]
                self.driver.on_screen(&self.panes, &self.up, window, cx);
            }
            Down::Gone(terminals) => {
                for pane in &mut self.panes {
                    if pane.terminal.is_some_and(|t| terminals.contains(&t)) {
                        *pane = Pane::empty();
                    }
                }
            }
            Down::Focus(terminal) => {
                match self.panes.iter().position(|p| p.terminal == Some(terminal)) {
                    Some(i) => self.focused = i,
                    None => {
                        let pane = &mut self.panes[self.focused];
                        let from = pane.terminal;
                        *pane = Pane::empty();
                        pane.terminal = Some(terminal);
                        let _ = self.up.send(Up::Swap { from, to: terminal });
                    }
                }
                window.activate_window();
            }
            Down::Disconnected(why) => {
                if self.connected {
                    eprintln!("qoop-desktop: {why}; connecting again");
                }
                self.connected = false;
            }
        }
        // Only the bench driver reads the context.
        let _ = cx;
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.modifiers.platform && k.key == "q" {
            cx.quit();
            return;
        }
        // Nothing typed while the daemon is away: it would land, late, in whatever runs then.
        let Some(pane) = self.panes.get(self.focused).filter(|_| self.connected) else {
            return;
        };
        let Some(terminal) = pane.terminal else {
            return;
        };
        let app_cursor = pane
            .screen
            .as_ref()
            .is_some_and(|s| s.modes.application_cursor);
        let press = Press {
            key: &k.key,
            key_char: k.key_char.as_deref(),
            ctrl: k.modifiers.control,
            alt: k.modifiers.alt,
            shift: k.modifiers.shift,
            cmd: k.modifiers.platform,
            option_is_meta: self.option_is_meta,
        };
        if let Some(bytes) = keys::encode(press, app_cursor) {
            let _ = self.up.send(Up::Input(terminal, bytes));
            cx.stop_propagation();
        }
    }

    fn pane(&self, i: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = &self.panes[i];
        let focused = self.focused == i;
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.focused = i;
                    this.marked = None;
                    cx.notify();
                }),
            )
            .child(TerminalElement {
                pane: i,
                terminal: pane.terminal,
                screen: pane.screen.clone(),
                cells: self.cells.clone(),
                focused,
                stale: !self.connected,
                sent: pane.sent.clone(),
                viewport: pane.viewport.clone(),
                up: self.up.clone(),
                input: focused.then(|| (self.focus.clone(), cx.entity())),
            })
    }

    /// The focused pane's cursor cell inside the pane's bounds, where an input method puts
    /// its candidate window.
    fn cursor_cell(&self, pane: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
        let p = self.panes.get(self.focused)?;
        let screen = p.screen.as_ref()?;
        let (top, left) = p.viewport.get().map_or((0, 0), |v| (v.row, v.col));
        let row = screen.cursor.0.saturating_sub(usize::from(top));
        let col = screen.cursor.1.saturating_sub(usize::from(left));
        Some(Bounds::new(
            point(
                pane.origin.x + self.cells.width * col as f32,
                pane.origin.y + self.cells.height * row as f32,
            ),
            size(self.cells.width, self.cells.height),
        ))
    }
}

/// Text from the input method: a dead key's accented letter, an IME's committed word.
/// Ordinary keys never come here; `key` sends them.
impl EntityInputHandler for Spike {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked.as_ref().map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.marked = None;
        if let Some(terminal) = self.panes.get(self.focused).and_then(|p| p.terminal)
            && self.connected
            && !text.is_empty()
        {
            let _ = self.up.send(Up::Input(terminal, text.as_bytes().to_vec()));
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_string());
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_cell(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for Spike {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Spike {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row = |range: Range<usize>, cx: &mut Context<Self>| {
            let mut r = div().flex().flex_row().flex_1().min_h_0().gap_px();
            for i in range {
                r = r.child(self.pane(i, cx));
            }
            r
        };
        let n = self.panes.len();
        let top = row(0..n.min(2), cx);
        let mut root = div()
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .size_full()
            .flex()
            .flex_col()
            .gap_px()
            .bg(rgb(DIVIDER))
            .child(top);
        if n > 2 {
            root = root.child(row(2..n, cx));
        }
        root
    }
}

/// The window size that gives every pane `cols` x `rows` cells.
fn window_size(o: &Options, cells: &Cells) -> gpui::Size<Pixels> {
    let across = o.panes.min(2) as f32;
    let down = if o.panes > 2 { 2. } else { 1. };
    size(
        cells.width * (f32::from(o.cols) * across) + px(across - 1.),
        cells.height * (f32::from(o.rows) * down) + px(down - 1.),
    )
}

/// The cell size of the terminal font: the advance of `m` across, 1.2 em down, both in
/// whole pixels so rows and columns never fall between device pixels.
fn cells(cx: &App, font_size: f32) -> Cells {
    let font = font("Menlo");
    let text = cx.text_system();
    let id = text.resolve_font(&font);
    let size = px(font_size);
    // Whole pixels across too: a fractional advance puts glyphs on four subpixel positions,
    // each rasterized on its own (Herdr moved to whole-pixel cells for the same reason).
    let width = text
        .advance(id, size, 'm')
        .map(|s| s.width.round())
        .unwrap_or(px((font_size * 0.6).round()))
        .max(px(1.));
    Cells {
        font,
        font_size: size,
        width,
        height: px((font_size * 1.2).ceil()),
    }
}

pub fn main() {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("qoop-desktop: {e}");
            std::process::exit(2);
        }
    };
    let socket = match qoop_runtime::runtime_dir() {
        Ok(dir) => qoop_runtime::socket_path(&dir),
        Err(e) => {
            eprintln!("qoop-desktop: {e}");
            std::process::exit(1);
        }
    };
    let (up, mut down, latest) = link::start(socket, options.panes, (FOREGROUND, BACKGROUND));
    gpui_platform::application().run(move |cx: &mut App| {
        let cells = cells(cx, options.font_size);
        let bounds = Bounds::centered(None, window_size(&options, &cells), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    let visibility =
                        cx.observe_window_visibility(window, |this: &mut Spike, seen, _, _| {
                            probe::note(|| {
                                serde_json::json!({"ev": "visibility", "visible": seen.is_visible()})
                            });
                            // Hidden, nothing drawn would be presented: stop the renders.
                            let terminals = this.terminals();
                            let _ = this.up.send(if seen.is_visible() {
                                Up::Show(terminals)
                            } else {
                                Up::Hide(terminals)
                            });
                        });
                    let spike = Spike {
                        panes: (0..options.panes).map(|_| Pane::empty()).collect(),
                        focused: 0,
                        focus,
                        cells: cells.clone(),
                        up: up.clone(),
                        latest: latest.clone(),
                        connected: false,
                        option_is_meta: options.option_is_meta,
                        marked: None,
                        #[cfg(feature = "bench")]
                        driver: crate::bench::Driver::new(options.bench.clone()),
                        _visibility: visibility,
                    };
                    cx.spawn_in(window, async move |this, cx| {
                        while let Some(first) = down.recv().await {
                            let mut batch = vec![first];
                            while let Ok(more) = down.try_recv() {
                                batch.push(more);
                            }
                            let applied = this.update_in(cx, |this, window, cx| {
                                for d in batch {
                                    this.apply(d, window, cx);
                                }
                                cx.notify();
                            });
                            if applied.is_err() {
                                return;
                            }
                        }
                        // The link thread is gone (it only ends by panicking): say so and
                        // keep the last screens faded rather than take keys into nothing.
                        eprintln!("qoop-desktop: the connection to the daemon stopped");
                        let _ = this.update(cx, |this, cx| {
                            this.connected = false;
                            cx.notify();
                        });
                    })
                    .detach();
                    spike
                })
            },
        );
        if window.is_err() {
            eprintln!("qoop-desktop: could not open a window");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
