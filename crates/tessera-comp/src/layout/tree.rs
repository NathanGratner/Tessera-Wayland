//! The tiling model: one binary split tree per workspace (design §3.3).
//!
//! Deliberately free of Smithay types so it can be unit-tested on any platform.
//! Leaves are windows; inner nodes split their area along an axis at a ratio.

/// Which way a split divides its area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children sit side by side (left | right).
    Horizontal,
    /// Children sit one above the other (top / bottom).
    Vertical,
}

/// A direction for focus, swapping, resizing and placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }

    /// True for the directions that point at the second child of a split.
    fn is_forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(self) -> i32 {
        self.y + self.h
    }

    fn inset(self, d: i32) -> Self {
        Self {
            x: self.x + d,
            y: self.y + d,
            w: (self.w - 2 * d).max(0),
            h: (self.h - 2 * d).max(0),
        }
    }

    /// Splits into two rectangles with `gap` pixels between them.
    fn split(self, axis: Axis, ratio: f32, gap: i32) -> (Self, Self) {
        match axis {
            Axis::Horizontal => {
                let available = (self.w - gap).max(0);
                let first = (available as f32 * ratio)
                    .round()
                    .clamp(0.0, available as f32) as i32;
                (
                    Rect::new(self.x, self.y, first, self.h),
                    Rect::new(self.x + first + gap, self.y, available - first, self.h),
                )
            }
            Axis::Vertical => {
                let available = (self.h - gap).max(0);
                let first = (available as f32 * ratio)
                    .round()
                    .clamp(0.0, available as f32) as i32;
                (
                    Rect::new(self.x, self.y, self.w, first),
                    Rect::new(self.x, self.y + first + gap, self.w, available - first),
                )
            }
        }
    }

    /// Shrinks to whole cells, centring the leftover pixels so they join the gaps.
    fn snap_to_cells(self, cell_w: i32, cell_h: i32) -> Self {
        let snap = |value: i32, cell: i32| {
            if cell <= 0 {
                return (value, 0);
            }
            let snapped = value - value % cell;
            if snapped <= 0 {
                (value, 0)
            } else {
                (snapped, (value - snapped) / 2)
            }
        };
        let (w, dx) = snap(self.w, cell_w);
        let (h, dy) = snap(self.h, cell_h);
        Rect::new(self.x + dx, self.y + dy, w, h)
    }
}

/// How a new window enters the tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Placement<Id> {
    /// Split the focused window along its longer side ("dwindle").
    Auto,
    /// Split `anchor`, putting the new window on `side` with `ratio` of that space.
    Beside {
        anchor: Id,
        side: Direction,
        ratio: f32,
    },
}

/// Gaps and cell snapping applied when turning the tree into rectangles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutOptions {
    /// Pixels between tiles, and between tiles and the screen edge.
    pub gaps: i32,
    /// Cell size for "snap to cell grid"; tiles shrink to whole cells.
    pub snap: Option<(i32, i32)>,
}

pub const MIN_RATIO: f32 = 0.1;
pub const MAX_RATIO: f32 = 0.9;
const DEFAULT_RATIO: f32 = 0.5;

/// Which child of a split to descend into; a `Vec<Branch>` addresses one leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branch {
    A,
    B,
}

#[derive(Debug, Clone, PartialEq)]
enum Node<Id> {
    Leaf(Id),
    Split {
        axis: Axis,
        ratio: f32,
        a: Box<Node<Id>>,
        b: Box<Node<Id>>,
    },
}

impl<Id: Clone + Eq> Node<Id> {
    fn contains(&self, id: &Id) -> bool {
        match self {
            Node::Leaf(leaf) => leaf == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    fn collect(&self, out: &mut Vec<Id>) {
        match self {
            Node::Leaf(id) => out.push(id.clone()),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    fn layout(&self, rect: Rect, gaps: i32, out: &mut Vec<(Id, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((id.clone(), rect)),
            Node::Split { axis, ratio, a, b } => {
                let (first, second) = rect.split(*axis, *ratio, gaps);
                a.layout(first, gaps, out);
                b.layout(second, gaps, out);
            }
        }
    }

    fn path_to(&self, id: &Id, path: &mut Vec<Branch>) -> bool {
        match self {
            Node::Leaf(leaf) => leaf == id,
            Node::Split { a, b, .. } => {
                path.push(Branch::A);
                if a.path_to(id, path) {
                    return true;
                }
                path.pop();
                path.push(Branch::B);
                if b.path_to(id, path) {
                    return true;
                }
                path.pop();
                false
            }
        }
    }

    fn leaf_at_mut(&mut self, path: &[Branch]) -> Option<&mut Id> {
        match (self, path.split_first()) {
            (Node::Leaf(leaf), None) => Some(leaf),
            (Node::Split { a, b, .. }, Some((branch, rest))) => match branch {
                Branch::A => a.leaf_at_mut(rest),
                Branch::B => b.leaf_at_mut(rest),
            },
            _ => None,
        }
    }

    /// Turns the leaf holding `target` into a split containing it and `new`.
    fn split_leaf(&mut self, target: &Id, new: Id, axis: Axis, ratio: f32, new_is_second: bool) {
        match self {
            Node::Leaf(leaf) if leaf == target => {
                let existing = Node::Leaf(leaf.clone());
                let new = Node::Leaf(new);
                let (a, b) = if new_is_second {
                    (existing, new)
                } else {
                    (new, existing)
                };
                *self = Node::Split {
                    axis,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                };
            }
            Node::Leaf(_) => {}
            Node::Split { a, b, .. } => {
                if a.contains(target) {
                    a.split_leaf(target, new, axis, ratio, new_is_second);
                } else if b.contains(target) {
                    b.split_leaf(target, new, axis, ratio, new_is_second);
                }
            }
        }
    }

    /// Removes `id`. Returns true when this node itself was the leaf to drop.
    fn remove(&mut self, id: &Id) -> bool {
        match self {
            Node::Leaf(leaf) => leaf == id,
            Node::Split { a, b, .. } => {
                if a.contains(id) {
                    if a.remove(id) {
                        // `a` was the leaf: this split collapses into `b`.
                        let remaining = std::mem::replace(&mut **b, Node::Leaf(id.clone()));
                        *self = remaining;
                    }
                } else if b.contains(id) && b.remove(id) {
                    let remaining = std::mem::replace(&mut **a, Node::Leaf(id.clone()));
                    *self = remaining;
                }
                false
            }
        }
    }

    /// Moves the shared edge of the closest enclosing split on `dir`'s axis.
    fn resize(&mut self, id: &Id, dir: Direction, delta: f32) -> bool {
        let Node::Split { axis, ratio, a, b } = self else {
            return false;
        };
        let in_a = a.contains(id);
        if !in_a && !b.contains(id) {
            return false;
        }

        // Deeper splits win, so resizing moves the edge nearest the window.
        let child = if in_a { a } else { b };
        if child.resize(id, dir, delta) {
            return true;
        }

        if *axis != dir.axis() {
            return false;
        }
        let signed = if dir.is_forward() { delta } else { -delta };
        *ratio = (*ratio + signed).clamp(MIN_RATIO, MAX_RATIO);
        true
    }
}

/// One workspace's windows, tiled.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutTree<Id> {
    root: Option<Node<Id>>,
}

impl<Id> Default for LayoutTree<Id> {
    fn default() -> Self {
        Self { root: None }
    }
}

impl<Id: Clone + Eq> LayoutTree<Id> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn contains(&self, id: &Id) -> bool {
        self.root.as_ref().is_some_and(|root| root.contains(id))
    }

    /// Every window, in tree order (left/top to right/bottom).
    pub fn ids(&self) -> Vec<Id> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.collect(&mut out);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.ids().len()
    }

    /// Rectangles for every window, with gaps applied and optional cell snapping.
    pub fn layout(&self, area: Rect, opts: LayoutOptions) -> Vec<(Id, Rect)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.layout(area.inset(opts.gaps), opts.gaps, &mut out);
        }
        if let Some((cell_w, cell_h)) = opts.snap {
            for (_, rect) in &mut out {
                *rect = rect.snap_to_cells(cell_w, cell_h);
            }
        }
        out
    }

    /// Adds a window. `focused` decides which leaf `Placement::Auto` splits;
    /// `area` is only used to pick the split axis.
    pub fn insert(&mut self, id: Id, placement: Placement<Id>, focused: Option<&Id>, area: Rect) {
        if self.contains(&id) {
            return;
        }
        if self.root.is_none() {
            self.root = Some(Node::Leaf(id));
            return;
        }

        let (target, axis, ratio, new_is_second) = match placement {
            Placement::Auto => {
                let Some(target) = focused
                    .filter(|id| self.contains(id))
                    .cloned()
                    .or_else(|| self.ids().last().cloned())
                else {
                    return;
                };
                // Split the target tile along its longer side.
                let rect = self.rect_of(&target, area);
                let axis = if rect.w >= rect.h {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                (target, axis, DEFAULT_RATIO, true)
            }
            Placement::Beside {
                anchor,
                side,
                ratio,
            } => {
                if !self.contains(&anchor) {
                    return;
                }
                let new_is_second = side.is_forward();
                // `ratio` is the new window's share of the anchor's space.
                let share = ratio.clamp(MIN_RATIO, MAX_RATIO);
                let split_ratio = if new_is_second { 1.0 - share } else { share };
                (anchor, side.axis(), split_ratio, new_is_second)
            }
        };

        if let Some(root) = &mut self.root {
            root.split_leaf(&target, id, axis, ratio, new_is_second);
        }
    }

    /// Removes a window; its sibling takes over the space. True if it was there.
    pub fn remove(&mut self, id: &Id) -> bool {
        let Some(root) = &mut self.root else {
            return false;
        };
        if !root.contains(id) {
            return false;
        }
        if root.remove(id) {
            self.root = None;
        }
        true
    }

    /// The window next to `id` in `dir`, by the rectangles `area` produces.
    pub fn neighbor(&self, id: &Id, dir: Direction, area: Rect, opts: LayoutOptions) -> Option<Id> {
        // Snapping leaves ragged edges; adjacency is clearer without it.
        let rects = self.layout(area, LayoutOptions { snap: None, ..opts });
        let me = rects.iter().find(|(other, _)| other == id)?.1;

        rects
            .iter()
            .filter(|(other, _)| other != id)
            .filter_map(|(other, rect)| {
                let gap = match dir {
                    Direction::Right => rect.x - me.right(),
                    Direction::Left => me.x - rect.right(),
                    Direction::Down => rect.y - me.bottom(),
                    Direction::Up => me.y - rect.bottom(),
                };
                if gap < 0 {
                    return None;
                }
                // Only windows that actually share an edge with this one.
                let overlaps = match dir.axis() {
                    Axis::Horizontal => rect.y < me.bottom() && me.y < rect.bottom(),
                    Axis::Vertical => rect.x < me.right() && me.x < rect.right(),
                };
                if !overlaps {
                    return None;
                }
                let offset = match dir.axis() {
                    Axis::Horizontal => (rect.y - me.y).abs(),
                    Axis::Vertical => (rect.x - me.x).abs(),
                };
                Some((gap, offset, other.clone()))
            })
            .min_by_key(|(gap, offset, _)| (*gap, *offset))
            .map(|(_, _, id)| id)
    }

    /// Swaps `id` with its neighbour in `dir`. True if there was one.
    pub fn swap(&mut self, id: &Id, dir: Direction, area: Rect, opts: LayoutOptions) -> bool {
        let Some(other) = self.neighbor(id, dir, area, opts) else {
            return false;
        };
        let Some(root) = &mut self.root else {
            return false;
        };

        let mut here = Vec::new();
        let mut there = Vec::new();
        if !root.path_to(id, &mut here) || !root.path_to(&other, &mut there) {
            return false;
        }
        if let Some(leaf) = root.leaf_at_mut(&here) {
            *leaf = other.clone();
        }
        if let Some(leaf) = root.leaf_at_mut(&there) {
            *leaf = id.clone();
        }
        true
    }

    /// Moves the edge nearest `id` on `dir`'s axis by `delta`, clamped to 10–90%.
    /// The edge always moves in `dir`, so Right/Down grow the split's first child.
    pub fn resize(&mut self, id: &Id, dir: Direction, delta: f32) -> bool {
        self.root
            .as_mut()
            .is_some_and(|root| root.resize(id, dir, delta))
    }

    fn rect_of(&self, id: &Id, area: Rect) -> Rect {
        self.layout(area, LayoutOptions::default())
            .into_iter()
            .find(|(other, _)| other == id)
            .map(|(_, rect)| rect)
            .unwrap_or(area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        w: 1000,
        h: 800,
    };

    fn opts(gaps: i32) -> LayoutOptions {
        LayoutOptions { gaps, snap: None }
    }

    /// Builds a tree the way the compositor does: each new window splits the previous one.
    fn dwindle(ids: &[&'static str]) -> LayoutTree<&'static str> {
        let mut tree = LayoutTree::new();
        let mut focus: Option<&'static str> = None;
        for id in ids {
            tree.insert(*id, Placement::Auto, focus.as_ref(), AREA);
            focus = Some(*id);
        }
        tree
    }

    fn rect(tree: &LayoutTree<&'static str>, id: &str, opts: LayoutOptions) -> Rect {
        tree.layout(AREA, opts)
            .into_iter()
            .find(|(other, _)| *other == id)
            .unwrap_or_else(|| panic!("{id} is not in the tree"))
            .1
    }

    #[test]
    fn empty_tree_has_no_windows() {
        let tree: LayoutTree<&str> = LayoutTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert!(tree.layout(AREA, opts(0)).is_empty());
        assert!(!tree.contains(&"a"));
    }

    #[test]
    fn first_window_fills_the_area() {
        let tree = dwindle(&["a"]);
        assert_eq!(rect(&tree, "a", opts(0)), AREA);
    }

    #[test]
    fn second_window_splits_the_longer_side() {
        // 1000x800 is wider than tall, so the split is left/right.
        let tree = dwindle(&["a", "b"]);
        assert_eq!(rect(&tree, "a", opts(0)), Rect::new(0, 0, 500, 800));
        assert_eq!(rect(&tree, "b", opts(0)), Rect::new(500, 0, 500, 800));
    }

    #[test]
    fn third_window_dwindles_into_the_focused_tile() {
        // b's tile is 500x800, taller than wide, so it splits top/bottom.
        let tree = dwindle(&["a", "b", "c"]);
        assert_eq!(rect(&tree, "a", opts(0)), Rect::new(0, 0, 500, 800));
        assert_eq!(rect(&tree, "b", opts(0)), Rect::new(500, 0, 500, 400));
        assert_eq!(rect(&tree, "c", opts(0)), Rect::new(500, 400, 500, 400));
        assert_eq!(tree.ids(), vec!["a", "b", "c"]);
    }

    #[test]
    fn auto_insert_without_focus_splits_the_last_window() {
        let mut tree = dwindle(&["a", "b"]);
        tree.insert("c", Placement::Auto, None, AREA);
        assert_eq!(rect(&tree, "b", opts(0)), Rect::new(500, 0, 500, 400));
        assert_eq!(rect(&tree, "c", opts(0)), Rect::new(500, 400, 500, 400));
    }

    #[test]
    fn beside_gives_the_new_window_its_share() {
        let mut tree = dwindle(&["a"]);
        tree.insert(
            "b",
            Placement::Beside {
                anchor: "a",
                side: Direction::Right,
                ratio: 0.6,
            },
            None,
            AREA,
        );
        // The new window takes 60% on the right; this is the §8 tiled-terminal case.
        assert_eq!(rect(&tree, "a", opts(0)), Rect::new(0, 0, 400, 800));
        assert_eq!(rect(&tree, "b", opts(0)), Rect::new(400, 0, 600, 800));
    }

    #[test]
    fn beside_can_place_the_new_window_first() {
        let mut tree = dwindle(&["a"]);
        tree.insert(
            "b",
            Placement::Beside {
                anchor: "a",
                side: Direction::Left,
                ratio: 0.25,
            },
            None,
            AREA,
        );
        assert_eq!(rect(&tree, "b", opts(0)), Rect::new(0, 0, 250, 800));
        assert_eq!(rect(&tree, "a", opts(0)), Rect::new(250, 0, 750, 800));
    }

    #[test]
    fn beside_clamps_extreme_ratios() {
        let mut tree = dwindle(&["a"]);
        tree.insert(
            "b",
            Placement::Beside {
                anchor: "a",
                side: Direction::Right,
                ratio: 0.99,
            },
            None,
            AREA,
        );
        assert_eq!(rect(&tree, "b", opts(0)).w, 900);
    }

    #[test]
    fn beside_an_unknown_anchor_does_nothing() {
        let mut tree = dwindle(&["a"]);
        tree.insert(
            "b",
            Placement::Beside {
                anchor: "ghost",
                side: Direction::Right,
                ratio: 0.5,
            },
            None,
            AREA,
        );
        assert_eq!(tree.ids(), vec!["a"]);
    }

    #[test]
    fn inserting_a_duplicate_id_is_ignored() {
        let mut tree = dwindle(&["a", "b"]);
        tree.insert("b", Placement::Auto, Some(&"a"), AREA);
        assert_eq!(tree.ids(), vec!["a", "b"]);
    }

    #[test]
    fn removing_the_only_window_empties_the_tree() {
        let mut tree = dwindle(&["a"]);
        assert!(tree.remove(&"a"));
        assert!(tree.is_empty());
        assert!(!tree.remove(&"a"));
    }

    #[test]
    fn removing_a_deep_leaf_gives_its_space_to_its_sibling() {
        let mut tree = dwindle(&["a", "b", "c"]);
        assert!(tree.remove(&"c"));
        assert_eq!(tree.ids(), vec!["a", "b"]);
        // b reclaims the whole right half.
        assert_eq!(rect(&tree, "b", opts(0)), Rect::new(500, 0, 500, 800));
        assert_eq!(rect(&tree, "a", opts(0)), Rect::new(0, 0, 500, 800));
    }

    #[test]
    fn removing_a_middle_window_lets_the_others_reclaim_the_space() {
        let mut tree = dwindle(&["a", "b", "c"]);
        assert!(tree.remove(&"b"));
        assert_eq!(rect(&tree, "c", opts(0)), Rect::new(500, 0, 500, 800));
    }

    #[test]
    fn removing_an_unknown_window_reports_false() {
        let mut tree = dwindle(&["a", "b"]);
        assert!(!tree.remove(&"ghost"));
        assert_eq!(tree.len(), 2);
    }

    #[test]
    fn neighbor_finds_the_window_sharing_an_edge() {
        let tree = dwindle(&["a", "b", "c"]);
        // a is the left half; b is top-right, c is bottom-right.
        assert_eq!(
            tree.neighbor(&"a", Direction::Right, AREA, opts(0)),
            Some("b")
        );
        assert_eq!(
            tree.neighbor(&"b", Direction::Left, AREA, opts(0)),
            Some("a")
        );
        assert_eq!(
            tree.neighbor(&"b", Direction::Down, AREA, opts(0)),
            Some("c")
        );
        assert_eq!(tree.neighbor(&"c", Direction::Up, AREA, opts(0)), Some("b"));
    }

    #[test]
    fn neighbor_stops_at_the_screen_edge() {
        let tree = dwindle(&["a", "b", "c"]);
        assert_eq!(tree.neighbor(&"a", Direction::Left, AREA, opts(0)), None);
        assert_eq!(tree.neighbor(&"a", Direction::Up, AREA, opts(0)), None);
        assert_eq!(tree.neighbor(&"c", Direction::Down, AREA, opts(0)), None);
        assert_eq!(tree.neighbor(&"ghost", Direction::Up, AREA, opts(0)), None);
    }

    #[test]
    fn neighbor_still_works_across_gaps() {
        let tree = dwindle(&["a", "b"]);
        assert_eq!(
            tree.neighbor(&"a", Direction::Right, AREA, opts(10)),
            Some("b")
        );
    }

    #[test]
    fn swap_exchanges_two_tiles() {
        let mut tree = dwindle(&["a", "b", "c"]);
        let before = rect(&tree, "a", opts(0));
        assert!(tree.swap(&"a", Direction::Right, AREA, opts(0)));
        assert_eq!(rect(&tree, "b", opts(0)), before);
        assert_eq!(rect(&tree, "a", opts(0)), Rect::new(500, 0, 500, 400));
        assert_eq!(tree.len(), 3);
    }

    #[test]
    fn swap_without_a_neighbour_changes_nothing() {
        let mut tree = dwindle(&["a", "b"]);
        let before = tree.clone();
        assert!(!tree.swap(&"a", Direction::Left, AREA, opts(0)));
        assert_eq!(tree, before);
    }

    #[test]
    fn resize_moves_the_edge_in_the_given_direction() {
        let mut tree = dwindle(&["a", "b"]);
        assert!(tree.resize(&"a", Direction::Right, 0.05));
        assert_eq!(rect(&tree, "a", opts(0)).w, 550);
        assert!(tree.resize(&"a", Direction::Left, 0.05));
        assert_eq!(rect(&tree, "a", opts(0)).w, 500);
    }

    #[test]
    fn resize_clamps_between_10_and_90_percent() {
        let mut tree = dwindle(&["a", "b"]);
        for _ in 0..20 {
            assert!(tree.resize(&"a", Direction::Right, 0.05));
        }
        assert_eq!(rect(&tree, "a", opts(0)).w, 900);
        for _ in 0..40 {
            assert!(tree.resize(&"a", Direction::Left, 0.05));
        }
        assert_eq!(rect(&tree, "a", opts(0)).w, 100);
    }

    #[test]
    fn resize_uses_the_split_closest_to_the_window() {
        let mut tree = dwindle(&["a", "b", "c"]);
        // c sits in the inner top/bottom split, so Down moves that edge...
        assert!(tree.resize(&"c", Direction::Down, 0.05));
        assert_eq!(rect(&tree, "b", opts(0)).h, 440);
        assert_eq!(rect(&tree, "a", opts(0)).w, 500);
        // ...while Right has to climb to the outer left/right split.
        assert!(tree.resize(&"c", Direction::Right, 0.05));
        assert_eq!(rect(&tree, "a", opts(0)).w, 550);
    }

    #[test]
    fn resize_reports_false_when_no_split_matches() {
        let mut tree = dwindle(&["a", "b"]);
        assert!(!tree.resize(&"a", Direction::Up, 0.05));
        assert!(!tree.resize(&"ghost", Direction::Right, 0.05));
        let single = dwindle(&["only"]);
        assert!(!single.clone().resize(&"only", Direction::Right, 0.05));
    }

    #[test]
    fn gaps_surround_every_tile() {
        let tree = dwindle(&["a", "b"]);
        let a = rect(&tree, "a", opts(10));
        let b = rect(&tree, "b", opts(10));
        assert_eq!(a, Rect::new(10, 10, 485, 780));
        assert_eq!(b, Rect::new(505, 10, 485, 780));
        // Equal gaps at both screen edges and between the tiles.
        assert_eq!(a.x, AREA.w - b.right());
        assert_eq!(b.x - a.right(), 10);
    }

    #[test]
    fn snapping_shrinks_tiles_to_whole_cells_and_centres_the_remainder() {
        let tree = dwindle(&["a"]);
        let snapped = rect(
            &tree,
            "a",
            LayoutOptions {
                gaps: 0,
                snap: Some((12, 17)),
            },
        );
        // 1000 = 83 cells of 12 + 4 spare: 2px joins the gap on each side.
        assert_eq!((snapped.x, snapped.w), (2, 996));
        assert_eq!(snapped.w % 12, 0);
        // 800 = 47 cells of 17 + 1 spare, which cannot be split evenly.
        assert_eq!((snapped.y, snapped.h), (0, 799));
        assert_eq!(snapped.h % 17, 0);
    }

    #[test]
    fn snapping_keeps_tiles_that_are_smaller_than_one_cell() {
        let tree = dwindle(&["a"]);
        let snapped = tree.layout(
            Rect::new(0, 0, 10, 10),
            LayoutOptions {
                gaps: 0,
                snap: Some((100, 100)),
            },
        );
        assert_eq!(snapped[0].1, Rect::new(0, 0, 10, 10));
    }

    #[test]
    fn a_tiny_area_never_produces_negative_sizes() {
        let tree = dwindle(&["a", "b", "c"]);
        for (_, rect) in tree.layout(Rect::new(0, 0, 4, 4), opts(8)) {
            assert!(rect.w >= 0 && rect.h >= 0, "{rect:?}");
        }
    }
}
