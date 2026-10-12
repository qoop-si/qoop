//! Client-owned tiling layout: one tree per tab.
//!
//! The daemon keeps a copy of each tree (`Pane`) so a restart brings the tab back
//! whole, and never reads it: it sees independent terminals with a single resize
//! owner each. The tree only decides which terminals are visible,
//! where on the pane, the rectangle each is sized to, and which one owns the
//! keyboard. Every function here is pure so it can be tested without a screen.

use qoop_runtime::control::{Pane, SplitDir};

/// Split direction.
///
/// `V` splits the width (panes side by side, vertical divider).
/// `H` splits the height (panes stacked, horizontal divider).
/// Named after the keys that create them (`ctrl-a v`, `ctrl-a -` stacked).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    V,
    H,
}

/// Four-way focus movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
}

/// A rectangle inside the pane area, zero based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub cols: usize,
    pub rows: usize,
}

/// Minimum leaf size. A split that would leave a smaller leaf is rejected.
pub const MIN_COLS: usize = 8;
pub const MIN_ROWS: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// `None` is a placeholder: the tree entry was made before `CreateTerminal`
    /// answered. It is never painted and never focused.
    Leaf { terminal: Option<u32> },
    Split {
        dir: Dir,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A split, named by the way down to it from the root: `false` takes the first side,
/// `true` the second. What a divider drag holds on to between the press and the release.
pub type SplitPath = Vec<bool>;

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub root: Node,
    pub focus: u32,
    pub prev_focus: Option<u32>,
    pub zoomed: Option<u32>,
    /// Columns cut from the right of the root's first pane, left blank for something drawn
    /// there (qass's questions beside the board). Only ever set on a copy that is drawn,
    /// never on a tree that is kept or saved; the divider and every ratio are measured
    /// without it, so the second pane is the same with or without it.
    pub gap: usize,
    /// The gap is cut from the left (top) edge of the root's second pane instead of the right
    /// edge of its first: qass's tab with qass on the left and the board on the right.
    pub gap_second: bool,
    /// The width the owner dragged qass's questions to, in columns, `None` for the default. Kept
    /// and saved with the root split like its ratio; `gap` is the drawn result of it.
    pub questions: Option<usize>,
    /// A root pane folded away on its own side (the first on the left, the second on the right),
    /// the other taking the rest, whatever the root's direction: qass's tab with the board or
    /// qass folded. The folded pane is a tile no columns wide (its tab is on the rail). Only ever
    /// set on a drawn copy, like `gap`; the root and its ratio are left as saved. With a gap, it
    /// is cut from the open pane on the edge facing the folded one.
    pub fold: Option<u32>,
    /// Columns taken from the board (both panes when they are stacked) for qass's rail: the tabs
    /// of the folded panes, the edge line and one blank column. Only ever set on a drawn copy,
    /// like `gap`, and cut after the split is measured, so the divider and every ratio are the
    /// same with or without it and qass keeps its columns on either side.
    pub rail: usize,
}

impl Layout {
    /// One terminal filling the whole pane.
    pub fn single(terminal: u32) -> Self {
        Layout {
            root: Node::Leaf {
                terminal: Some(terminal),
            },
            focus: terminal,
            prev_focus: None,
            zoomed: None,
            gap: 0,
            gap_second: false,
            questions: None,
            fold: None,
            rail: 0,
        }
    }

    /// The tree as the daemon keeps it, or `None` while a split is on its way: a
    /// placeholder has no terminal to name, and the bind writes the tree once it has.
    pub fn to_pane(&self) -> Option<Pane> {
        let mut pane = to_pane(&self.root)?;
        if let Pane::Split { questions, .. } = &mut pane {
            *questions = self.questions.map_or(0, |w| w as u32);
        }
        Some(pane)
    }

    /// A tree the daemon kept, with the keyboard in its first pane (the tab).
    pub fn from_pane(pane: &Pane) -> Self {
        let root = from_pane(pane);
        let mut leaves = Vec::new();
        collect_leaves(&root, &mut leaves);
        Layout {
            root,
            focus: leaves.first().copied().unwrap_or(0),
            prev_focus: None,
            zoomed: None,
            gap: 0,
            gap_second: false,
            questions: match pane {
                Pane::Split { questions, .. } if *questions > 0 => Some(*questions as usize),
                _ => None,
            },
            fold: None,
            rail: 0,
        }
    }

    /// Terminals held by the tree, placeholders excluded, tree order.
    pub fn leaves(&self) -> Vec<u32> {
        let mut out = Vec::new();
        collect_leaves(&self.root, &mut out);
        out
    }

    pub fn contains(&self, terminal: u32) -> bool {
        self.leaves().contains(&terminal)
    }

    pub fn leaf_count(&self) -> usize {
        self.leaves().len()
    }

    /// Whether a split is waiting for its terminal. Only true while a real
    /// leaf stands beside the placeholder: a bare empty root is an empty tree,
    /// never a pending split.
    pub fn has_pending(&self) -> bool {
        has_live_sibling(&self.root)
    }

    /// Whether the tree holds no terminal at all.
    pub fn is_empty(&self) -> bool {
        self.leaves().is_empty()
    }

    fn set_focus(&mut self, terminal: u32) {
        if terminal != self.focus && self.contains(terminal) {
            self.prev_focus = Some(self.focus);
            self.focus = terminal;
        }
    }

    pub fn focus_terminal(&mut self, terminal: u32) -> bool {
        if !self.contains(terminal) {
            return false;
        }
        self.set_focus(terminal);
        true
    }

    /// The root split's direction and ratio, `None` for a single pane.
    pub fn root_split(&self) -> Option<(Dir, f32)> {
        match &self.root {
            Node::Split { dir, ratio, .. } => Some((*dir, *ratio)),
            Node::Leaf { .. } => None,
        }
    }

    /// Lay the root split out another way: what qass's tab shows at a width its saved split
    /// does not suit. False for a single pane.
    pub fn set_root_split(&mut self, to: Dir, to_ratio: f32) -> bool {
        match &mut self.root {
            Node::Split { dir, ratio, .. } => {
                *dir = to;
                *ratio = to_ratio;
                true
            }
            Node::Leaf { .. } => false,
        }
    }

    /// Set the root split's ratio, refusing what `set_ratio` refuses.
    pub fn set_root_ratio(&mut self, area: Rect, ratio: f32) -> bool {
        let area = self.inner(area);
        self.set_ratio(area, &[], ratio)
    }

    /// Put the root's two panes on each other's side, so that each keeps the columns (rows) it
    /// has in `area`: the ratio is set from whole cells, the way a drag sets it, so a pane never
    /// loses a cell to rounding and swapping twice is the identity. False unless the root is a
    /// split of two panes.
    pub fn swap_root(&mut self, area: Rect) -> bool {
        let inner = self.inner(area);
        let Node::Split {
            dir,
            ratio,
            first,
            second,
        } = &mut self.root
        else {
            return false;
        };
        let Some((_, far)) = halves(*dir, *ratio, inner) else {
            return false;
        };
        let (far, total) = match *dir {
            Dir::V => (far.cols, inner.cols - 1),
            Dir::H => (far.rows, inner.rows - 1),
        };
        std::mem::swap(first, second);
        // Half a cell past the edge, so the floor in `halves` lands on the cell itself.
        *ratio = (far as f32 + 0.5) / total as f32;
        true
    }

    /// Visible terminals: zoomed alone when zoom is on, else all leaves.
    pub fn visible(&self) -> Vec<u32> {
        if let Some(z) = self.zoomed
            && self.contains(z)
        {
            return vec![z];
        }
        self.leaves()
    }

    /// Add a placeholder split of `target`. Focus does not move until the new
    /// terminal is bound. Returns false when there is no room or `target` is
    /// not a leaf, or a placeholder is already pending.
    pub fn split_placeholder(&mut self, target: u32, dir: Dir, area: Rect) -> bool {
        if has_placeholder(&self.root) {
            return false;
        }
        if !self.contains(target) {
            return false;
        }
        if self.zoomed.is_some() {
            return false;
        }
        // Measured inside the frame the split is about to bring, so a pane that fits now
        // still fits once the frame takes its cell from every side.
        let mut rects = Vec::new();
        layout_node(&self.root, framed(area), &mut rects);
        let Some(rect) = rects
            .into_iter()
            .find(|(t, _)| *t == target)
            .map(|(_, r)| r)
        else {
            return false;
        };
        if !fits(rect, dir) {
            return false;
        }
        if !split_node(&mut self.root, target, dir) {
            return false;
        }
        true
    }

    /// Put `terminal`, which is in no pane of this tree, beside `target` on the side
    /// `side` names, with the keyboard in it. What a row dropped on a pane's edge does.
    /// Refuses what `split_placeholder` refuses: no room, no such target, a zoom, and
    /// a terminal already here.
    pub fn insert_beside(&mut self, target: u32, terminal: u32, side: Move, area: Rect) -> bool {
        let dir = match side {
            Move::Left | Move::Right => Dir::V,
            Move::Up | Move::Down => Dir::H,
        };
        if self.contains(terminal) || !self.split_placeholder(target, dir, area) {
            return false;
        }
        bind_first_placeholder(&mut self.root, terminal);
        if matches!(side, Move::Left | Move::Up) {
            put_first(&mut self.root, terminal);
        }
        self.zoomed = None;
        self.set_focus(terminal);
        true
    }

    /// Bind the pending placeholder to a real terminal. Refuses a duplicate
    /// id and a bind with no pending split beside a live leaf: both would
    /// leave two panes answering to one terminal.
    pub fn bind_placeholder(&mut self, terminal: u32) -> bool {
        if self.contains(terminal) || !self.has_pending() {
            return false;
        }
        if !bind_first_placeholder(&mut self.root, terminal) {
            return false;
        }
        self.zoomed = None;
        self.set_focus(terminal);
        true
    }

    /// Drop the pending placeholder after a failed spawn. Focus is untouched.
    /// Reports whether a placeholder was actually removed: a bare empty root
    /// is not a rollback.
    pub fn drop_placeholder(&mut self) -> bool {
        remove_first_placeholder(&mut self.root)
    }

    /// Remove a terminal's leaf, collapsing the parent split. Returns true
    /// when no leaves remain. The root goes bare, not to a phantom pending
    /// placeholder: an empty tree is empty.
    ///
    /// Focus after a close: `prev_focus` when it survives, else the first
    /// leaf in tree order. Tree order, not geometry: geometry needs rects this
    /// type does not carry, and the caller with a screen should route through
    /// `neighbour_in_dir`.
    pub fn remove(&mut self, terminal: u32) -> bool {
        remove_terminal(&mut self.root, terminal);
        if self.zoomed == Some(terminal) {
            self.zoomed = None;
        }
        if self.prev_focus == Some(terminal) {
            self.prev_focus = None;
        }
        let leaves = self.leaves();
        if leaves.is_empty() {
            return true;
        }
        if self.focus == terminal || !self.contains(self.focus) {
            let next = self
                .prev_focus
                .filter(|p| leaves.contains(p))
                .unwrap_or(leaves[0]);
            self.prev_focus = None;
            self.focus = next;
        }
        false
    }

    /// Cycle focus through the visible terminals in tree order.
    ///
    /// Tree order is the only order the tree knows. It is not the order on
    /// screen with three or more panes: a cycle visits every pane exactly
    /// once and wraps, where you land is just not left-to-right.
    pub fn focus_next(&mut self) -> bool {
        let visible = self.visible();
        if visible.len() < 2 {
            return false;
        }
        let at = visible.iter().position(|t| *t == self.focus);
        let next = match at {
            Some(at) => visible[(at + 1) % visible.len()],
            None => visible[0],
        };
        self.set_focus(next);
        true
    }

    /// Directional focus across the visible tiles.
    pub fn focus_move(&mut self, area: Rect, mv: Move) -> bool {
        let rects = self.tile_rects(area);
        let visible = self.visible();
        let rects: Vec<(u32, Rect)> = rects
            .into_iter()
            .filter(|(t, _)| visible.contains(t))
            .collect();
        if let Some(next) = neighbour_in_dir(&rects, self.focus, mv) {
            self.set_focus(next);
            return true;
        }
        false
    }

    /// Toggle zoom on the focused terminal. No-op with a single leaf.
    ///
    /// Zoom follows the focus: toggling while
    /// focused elsewhere moves the zoom to the focused terminal rather than
    /// closing it. `focus_next`/`focus_move` stay blocked while zoomed, but a
    /// direct `focus_terminal` (tab bar, click) moves the keyboard under the
    /// zoom; the next toggle zooms the newly focused terminal.
    pub fn toggle_zoom(&mut self) -> bool {
        if self.leaf_count() < 2 {
            return false;
        }
        if self.zoomed == Some(self.focus) {
            self.zoomed = None;
        } else {
            self.zoomed = Some(self.focus);
        }
        true
    }

    /// Tile rectangles for the visible terminals within `area`.
    pub fn tile_rects(&self, area: Rect) -> Vec<(u32, Rect)> {
        if let Some(z) = self.zoomed
            && self.contains(z)
        {
            return vec![(z, area)];
        }
        if let Some(folded) = self.folded(area) {
            return folded;
        }
        let mut out = Vec::new();
        layout_node(&self.root, self.inner(area), &mut out);
        // The gap comes out of one pane only, after the split is measured: the first pane's
        // right edge, or the second pane's left edge.
        if self.gap > 0
            && let Node::Split { first, second, .. } = &self.root
            && let Node::Leaf {
                terminal: Some(cut),
            } = **(if self.gap_second { second } else { first })
            && let Some((_, rect)) = out.iter_mut().find(|(t, _)| *t == cut)
        {
            let cut = self.gap.min(rect.cols);
            rect.cols -= cut;
            if self.gap_second {
                rect.x += cut;
            }
        }
        self.cut_rail(&mut out);
        out
    }

    /// The rail's columns taken from the board, so qass keeps its columns whichever side it is on:
    /// off the left edge of the first pane when that is the board, else (the board second, after
    /// the gap was cut from its left edge) both panes move right and the second gives the
    /// columns up. Both panes give them when stacked.
    fn cut_rail(&self, out: &mut [(u32, Rect)]) {
        if self.rail == 0 {
            return;
        }
        let Node::Split {
            dir, first, second, ..
        } = &self.root
        else {
            return;
        };
        let leaf = |node: &Node| match node {
            Node::Leaf { terminal } => *terminal,
            _ => None,
        };
        let (first, second) = (leaf(first), leaf(second));
        for (t, rect) in out.iter_mut() {
            let cut = self.rail.min(rect.cols.saturating_sub(1));
            let is_first = Some(*t) == first;
            let is_second = Some(*t) == second;
            match dir {
                Dir::H if is_first || is_second => {
                    rect.x += cut;
                    rect.cols -= cut;
                }
                Dir::V if is_first && !self.gap_second => {
                    rect.x += cut;
                    rect.cols -= cut;
                }
                Dir::V if self.gap_second && is_first => rect.x += self.rail,
                Dir::V if self.gap_second && is_second => {
                    rect.x += self.rail;
                    rect.cols = rect.cols.saturating_sub(self.rail).max(1);
                }
                _ => {}
            }
        }
    }

    /// The tiles while a root pane is folded ([`Layout::fold`]): the folded pane no columns wide
    /// against its own edge, the open pane the rest less the rail and the gap on the edge that
    /// faces the folded one. `None` without a fold on a root of two panes.
    fn folded(&self, area: Rect) -> Option<Vec<(u32, Rect)>> {
        let fold = self.fold?;
        let Node::Split { first, second, .. } = &self.root else {
            return None;
        };
        let (Node::Leaf { terminal: Some(a) }, Node::Leaf { terminal: Some(b) }) =
            (&**first, &**second)
        else {
            return None;
        };
        let inner = self.inner(area);
        if inner.cols < 3 + self.rail + self.gap || (fold != *a && fold != *b) {
            return None;
        }
        let open_cols = inner.cols - self.rail - self.gap;
        Some(if fold == *a {
            let shut = Rect {
                x: inner.x + self.rail,
                cols: 0,
                ..inner
            };
            let open = Rect {
                x: inner.x + self.rail + self.gap,
                cols: open_cols,
                ..inner
            };
            vec![(*a, shut), (*b, open)]
        } else {
            let open = Rect {
                x: inner.x + self.rail,
                cols: open_cols,
                ..inner
            };
            let shut = Rect {
                x: inner.x + inner.cols,
                cols: 0,
                ..inner
            };
            vec![(*a, open), (*b, shut)]
        })
    }

    /// The part of `area` the panes share once a frame is drawn around them: one cell in
    /// from every side while a split is on screen, all of it for one pane or a zoom.
    ///
    /// The one place the frame is taken off, so sizing, painting, clicks and drags all
    /// measure the same tiles.
    pub fn inner(&self, area: Rect) -> Rect {
        if self.zoomed.is_some() || !matches!(self.root, Node::Split { .. }) {
            return area;
        }
        framed(area)
    }

    /// Move the divider nearest the focused pane on the move's axis, by `step` of its
    /// split, the way the key points.
    ///
    /// The nearest ancestor split on that axis, whichever side of it the pane is on: a
    /// pane on the right of a divider still moves it right with `l`. False when there is no
    /// such divider, it is already at its limit, or moving it would squeeze a pane below
    /// the minimum size.
    pub fn nudge(&mut self, area: Rect, mv: Move, step: f32) -> bool {
        if self.zoomed.is_some() {
            return false;
        }
        let area = self.inner(area);
        // Left and right move a side by side divider, up and down a stacked one, each the
        // way the key points (tmux's rule): which side of it the pane is on does not matter.
        let (dir, sign) = match mv {
            Move::Left => (Dir::V, -1.0),
            Move::Right => (Dir::V, 1.0),
            Move::Up => (Dir::H, -1.0),
            Move::Down => (Dir::H, 1.0),
        };
        let Some(path) = ancestor_on(&self.root, self.focus, dir, &mut Vec::new()) else {
            return false;
        };
        let Some((_, ratio, _)) = split_at(&self.root, &path, area) else {
            return false;
        };
        self.set_ratio(area, &path, ratio + sign * step)
    }

    /// Give every pane the same share of its row or column.
    ///
    /// A split's ratio is how many panes its first side lines up along the split's axis,
    /// against the whole: `[A | (B over C)]` is half and half, since the stacked pair
    /// is one column, and `[A | B | C]` is thirds. Answers whether anything moved.
    pub fn equalize(&mut self) -> bool {
        let before = self.root.clone();
        equalize_node(&mut self.root);
        self.root != before
    }

    /// The divider under a pane-local cell, as the path to its split. `None` off a
    /// divider, and while zoomed, when no divider is drawn.
    pub fn divider_at(&self, area: Rect, col: usize, row: usize) -> Option<SplitPath> {
        if self.zoomed.is_some() {
            return None;
        }
        let mut path = Vec::new();
        let col = col.checked_sub(self.rail_shift())?;
        divider_in(&self.root, self.inner(area), col, row, &mut path).then_some(path)
    }

    /// How far the rail moves the whole side by side split right: with the board second, qass
    /// and the board both stand that much right of where the split measures them.
    pub fn rail_shift(&self) -> usize {
        match &self.root {
            Node::Split { dir: Dir::V, .. } if self.gap_second => self.rail,
            _ => 0,
        }
    }

    /// The two root panes' columns before the gap is cut, side by side only.
    pub fn root_cols(&self, area: Rect) -> Option<(usize, usize)> {
        let Node::Split {
            dir: Dir::V, ratio, ..
        } = &self.root
        else {
            return None;
        };
        let (first, second) = halves(Dir::V, *ratio, self.inner(area))?;
        Some((first.cols, second.cols))
    }

    /// The questions' own edge, the board's frame edge beside them: its column and the rows it
    /// runs over, pane-local. `None` without a gap, and while zoomed, when nothing is drawn.
    pub fn questions_edge(&self, area: Rect) -> Option<(usize, usize, usize)> {
        let Node::Split {
            dir: Dir::V, ratio, ..
        } = &self.root
        else {
            return None;
        };
        if self.gap == 0 || self.zoomed.is_some() || self.fold.is_some() {
            return None;
        }
        let (first, second) = halves(Dir::V, *ratio, self.inner(area))?;
        let x = if self.gap_second {
            second.x + self.gap - 1
        } else {
            first.x + first.cols - self.gap
        };
        Some((x, first.y, first.rows))
    }

    /// Whether a pane-local cell is on the questions' edge.
    pub fn on_questions_edge(&self, area: Rect, col: usize, row: usize) -> bool {
        self.questions_edge(area)
            .is_some_and(|(x, y, rows)| col == x && row >= y && row < y + rows)
    }

    /// The gap (the edge column and the questions) that puts the questions' edge on `col`.
    pub fn gap_at(&self, area: Rect, col: usize) -> Option<usize> {
        let Node::Split {
            dir: Dir::V, ratio, ..
        } = &self.root
        else {
            return None;
        };
        let (first, second) = halves(Dir::V, *ratio, self.inner(area))?;
        if self.gap_second {
            (col + 1).checked_sub(second.x)
        } else {
            (first.x + first.cols).checked_sub(col)
        }
    }

    /// Put the divider of the split at `path` under a pane-local cell. Answers whether it
    /// moved.
    ///
    /// A cell past what the limits allow is refused whole and the divider stays where it
    /// was, rather than being pulled part of the way: a terminal reports the pointer cell by
    /// cell, so a drag toward the limit has already put the divider on the last allowed cell
    /// by the time the pointer passes it.
    pub fn drag(&mut self, area: Rect, path: &[bool], col: usize, row: usize) -> bool {
        let col = col.saturating_sub(self.rail_shift());
        let area = self.inner(area);
        let Some((dir, _, rect)) = split_at(&self.root, path, area) else {
            return false;
        };
        let (near, total) = match dir {
            Dir::V => (col.saturating_sub(rect.x), rect.cols.saturating_sub(1)),
            Dir::H => (row.saturating_sub(rect.y), rect.rows.saturating_sub(1)),
        };
        if total == 0 {
            return false;
        }
        // Half a cell past the edge, so the floor in `halves` lands on the cell itself.
        self.set_ratio(area, path, (near as f32 + 0.5) / total as f32)
    }

    /// Set one split's ratio, refusing a move that squeezes a pane below the minimum or
    /// leaves one with no room to be drawn at all. A pane already under the minimum, in a
    /// small window, may still grow.
    fn set_ratio(&mut self, area: Rect, path: &[bool], ratio: f32) -> bool {
        let ratio = ratio.clamp(0.1, 0.9);
        let mut before = Vec::new();
        layout_node(&self.root, area, &mut before);
        let Some(slot) = ratio_at(&mut self.root, path) else {
            return false;
        };
        if (*slot - ratio).abs() < f32::EPSILON {
            return false;
        }
        let was = std::mem::replace(slot, ratio);
        let mut after = Vec::new();
        layout_node(&self.root, area, &mut after);
        // A pane squeezed out of the drawing entirely is the worst squeeze of all: a
        // focused one would keep the keyboard while nothing of it is on screen.
        let refused = before.iter().any(|(terminal, then)| {
            after
                .iter()
                .find(|(t, _)| t == terminal)
                .is_none_or(|(_, now)| {
                    (now.cols < MIN_COLS && now.cols < then.cols)
                        || (now.rows < MIN_ROWS && now.rows < then.rows)
                })
        });
        if refused && let Some(slot) = ratio_at(&mut self.root, path) {
            *slot = was;
        }
        !refused
    }

    /// Which visible terminal covers a pane-local `(col, row)`. Divider cells
    /// and placeholders answer `None`.
    pub fn hit(&self, area: Rect, col: usize, row: usize) -> Option<u32> {
        self.tile_rects(area)
            .into_iter()
            .find(|(_, r)| col >= r.x && col < r.x + r.cols && row >= r.y && row < r.y + r.rows)
            .map(|(t, _)| t)
    }
}

fn to_pane(node: &Node) -> Option<Pane> {
    match node {
        Node::Leaf { terminal } => terminal.map(|terminal| Pane::Leaf { terminal }),
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => Some(Pane::Split {
            dir: match dir {
                Dir::V => SplitDir::V,
                Dir::H => SplitDir::H,
            },
            permille: (ratio.clamp(0.0, 1.0) * 1000.0).round() as u16,
            questions: 0,
            first: Box::new(to_pane(first)?),
            second: Box::new(to_pane(second)?),
        }),
    }
}

fn from_pane(pane: &Pane) -> Node {
    match pane {
        Pane::Leaf { terminal } => Node::Leaf {
            terminal: Some(*terminal),
        },
        Pane::Split {
            dir,
            permille,
            first,
            second,
            ..
        } => Node::Split {
            dir: match dir {
                SplitDir::V => Dir::V,
                SplitDir::H => Dir::H,
            },
            ratio: f32::from((*permille).min(1000)) / 1000.0,
            first: Box::new(from_pane(first)),
            second: Box::new(from_pane(second)),
        },
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<u32>) {
    match node {
        Node::Leaf { terminal } => {
            if let Some(t) = terminal {
                out.push(*t);
            }
        }
        Node::Split { first, second, .. } => {
            collect_leaves(first, out);
            collect_leaves(second, out);
        }
    }
}

fn has_placeholder(node: &Node) -> bool {
    match node {
        Node::Leaf { terminal } => terminal.is_none(),
        Node::Split { first, second, .. } => has_placeholder(first) || has_placeholder(second),
    }
}

/// A placeholder with a live leaf beside it: the only shape that counts as a
/// split on its way. A bare empty root is an empty tree, and treating it as
/// pending let an empty layout block every later split and resurrect itself
/// on bind.
fn has_live_sibling(node: &Node) -> bool {
    match node {
        Node::Leaf { .. } => false,
        Node::Split { first, second, .. } => {
            (is_empty_leaf(first) && has_live_leaf(second))
                || (is_empty_leaf(second) && has_live_leaf(first))
                || has_live_sibling(first)
                || has_live_sibling(second)
        }
    }
}

fn has_live_leaf(node: &Node) -> bool {
    match node {
        Node::Leaf { terminal } => terminal.is_some(),
        Node::Split { first, second, .. } => has_live_leaf(first) || has_live_leaf(second),
    }
}

fn split_node(node: &mut Node, target: u32, dir: Dir) -> bool {
    match node {
        Node::Leaf { terminal } => {
            if *terminal == Some(target) {
                *node = Node::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf {
                        terminal: Some(target),
                    }),
                    second: Box::new(Node::Leaf { terminal: None }),
                };
                return true;
            }
            false
        }
        Node::Split { first, second, .. } => {
            split_node(first, target, dir) || split_node(second, target, dir)
        }
    }
}

/// Swap the two sides of the split whose second side is `terminal`'s own leaf.
fn put_first(node: &mut Node, terminal: u32) -> bool {
    let Node::Split { first, second, .. } = node else {
        return false;
    };
    if matches!(**second, Node::Leaf { terminal: Some(t) } if t == terminal) {
        std::mem::swap(first, second);
        return true;
    }
    put_first(first, terminal) || put_first(second, terminal)
}

fn bind_first_placeholder(node: &mut Node, terminal: u32) -> bool {
    match node {
        Node::Leaf { terminal: slot } => {
            if slot.is_none() {
                *slot = Some(terminal);
                return true;
            }
            false
        }
        Node::Split { first, second, .. } => {
            bind_first_placeholder(first, terminal) || bind_first_placeholder(second, terminal)
        }
    }
}

fn remove_first_placeholder(node: &mut Node) -> bool {
    match node {
        // A bare root is an empty tree, not a rollback: claiming success here
        // kept empty layouts alive and blocked every later split.
        Node::Leaf { .. } => false,
        Node::Split { first, second, .. } => {
            if matches!(first.as_ref(), Node::Leaf { terminal: None }) {
                *node = (**second).clone();
                return true;
            }
            if matches!(second.as_ref(), Node::Leaf { terminal: None }) {
                *node = (**first).clone();
                return true;
            }
            remove_first_placeholder(first) || remove_first_placeholder(second)
        }
    }
}

/// True when `terminal` was in this subtree. Only splits on that path
/// collapse: a pending placeholder anywhere else looks exactly like a removed
/// slot, and collapsing it would kill a split still waiting for its terminal.
fn remove_terminal(node: &mut Node, terminal: u32) -> bool {
    match node {
        // A single bare leaf removes to a bare root only when it is the one
        // being removed; a placeholder beside a dying leaf dies with it, or
        // every failed split resurrects as a pending zombie (see below).
        Node::Leaf { terminal: slot } => {
            let hit = *slot == Some(terminal);
            if hit {
                *slot = None;
            }
            hit
        }
        Node::Split { first, second, .. } => {
            let removed = remove_terminal(first, terminal) | remove_terminal(second, terminal);
            if !removed {
                return false;
            }
            let first_dead = matches!(first.as_ref(), Node::Leaf { terminal: None });
            let second_dead = matches!(second.as_ref(), Node::Leaf { terminal: None });
            if first_dead && second_dead {
                // Both sides empty: one of them was the removed terminal and
                // the other its stillborn split. Keep one bare marker.
                *node = Node::Leaf { terminal: None };
            } else if first_dead {
                *node = (**second).clone();
            } else if second_dead {
                *node = (**first).clone();
            }
            true
        }
    }
}

fn is_empty_leaf(node: &Node) -> bool {
    matches!(node, Node::Leaf { terminal: None })
}

fn fits(rect: Rect, dir: Dir) -> bool {
    match dir {
        Dir::V => rect.cols > MIN_COLS * 2 && rect.rows >= MIN_ROWS,
        Dir::H => rect.rows > MIN_ROWS * 2 && rect.cols >= MIN_COLS,
    }
}

fn layout_node(node: &Node, area: Rect, out: &mut Vec<(u32, Rect)>) {
    match node {
        Node::Leaf { terminal } => {
            if let Some(t) = terminal {
                out.push((*t, area));
            }
        }
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            if let Some((a, b)) = halves(*dir, *ratio, area) {
                layout_node(first, a, out);
                layout_node(second, b, out);
            }
        }
    }
}

/// `area` one cell in from every side, for the frame around split panes. An area too small
/// to give up the cells is left whole.
fn framed(area: Rect) -> Rect {
    if area.cols < 3 || area.rows < 3 {
        return area;
    }
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        cols: area.cols - 2,
        rows: area.rows - 2,
    }
}

/// The two sides of a split of `area`, with the one cell divider between them left out.
///
/// `None` when a shrunken window leaves less than two cells: clamped to a real rectangle
/// inside `area` rather than spilling past it. `fits` guards the split moment; this guards
/// every repaint after.
fn halves(dir: Dir, ratio: f32, area: Rect) -> Option<(Rect, Rect)> {
    let ratio = ratio.clamp(0.1, 0.9);
    let (total, across) = match dir {
        Dir::V => (area.cols.saturating_sub(1), area.rows),
        Dir::H => (area.rows.saturating_sub(1), area.cols),
    };
    if total < 2 || across == 0 {
        return None;
    }
    let near = ((total as f32 * ratio).floor() as usize)
        .max(1)
        .min(total.saturating_sub(1).max(1));
    let far = total.saturating_sub(near);
    Some(match dir {
        Dir::V => (
            Rect { cols: near, ..area },
            Rect {
                x: area.x + near + 1,
                cols: far,
                ..area
            },
        ),
        Dir::H => (
            Rect { rows: near, ..area },
            Rect {
                y: area.y + near + 1,
                rows: far,
                ..area
            },
        ),
    })
}

/// How many panes a subtree puts side by side along `dir`: a split the other way is one
/// block, however many panes it holds.
fn weight(node: &Node, dir: Dir) -> usize {
    match node {
        Node::Split {
            dir: d,
            first,
            second,
            ..
        } if *d == dir => weight(first, dir) + weight(second, dir),
        _ => 1,
    }
}

fn equalize_node(node: &mut Node) {
    if let Node::Split {
        dir,
        ratio,
        first,
        second,
    } = node
    {
        let near = weight(first, *dir);
        *ratio = near as f32 / (near + weight(second, *dir)) as f32;
        equalize_node(first);
        equalize_node(second);
    }
}

/// The split at `path`: its direction, its ratio, and the area it divides.
fn split_at(node: &Node, path: &[bool], area: Rect) -> Option<(Dir, f32, Rect)> {
    let Node::Split {
        dir,
        ratio,
        first,
        second,
    } = node
    else {
        return None;
    };
    let Some((&side, rest)) = path.split_first() else {
        return Some((*dir, *ratio, area));
    };
    let (a, b) = halves(*dir, *ratio, area)?;
    if side {
        split_at(second, rest, b)
    } else {
        split_at(first, rest, a)
    }
}

/// The ratio of the split at `path`, to change in place.
fn ratio_at<'a>(node: &'a mut Node, path: &[bool]) -> Option<&'a mut f32> {
    let Node::Split {
        ratio,
        first,
        second,
        ..
    } = node
    else {
        return None;
    };
    match path.split_first() {
        None => Some(ratio),
        Some((true, rest)) => ratio_at(second, rest),
        Some((false, rest)) => ratio_at(first, rest),
    }
}

/// The path to the nearest split above `terminal` that divides along `dir`.
fn ancestor_on(node: &Node, terminal: u32, dir: Dir, path: &mut Vec<bool>) -> Option<Vec<bool>> {
    let Node::Split {
        dir: d,
        first,
        second,
        ..
    } = node
    else {
        return None;
    };
    for (side, child) in [(false, first), (true, second)] {
        let mut held = Vec::new();
        collect_leaves(child, &mut held);
        if held.contains(&terminal) {
            path.push(side);
            let deeper = ancestor_on(child, terminal, dir, path);
            path.pop();
            return deeper.or_else(|| (*d == dir).then(|| path.clone()));
        }
    }
    None
}

/// The divider under a pane-local cell, as the path to its split.
fn divider_in(node: &Node, area: Rect, col: usize, row: usize, path: &mut Vec<bool>) -> bool {
    let Node::Split {
        dir,
        ratio,
        first,
        second,
    } = node
    else {
        return false;
    };
    let Some((a, b)) = halves(*dir, *ratio, area) else {
        return false;
    };
    let on_divider = match dir {
        Dir::V => col == a.x + a.cols && row >= area.y && row < area.y + area.rows,
        Dir::H => row == a.y + a.rows && col >= area.x && col < area.x + area.cols,
    };
    if on_divider {
        return true;
    }
    for (side, child, rect) in [(false, first, a), (true, second, b)] {
        if col >= rect.x && col < rect.x + rect.cols && row >= rect.y && row < rect.y + rect.rows {
            path.push(side);
            if divider_in(child, rect, col, row, path) {
                return true;
            }
            path.pop();
        }
    }
    false
}

/// Geometric neighbour of `focus` in direction `mv`, preferring overlap then
/// edge distance then center.
///
/// Ties break on the lower terminal id, not on screen order: deterministic,
/// but not left-to-right with three or more panes. Same caveat as
/// `focus_next`: the cycle visits everything, the landing order is the tree's.
pub fn neighbour_in_dir(rects: &[(u32, Rect)], focus: u32, mv: Move) -> Option<u32> {
    let current = rects.iter().find(|(t, _)| *t == focus)?.1;
    let mut best: Option<(u32, i64, i64, i64)> = None;
    for (t, r) in rects {
        if *t == focus {
            continue;
        }
        let in_dir = match mv {
            Move::Left => r.x + r.cols <= current.x,
            Move::Right => r.x >= current.x + current.cols,
            Move::Up => r.y + r.rows <= current.y,
            Move::Down => r.y >= current.y + current.rows,
        };
        if !in_dir {
            continue;
        }
        let (overlap, gap) = match mv {
            Move::Left | Move::Right => {
                let top = current.y.max(r.y);
                let bottom = (current.y + current.rows).min(r.y + r.rows);
                let overlap = bottom.saturating_sub(top) as i64;
                let gap = match mv {
                    Move::Left => current.x as i64 - (r.x + r.cols) as i64,
                    _ => r.x as i64 - (current.x + current.cols) as i64,
                };
                (overlap, gap)
            }
            Move::Up | Move::Down => {
                let left = current.x.max(r.x);
                let right = (current.x + current.cols).min(r.x + r.cols);
                let overlap = right.saturating_sub(left) as i64;
                let gap = match mv {
                    Move::Up => current.y as i64 - (r.y + r.rows) as i64,
                    _ => r.y as i64 - (current.y + current.rows) as i64,
                };
                (overlap, gap)
            }
        };
        // Higher overlap wins, then smaller gap, then smaller terminal id.
        let score = (overlap, -gap, -(*t as i64));
        if best.is_none_or(|(_, bo, bg, bt)| {
            let (best_o, best_g, best_t) = (bo, bg, bt);
            (score.0, score.1, score.2) > (best_o, best_g, best_t)
        }) {
            best = Some((*t, overlap, gap, *t as i64));
        }
    }
    best.map(|(t, _, _, _)| t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(cols: usize, rows: usize) -> Rect {
        Rect {
            x: 0,
            y: 0,
            cols,
            rows,
        }
    }

    /// A row dropped on a pane's edge lands on that side of it, with the keyboard.
    #[test]
    fn a_dropped_terminal_lands_on_the_side_it_was_dropped_on() {
        let a = area(80, 24);
        let sides = [
            (Move::Left, Dir::V, vec![2, 1]),
            (Move::Right, Dir::V, vec![1, 2]),
            (Move::Up, Dir::H, vec![2, 1]),
            (Move::Down, Dir::H, vec![1, 2]),
        ];
        for (side, dir, order) in sides {
            let mut layout = Layout::single(1);
            assert!(layout.insert_beside(1, 2, side, a), "{side:?}");
            assert_eq!(layout.leaves(), order, "{side:?}");
            assert_eq!(layout.focus, 2, "{side:?}");
            assert!(!layout.has_pending(), "{side:?}");
            assert!(matches!(layout.root, Node::Split { dir: d, .. } if d == dir));
        }
        // Into the middle of a tree: beside its own target, the rest untouched.
        let mut layout = Layout::single(1);
        assert!(layout.insert_beside(1, 2, Move::Right, a));
        assert!(layout.insert_beside(1, 3, Move::Up, a));
        assert_eq!(layout.leaves(), vec![3, 1, 2]);
        // Twice, no target, no room: refused and nothing changed.
        let before = layout.clone();
        assert!(!layout.insert_beside(1, 3, Move::Left, a));
        assert!(!layout.insert_beside(9, 4, Move::Left, a));
        assert!(!layout.insert_beside(1, 4, Move::Left, area(10, 4)));
        assert_eq!(layout, before);
    }

    #[test]
    fn a_tree_goes_to_the_daemon_and_comes_back_the_same() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert_eq!(
            layout.to_pane(),
            None,
            "a pending split has nothing to name"
        );
        assert!(layout.bind_placeholder(2));
        assert!(layout.split_placeholder(2, Dir::H, area(80, 24)));
        assert!(layout.bind_placeholder(3));
        let pane = layout.to_pane().expect("a whole tree");
        let back = Layout::from_pane(&pane);
        assert_eq!(back.root, layout.root);
        assert_eq!(back.leaves(), vec![1, 2, 3]);
        assert_eq!(back.focus, 1, "the keyboard lands on the tab");
        assert_eq!(
            back.tile_rects(area(80, 24)),
            layout.tile_rects(area(80, 24))
        );
    }

    #[test]
    fn a_single_terminal_fills_the_area() {
        let layout = Layout::single(1);
        assert_eq!(layout.tile_rects(area(80, 24)), vec![(1, area(80, 24))]);
    }

    #[test]
    fn a_vertical_split_puts_two_panes_side_by_side_with_one_divider() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        let rects = layout.tile_rects(area(80, 24));
        assert_eq!(rects.len(), 2);
        // Two panes, one divider, and the frame's column on each side.
        let total: usize = rects.iter().map(|(_, r)| r.cols).sum::<usize>() + 1 + 2;
        assert_eq!(total, 80);
        assert!(rects.iter().all(|(_, r)| r.rows == 22));
    }

    #[test]
    fn a_horizontal_split_stacks_two_panes_with_one_divider() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::H, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        let rects = layout.tile_rects(area(80, 24));
        assert_eq!(rects.len(), 2);
        let total: usize = rects.iter().map(|(_, r)| r.rows).sum::<usize>() + 1 + 2;
        assert_eq!(total, 24);
        assert!(rects.iter().all(|(_, r)| r.cols == 78));
    }

    /// The frame is there only while panes share the area: one pane and a zoom fill it.
    #[test]
    fn a_frame_surrounds_split_panes_and_nothing_else() {
        let mut layout = Layout::single(1);
        assert_eq!(layout.inner(area(80, 24)), area(80, 24));
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        let inner = layout.inner(area(80, 24));
        assert_eq!((inner.x, inner.y, inner.cols, inner.rows), (1, 1, 78, 22));
        assert!(
            layout
                .tile_rects(area(80, 24))
                .iter()
                .all(|(_, r)| r.x >= 1 && r.y == 1)
        );
        layout.toggle_zoom();
        assert_eq!(layout.tile_rects(area(80, 24)), vec![(2, area(80, 24))]);
    }

    #[test]
    fn a_split_too_small_is_refused() {
        let mut layout = Layout::single(1);
        assert!(!layout.split_placeholder(1, Dir::V, area(10, 24)));
        assert!(!layout.split_placeholder(1, Dir::H, area(80, 5)));
        assert_eq!(layout.leaf_count(), 1);
    }

    #[test]
    fn a_second_split_while_one_is_pending_is_refused() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(!layout.split_placeholder(1, Dir::H, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        assert!(layout.split_placeholder(1, Dir::H, area(80, 24)));
    }

    #[test]
    fn a_failed_spawn_rolls_back_without_moving_focus() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.drop_placeholder());
        assert_eq!(layout.leaf_count(), 1);
        assert_eq!(layout.focus, 1);
    }

    #[test]
    fn binding_moves_focus_and_clears_zoom() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        assert_eq!(layout.focus, 2);
        assert_eq!(layout.prev_focus, Some(1));
    }

    #[test]
    fn a_duplicate_bind_and_a_bind_with_no_pending_split_are_refused() {
        let mut layout = Layout::single(1);
        assert!(!layout.bind_placeholder(1));
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(!layout.bind_placeholder(1));
        assert!(layout.bind_placeholder(2));
        assert!(!layout.bind_placeholder(3));
    }

    #[test]
    fn an_empty_tree_is_empty_not_pending() {
        let mut layout = Layout::single(1);
        assert!(layout.remove(1));
        assert!(layout.is_empty());
        assert!(!layout.has_pending());
        assert!(!layout.drop_placeholder());
        assert!(!layout.bind_placeholder(9));
    }

    #[test]
    fn removing_a_leaf_beside_a_placeholder_kills_both_sides() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.remove(1));
        assert!(layout.is_empty());
        assert!(!layout.has_pending());
    }

    #[test]
    fn closing_a_terminal_elsewhere_keeps_a_pending_split() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(!layout.remove(9));
        assert!(layout.has_pending());
        assert!(layout.bind_placeholder(2));
    }

    #[test]
    fn directional_focus_moves_across_a_split() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        // Tree order: 1 left, 2 right.
        assert!(layout.focus_move(area(80, 24), Move::Left));
        assert_eq!(layout.focus, 1);
        assert!(!layout.focus_move(area(80, 24), Move::Left));
        assert!(layout.focus_move(area(80, 24), Move::Right));
        assert_eq!(layout.focus, 2);
    }

    #[test]
    fn cycling_wraps_through_visible_panes() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        assert!(layout.focus_next());
        assert_eq!(layout.focus, 1);
        assert!(layout.focus_next());
        assert_eq!(layout.focus, 2);
    }

    #[test]
    fn zoom_shows_only_the_focused_terminal() {
        let mut layout = Layout::single(1);
        assert!(!layout.toggle_zoom());
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        assert!(layout.toggle_zoom());
        assert_eq!(layout.visible(), vec![2]);
        assert_eq!(layout.tile_rects(area(80, 24)), vec![(2, area(80, 24))]);
        assert!(layout.toggle_zoom());
        assert_eq!(layout.visible().len(), 2);
    }

    #[test]
    fn closing_returns_focus_to_the_previous_terminal_when_it_survives() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        // Focus is 2, previous is 1. Closing 2 returns to 1.
        assert!(!layout.remove(2));
        assert_eq!(layout.focus, 1);
        assert_eq!(layout.visible(), vec![1]);
    }

    #[test]
    fn closing_the_last_leaf_reports_empty() {
        let mut layout = Layout::single(1);
        assert!(layout.remove(1));
    }

    #[test]
    fn divider_cells_belong_to_no_pane() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        let rects = layout.tile_rects(area(80, 24));
        let left = rects.iter().find(|(t, _)| *t == 1).unwrap().1;
        // The column right after the left pane is the divider, and the outer edge is frame.
        assert_eq!(layout.hit(area(80, 24), left.x + left.cols, 1), None);
        assert_eq!(layout.hit(area(80, 24), 0, 0), None);
        assert_eq!(layout.hit(area(80, 24), 1, 1), Some(1));
    }

    /// `[A | (B over C)]`: 1 on the left, 2 above 3 on the right, 3 focused.
    fn three() -> Layout {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        assert!(layout.split_placeholder(2, Dir::H, area(80, 24)));
        assert!(layout.bind_placeholder(3));
        layout
    }

    fn rect_of(layout: &Layout, terminal: u32) -> Rect {
        layout
            .tile_rects(area(80, 24))
            .into_iter()
            .find(|(t, _)| *t == terminal)
            .expect("a tile")
            .1
    }

    /// A folded pane is a tile no columns wide at its own edge, the open pane takes the rest less
    /// the rail (on the left) and the gap (on the edge that faces the folded one).
    #[test]
    fn a_folded_pane_is_no_columns_and_the_rail_and_the_gap_come_off_the_open_pane() {
        for width in [61, 121, 161] {
            let a = area(width, 30);
            for dir in [Dir::V, Dir::H] {
                let mut layout = Layout::single(1);
                assert!(layout.split_placeholder(1, Dir::V, area(200, 30)));
                assert!(layout.bind_placeholder(2));
                layout.set_root_split(dir, 0.7);
                let saved = layout.root_split();
                let inner = layout.inner(a);
                for rail in [0, 16] {
                    for gap in [0, 49] {
                        if inner.cols < 3 + rail + gap {
                            continue;
                        }
                        layout.rail = rail;
                        layout.gap = gap;
                        layout.fold = Some(1);
                        let tiles = layout.tile_rects(a);
                        let (first, second) = (tiles[0].1, tiles[1].1);
                        assert_eq!((tiles[0].0, tiles[1].0), (1, 2));
                        assert_eq!(
                            (first.x, first.cols, first.rows),
                            (inner.x + rail, 0, inner.rows)
                        );
                        assert_eq!(second.x, inner.x + rail + gap, "{width} {dir:?} {gap}");
                        assert_eq!(second.x + second.cols, inner.x + inner.cols);
                        layout.fold = Some(2);
                        let tiles = layout.tile_rects(a);
                        let (first, second) = (tiles[0].1, tiles[1].1);
                        assert_eq!((second.x, second.cols), (inner.x + inner.cols, 0));
                        assert_eq!(first.x, inner.x + rail);
                        assert_eq!(first.x + first.cols + gap, second.x, "gap, then the edge");
                        assert_eq!(layout.root_split(), saved, "the root is never touched");
                    }
                }
            }
        }
        // Zoomed, the fold is not looked at.
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, area(120, 30)));
        assert!(layout.bind_placeholder(2));
        layout.fold = Some(1);
        layout.zoomed = Some(1);
        assert_eq!(layout.tile_rects(area(120, 30)), vec![(1, area(120, 30))]);
    }

    /// The rail comes off the board (the first pane, or the second with qass first, then the
    /// whole split moves right), both panes when stacked: qass is the same tile with or without it.
    #[test]
    fn the_rail_is_taken_from_the_board_and_qass_keeps_its_tile() {
        let a = area(161, 30);
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, a));
        assert!(layout.bind_placeholder(2));
        // Board first (1), qass second (2).
        layout.gap_second = false;
        layout.gap = 20;
        layout.rail = 0;
        let bare = layout.tile_rects(a);
        layout.rail = 16;
        let railed = layout.tile_rects(a);
        assert_eq!(railed[1].1, bare[1].1, "qass is never touched");
        assert_eq!(
            (railed[0].1.x, railed[0].1.cols),
            (bare[0].1.x + 16, bare[0].1.cols - 16)
        );
        // Qass first (1), the board second (2): qass moves right by the rail, same columns.
        layout.gap_second = true;
        layout.rail = 0;
        let bare = layout.tile_rects(a);
        layout.rail = 16;
        let railed = layout.tile_rects(a);
        assert_eq!(
            (railed[0].1.x, railed[0].1.cols),
            (bare[0].1.x + 16, bare[0].1.cols),
            "qass keeps its columns"
        );
        assert_eq!(
            (railed[1].1.x, railed[1].1.cols),
            (bare[1].1.x + 16, bare[1].1.cols - 16),
            "the board gives the columns"
        );
        assert_eq!(
            railed[1].1.x + railed[1].1.cols,
            bare[1].1.x + bare[1].1.cols
        );
        // Stacked: both lose the rail.
        layout.gap_second = false;
        layout.gap = 0;
        layout.set_root_split(Dir::H, 0.5);
        layout.rail = 0;
        let bare = layout.tile_rects(a);
        layout.rail = 16;
        let railed = layout.tile_rects(a);
        for i in 0..2 {
            assert_eq!(
                railed[i].1.x,
                bare[i].1.x + 16,
                "stacked: both lose the rail"
            );
            assert_eq!(railed[i].1.cols, bare[i].1.cols - 16);
        }
        // The divider is where the panes are drawn: with qass first the whole split stands the
        // rail's columns to the right, and so does the divider a press finds and a drag moves.
        layout.set_root_split(Dir::V, 0.5);
        layout.gap_second = true;
        layout.rail = 0;
        let at = layout.tile_rects(a)[0].1;
        let bare_col = at.x + at.cols;
        assert!(layout.divider_at(a, bare_col, at.y + 3).is_some());
        layout.rail = 16;
        let at = layout.tile_rects(a)[0].1;
        let col = at.x + at.cols;
        assert_eq!(col, bare_col + 16);
        assert!(
            layout.divider_at(a, col, at.y + 3).is_some(),
            "the drawn divider"
        );
        assert!(
            layout.divider_at(a, bare_col, at.y + 3).is_none(),
            "not where it was"
        );
        let before = layout.root_split();
        assert!(layout.drag(a, &[], col + 5, at.y + 3));
        assert_ne!(layout.root_split(), before);
        let moved = layout.tile_rects(a)[0].1;
        assert_eq!(
            moved.x + moved.cols,
            col + 5,
            "the divider follows the pointer"
        );
    }

    /// Swapping the root's panes keeps every pane's size to the cell, twice is the identity,
    /// and the gap follows the board: cut from the second pane's left edge when it is second.
    #[test]
    fn swapping_the_root_keeps_every_size_and_twice_is_the_identity() {
        for width in [97, 120, 161, 200] {
            let a = area(width, 24);
            for ratio in [0.3_f32, 0.5, 0.7, 0.713] {
                let mut layout = Layout::single(1);
                assert!(layout.split_placeholder(1, Dir::V, a));
                assert!(layout.bind_placeholder(2));
                assert!(layout.set_root_split(Dir::V, ratio));
                let before = layout.tile_rects(a);
                assert!(layout.swap_root(a));
                assert_eq!(layout.leaves(), [2, 1]);
                let after = layout.tile_rects(a);
                for (t, r) in &before {
                    let now = after.iter().find(|(u, _)| u == t).unwrap().1;
                    assert_eq!(now.cols, r.cols, "{width} {ratio}: {t} keeps its columns");
                    assert_eq!(now.rows, r.rows);
                }
                // Side by side still, on the other sides.
                assert!(after[0].1.x < after[1].1.x);
                assert!(layout.swap_root(a));
                assert_eq!(layout.leaves(), [1, 2]);
                assert_eq!(layout.tile_rects(a), before, "{width} {ratio}");
            }
        }
        // A tree with no split has nothing to swap.
        assert!(!Layout::single(1).swap_root(area(80, 24)));
        // The gap is cut from the first pane's right edge, or the second's left edge.
        let a = area(120, 24);
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::V, a));
        assert!(layout.bind_placeholder(2));
        let (one, two) = (rect_of_in(&layout, 1, a), rect_of_in(&layout, 2, a));
        layout.gap = 10;
        assert_eq!(rect_of_in(&layout, 1, a).cols, one.cols - 10);
        assert_eq!(rect_of_in(&layout, 2, a), two, "the other pane is the same");
        layout.gap_second = true;
        assert_eq!(rect_of_in(&layout, 1, a), one);
        let cut = rect_of_in(&layout, 2, a);
        assert_eq!((cut.x, cut.cols), (two.x + 10, two.cols - 10));
    }

    fn rect_of_in(layout: &Layout, terminal: u32, a: Rect) -> Rect {
        layout
            .tile_rects(a)
            .into_iter()
            .find(|(t, _)| *t == terminal)
            .expect("a tile")
            .1
    }

    #[test]
    fn a_nudge_moves_the_nearest_divider_the_way_the_key_points() {
        let mut layout = three();
        let a = area(80, 24);
        let before = rect_of(&layout, 1).cols;
        // 3 is right of the side by side divider: `l` still moves that divider right.
        assert!(layout.nudge(a, Move::Right, 0.05));
        assert!(rect_of(&layout, 1).cols > before);
        assert!(layout.nudge(a, Move::Left, 0.05));
        assert_eq!(rect_of(&layout, 1).cols, before);
        // Up and down take the stacked divider, the nearest one on that axis.
        let top = rect_of(&layout, 2).rows;
        assert!(layout.nudge(a, Move::Down, 0.05));
        assert!(rect_of(&layout, 2).rows > top);
        assert!(layout.nudge(a, Move::Up, 0.2));
        assert!(rect_of(&layout, 2).rows < top);
        // Pane 1 has no stacked divider above it.
        layout.focus_terminal(1);
        assert!(!layout.nudge(a, Move::Down, 0.05));
    }

    #[test]
    fn a_nudge_stops_at_the_limits_and_never_squeezes_a_pane_below_the_minimum() {
        let mut layout = three();
        let a = area(80, 24);
        let mut moves = 0;
        while layout.nudge(a, Move::Right, 0.2) {
            moves += 1;
            assert!(moves < 10, "the divider never stopped");
        }
        assert!(rect_of(&layout, 2).cols >= MIN_COLS);
        // Stacked in a short window: the rows run out before the ratio does.
        let short = area(80, 12);
        while layout.nudge(short, Move::Down, 0.2) {}
        assert!(
            layout
                .tile_rects(short)
                .iter()
                .all(|(_, r)| r.rows >= MIN_ROWS)
        );
        // Zoomed, nothing is drawn to move.
        layout.toggle_zoom();
        assert!(!layout.nudge(a, Move::Left, 0.05));
    }

    #[test]
    fn equalize_halves_a_column_against_a_stack_and_thirds_three_in_a_row() {
        let mut layout = three();
        let a = area(81, 25);
        assert!(layout.nudge(a, Move::Right, 0.2));
        assert!(layout.equalize());
        let widths: Vec<usize> = layout.tile_rects(a).iter().map(|(_, r)| r.cols).collect();
        assert_eq!(widths, vec![39, 39, 39], "the stack is one column");
        assert!(!layout.equalize(), "already even");

        let mut row = Layout::single(1);
        assert!(row.split_placeholder(1, Dir::V, area(80, 24)));
        assert!(row.bind_placeholder(2));
        assert!(row.split_placeholder(2, Dir::V, area(80, 24)));
        assert!(row.bind_placeholder(3));
        row.equalize();
        let widths: Vec<usize> = row
            .tile_rects(area(94, 24))
            .iter()
            .map(|(_, r)| r.cols)
            .collect();
        assert_eq!(widths, vec![30, 30, 30]);
    }

    /// Found in review: `[A over (B over C)]` in twenty rows, dragged to the bottom, left B
    /// and C no room to be drawn at all, and a focused B kept the keyboard off screen.
    #[test]
    fn a_move_that_would_leave_a_pane_undrawn_is_refused() {
        let mut layout = Layout::single(1);
        let a = area(80, 20);
        assert!(layout.split_placeholder(1, Dir::H, a));
        assert!(layout.bind_placeholder(2));
        assert!(layout.split_placeholder(2, Dir::H, a));
        assert!(layout.bind_placeholder(3));
        layout.focus_terminal(2);
        assert!(!layout.drag(a, &[], 0, 19));
        assert!(!layout.nudge(a, Move::Down, 0.9));
        assert_eq!(layout.tile_rects(a).len(), 3, "every pane is still drawn");
    }

    #[test]
    fn a_divider_is_found_under_the_pointer_and_dragged_to_it() {
        let mut layout = three();
        let a = area(80, 24);
        let left = rect_of(&layout, 1);
        let right = rect_of(&layout, 2);
        assert_eq!(layout.divider_at(a, left.x + left.cols, 5), Some(vec![]));
        assert_eq!(
            layout.divider_at(a, right.x + 3, right.y + right.rows),
            Some(vec![true])
        );
        assert_eq!(layout.divider_at(a, 2, 2), None, "inside a pane");
        // Dragged to column 30: the left pane runs from the frame to it, 29 columns.
        assert!(layout.drag(a, &[], 30, 5));
        assert_eq!(rect_of(&layout, 1).cols, 29);
        assert!(
            !layout.drag(a, &[], 30, 9),
            "the same column again is no move"
        );
        // Dragged to the far edge: stopped at the limit, the right side still a pane.
        layout.drag(a, &[], 79, 5);
        assert!(rect_of(&layout, 2).cols >= MIN_COLS);
        // The stacked divider follows rows.
        assert!(layout.drag(a, &[true], 60, 6));
        assert_eq!(rect_of(&layout, 2).rows, 5);
        layout.toggle_zoom();
        assert_eq!(
            layout.divider_at(a, left.x + left.cols, 5),
            None,
            "no divider while zoomed"
        );
    }
}
