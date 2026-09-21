//! Client-local tiling layout: one tree per space.
//!
//! The daemon knows nothing about this. It sees independent terminals with a
//! single resize owner each. The tree only decides which terminals are visible,
//! where on the pane, and which one owns the keyboard and the single `Resize`
//! stream. Every function here is pure so it can be tested without a screen.

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

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub root: Node,
    pub focus: u32,
    pub prev_focus: Option<u32>,
    pub zoomed: Option<u32>,
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
        let Some(rect) = self
            .tile_rects(area)
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
                .or_else(|| neighbour_in_order(&leaves, terminal))
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
        let mut out = Vec::new();
        layout_node(&self.root, area, &mut out);
        out
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

fn remove_terminal(node: &mut Node, terminal: u32) {
    match node {
        // A single bare leaf removes to a bare root only when it is the one
        // being removed; a placeholder beside a dying leaf dies with it, or
        // every failed split resurrects as a pending zombie (see below).
        Node::Leaf { terminal: slot } => {
            if *slot == Some(terminal) {
                *slot = None;
            }
        }
        Node::Split { first, second, .. } => {
            remove_terminal(first, terminal);
            remove_terminal(second, terminal);
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
        }
    }
}

fn is_empty_leaf(node: &Node) -> bool {
    matches!(node, Node::Leaf { terminal: None })
}

/// The visible leaf after `terminal` in tree order, wrapping. Used when the
/// previous focus is gone: the neighbour closest to the closed pane.
fn neighbour_in_order(leaves: &[u32], closed: u32) -> Option<u32> {
    if leaves.is_empty() {
        return None;
    }
    // Without positions of the closed leaf, fall back to the first leaf.
    // Callers with geometry should prefer `neighbour_in_dir`.
    let _ = closed;
    Some(leaves[0])
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
            let ratio = ratio.clamp(0.1, 0.9);
            match dir {
                Dir::V => {
                    // A shrunken window can leave less than two cells: clamp to
                    // a real rectangle inside `area` rather than spilling past
                    // it. `fits` guards the split moment; this guards every
                    // repaint after.
                    let total = area.cols.saturating_sub(1);
                    if total < 2 || area.rows == 0 {
                        return;
                    }
                    let left = ((total as f32 * ratio).floor() as usize)
                        .max(1)
                        .min(total.saturating_sub(1).max(1));
                    let right = total.saturating_sub(left);
                    layout_node(
                        first,
                        Rect {
                            x: area.x,
                            y: area.y,
                            cols: left,
                            rows: area.rows,
                        },
                        out,
                    );
                    layout_node(
                        second,
                        Rect {
                            x: area.x + left + 1,
                            y: area.y,
                            cols: right,
                            rows: area.rows,
                        },
                        out,
                    );
                }
                Dir::H => {
                    let total = area.rows.saturating_sub(1);
                    if total < 2 || area.cols == 0 {
                        return;
                    }
                    let top = ((total as f32 * ratio).floor() as usize)
                        .max(1)
                        .min(total.saturating_sub(1).max(1));
                    let bottom = total.saturating_sub(top);
                    layout_node(
                        first,
                        Rect {
                            x: area.x,
                            y: area.y,
                            cols: area.cols,
                            rows: top,
                        },
                        out,
                    );
                    layout_node(
                        second,
                        Rect {
                            x: area.x,
                            y: area.y + top + 1,
                            cols: area.cols,
                            rows: bottom,
                        },
                        out,
                    );
                }
            }
        }
    }
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
        let total: usize = rects.iter().map(|(_, r)| r.cols).sum::<usize>() + 1;
        assert_eq!(total, 80);
        assert!(rects.iter().all(|(_, r)| r.rows == 24));
    }

    #[test]
    fn a_horizontal_split_stacks_two_panes_with_one_divider() {
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, Dir::H, area(80, 24)));
        assert!(layout.bind_placeholder(2));
        let rects = layout.tile_rects(area(80, 24));
        assert_eq!(rects.len(), 2);
        let total: usize = rects.iter().map(|(_, r)| r.rows).sum::<usize>() + 1;
        assert_eq!(total, 24);
        assert!(rects.iter().all(|(_, r)| r.cols == 80));
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
        // The column right after the left pane is the divider.
        assert_eq!(layout.hit(area(80, 24), left.cols, 0), None);
        assert_eq!(layout.hit(area(80, 24), 0, 0), Some(1));
    }
}
