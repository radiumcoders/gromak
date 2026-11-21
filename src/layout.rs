//! Pure layout logic: geometry helpers and the dwindle tiling tree.
//!
//! Nothing in this module talks to Win32, so it is fully unit-testable.
//! Windows are identified by an opaque `isize` id (their `HWND` at runtime).

/// An axis-aligned rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// Shrinks the rectangle by `n` pixels on every side.
    pub fn inset(&self, n: i32) -> Rect {
        Rect::new(self.x + n, self.y + n, (self.w - 2 * n).max(0), (self.h - 2 * n).max(0))
    }

    /// A rectangle of `w`×`h` centered inside `self`.
    pub fn centered(&self, w: i32, h: i32) -> Rect {
        Rect::new(self.x + (self.w - w) / 2, self.y + (self.h - h) / 2, w, h)
    }
}

/// A cardinal direction, used for focus / move / resize commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn is_horizontal(self) -> bool {
        matches!(self, Dir::Left | Dir::Right)
    }
}

#[derive(Clone, Debug)]
enum Node {
    Leaf(isize),
    Split {
        /// `true` when the children sit side by side (a | b),
        /// `false` when they are stacked (a over b).
        horizontal: bool,
        /// Fraction of the available space given to `a`.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

/// A dwindle layout: a binary tree where every new window splits the
/// focused window's tile in two, along its longer side.
#[derive(Clone, Debug, Default)]
pub struct Dwindle {
    root: Option<Node>,
}

const MIN_RATIO: f32 = 0.1;
const MAX_RATIO: f32 = 0.9;

fn split_rect(r: Rect, horizontal: bool, ratio: f32, gap: i32) -> (Rect, Rect) {
    if horizontal {
        let avail = (r.w - gap).max(0);
        let aw = (avail as f32 * ratio).round() as i32;
        (Rect { w: aw, ..r }, Rect { x: r.x + aw + gap, w: avail - aw, ..r })
    } else {
        let avail = (r.h - gap).max(0);
        let ah = (avail as f32 * ratio).round() as i32;
        (Rect { h: ah, ..r }, Rect { y: r.y + ah + gap, h: avail - ah, ..r })
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<isize>) {
    match node {
        Node::Leaf(w) => out.push(*w),
        Node::Split { a, b, .. } => {
            collect_leaves(a, out);
            collect_leaves(b, out);
        }
    }
}

fn layout_node(node: &Node, rect: Rect, gap: i32, out: &mut Vec<(isize, Rect)>) {
    match node {
        Node::Leaf(w) => out.push((*w, rect)),
        Node::Split { horizontal, ratio, a, b } => {
            let (ra, rb) = split_rect(rect, *horizontal, *ratio, gap);
            layout_node(a, ra, gap, out);
            layout_node(b, rb, gap, out);
        }
    }
}

fn insert_at(node: &mut Node, rect: Rect, gap: i32, target: isize, id: isize) -> bool {
    match node {
        Node::Leaf(w) => {
            if *w != target {
                return false;
            }
            let old = *w;
            *node = Node::Split {
                horizontal: rect.w >= rect.h,
                ratio: 0.5,
                a: Box::new(Node::Leaf(old)),
                b: Box::new(Node::Leaf(id)),
            };
            true
        }
        Node::Split { horizontal, ratio, a, b } => {
            let (ra, rb) = split_rect(rect, *horizontal, *ratio, gap);
            insert_at(a, ra, gap, target, id) || insert_at(b, rb, gap, target, id)
        }
    }
}

fn remove_node(node: Node, id: isize) -> Option<Node> {
    match node {
        Node::Leaf(w) => (w != id).then_some(Node::Leaf(w)),
        Node::Split { horizontal, ratio, a, b } => match (remove_node(*a, id), remove_node(*b, id)) {
            (Some(a), Some(b)) => Some(Node::Split { horizontal, ratio, a: Box::new(a), b: Box::new(b) }),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        },
    }
}

fn swap_leaves(node: &mut Node, x: isize, y: isize) {
    match node {
        Node::Leaf(w) => {
            if *w == x {
                *w = y;
            } else if *w == y {
                *w = x;
            }
        }
        Node::Split { a, b, .. } => {
            swap_leaves(a, x, y);
            swap_leaves(b, x, y);
        }
    }
}

fn toggle_parent(node: &mut Node, id: isize) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split { horizontal, a, b, .. } => {
            let is_child = |n: &Node| matches!(n, Node::Leaf(w) if *w == id);
            if is_child(a) || is_child(b) {
                *horizontal = !*horizontal;
                return true;
            }
            toggle_parent(a, id) || toggle_parent(b, id)
        }
    }
}

/// Records the path (false = `a`, true = `b`) from `node` to the leaf `id`.
fn path_to(node: &Node, id: isize, path: &mut Vec<bool>) -> bool {
    match node {
        Node::Leaf(w) => *w == id,
        Node::Split { a, b, .. } => {
            path.push(false);
            if path_to(a, id, path) {
                return true;
            }
            path.pop();
            path.push(true);
            if path_to(b, id, path) {
                return true;
            }
            path.pop();
            false
        }
    }
}

fn node_at_mut<'a>(mut node: &'a mut Node, path: &[bool]) -> &'a mut Node {
    for &side in path {
        node = match node {
            Node::Split { a, b, .. } => {
                if side {
                    b.as_mut()
                } else {
                    a.as_mut()
                }
            }
            Node::Leaf(_) => unreachable!("path descends past a leaf"),
        };
    }
    node
}

impl Dwindle {
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn contains(&self, id: isize) -> bool {
        self.windows().contains(&id)
    }

    /// All windows in the tree, in visual (left-to-right, top-to-bottom) order.
    pub fn windows(&self) -> Vec<isize> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect_leaves(root, &mut out);
        }
        out
    }

    /// Inserts `id` by splitting `target`'s tile (or the last tile when
    /// `target` is absent or not in the tree). `area` and `gap` must match
    /// what [`Dwindle::layout`] will be called with, because the split
    /// direction depends on the target tile's aspect ratio.
    pub fn insert(&mut self, id: isize, target: Option<isize>, area: Rect, gap: i32) {
        if self.contains(id) {
            return;
        }
        let Some(root) = self.root.as_mut() else {
            self.root = Some(Node::Leaf(id));
            return;
        };
        let mut leaves = Vec::new();
        collect_leaves(root, &mut leaves);
        let target = target
            .filter(|t| leaves.contains(t))
            .or_else(|| leaves.last().copied())
            .expect("non-empty tree has leaves");
        insert_at(root, area, gap, target, id);
    }

    /// Removes `id`; its sibling takes over the freed space.
    pub fn remove(&mut self, id: isize) -> bool {
        let had = self.contains(id);
        self.root = self.root.take().and_then(|n| remove_node(n, id));
        had
    }

    /// Swaps the tiles of two windows.
    pub fn swap(&mut self, x: isize, y: isize) {
        if let Some(root) = self.root.as_mut() {
            swap_leaves(root, x, y);
        }
    }

    /// Flips the direction of the split that directly contains `id`.
    pub fn toggle_split(&mut self, id: isize) -> bool {
        self.root.as_mut().is_some_and(|r| toggle_parent(r, id))
    }

    /// Moves the nearest divider around `id` that runs perpendicular to `dir`,
    /// by `delta` (a fraction of the split's size) in direction `dir`.
    pub fn resize(&mut self, id: isize, dir: Dir, delta: f32) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        let mut path = Vec::new();
        if !path_to(root, id, &mut path) {
            return false;
        }
        for depth in (0..path.len()).rev() {
            if let Node::Split { horizontal, ratio, .. } = node_at_mut(root, &path[..depth]) {
                if *horizontal == dir.is_horizontal() {
                    let signed = if matches!(dir, Dir::Right | Dir::Down) { delta } else { -delta };
                    *ratio = (*ratio + signed).clamp(MIN_RATIO, MAX_RATIO);
                    return true;
                }
            }
        }
        false
    }

    /// Computes the rectangle for every window, with `gap` pixels between tiles.
    pub fn layout(&self, area: Rect, gap: i32) -> Vec<(isize, Rect)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            layout_node(root, area, gap, &mut out);
        }
        out
    }
}

fn overlap(a0: i32, a1: i32, b0: i32, b1: i32) -> i32 {
    (a1.min(b1) - a0.max(b0)).max(0)
}

/// Finds the window geometrically adjacent to `from` in direction `dir`.
///
/// Candidates that overlap `from` on the perpendicular axis are preferred,
/// then the closest edge, then the closest center.
pub fn neighbor(rects: &[(isize, Rect)], from: isize, dir: Dir) -> Option<isize> {
    let cur = rects.iter().find(|(id, _)| *id == from)?.1;
    let (cx, cy) = cur.center();
    const TOLERANCE: i32 = 8;

    let pick = |strict: bool| {
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, r)| {
                let (ox, oy) = r.center();
                let beyond = match dir {
                    Dir::Left => ox < cx && (!strict || r.right() <= cur.x + TOLERANCE),
                    Dir::Right => ox > cx && (!strict || r.x >= cur.right() - TOLERANCE),
                    Dir::Up => oy < cy && (!strict || r.bottom() <= cur.y + TOLERANCE),
                    Dir::Down => oy > cy && (!strict || r.y >= cur.bottom() - TOLERANCE),
                };
                if !beyond {
                    return None;
                }
                let (edge, perp, ov) = match dir {
                    Dir::Left => (cur.x - r.right(), (cy - oy).abs(), overlap(cur.y, cur.bottom(), r.y, r.bottom())),
                    Dir::Right => (r.x - cur.right(), (cy - oy).abs(), overlap(cur.y, cur.bottom(), r.y, r.bottom())),
                    Dir::Up => (cur.y - r.bottom(), (cx - ox).abs(), overlap(cur.x, cur.right(), r.x, r.right())),
                    Dir::Down => (r.y - cur.bottom(), (cx - ox).abs(), overlap(cur.x, cur.right(), r.x, r.right())),
                };
                let penalty: i64 = if ov > 0 { 0 } else { 1_000_000 };
                Some((penalty + edge.max(0) as i64 * 4 + perp as i64, *id))
            })
            .min()
            .map(|(_, id)| id)
    };
    pick(true).or_else(|| pick(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1000, 600);

    fn rect_of(layout: &[(isize, Rect)], id: isize) -> Rect {
        layout.iter().find(|(w, _)| *w == id).unwrap().1
    }

    #[test]
    fn single_window_fills_area() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 10);
        assert_eq!(t.layout(AREA, 10), vec![(1, AREA)]);
    }

    #[test]
    fn second_window_splits_along_longer_side() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 10);
        t.insert(2, Some(1), AREA, 10);
        let l = t.layout(AREA, 10);
        assert_eq!(rect_of(&l, 1), Rect::new(0, 0, 495, 600));
        assert_eq!(rect_of(&l, 2), Rect::new(505, 0, 495, 600));
    }

    #[test]
    fn third_window_dwindles_into_target() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(2, Some(1), AREA, 0);
        t.insert(3, Some(2), AREA, 0);
        let l = t.layout(AREA, 0);
        assert_eq!(rect_of(&l, 1), Rect::new(0, 0, 500, 600));
        // Window 2's tile (500x600) is taller than wide, so it splits vertically.
        assert_eq!(rect_of(&l, 2), Rect::new(500, 0, 500, 300));
        assert_eq!(rect_of(&l, 3), Rect::new(500, 300, 500, 300));
        assert_eq!(t.windows(), vec![1, 2, 3]);
    }

    #[test]
    fn insert_without_target_uses_last_tile() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(2, None, AREA, 0);
        t.insert(3, Some(99), AREA, 0);
        assert_eq!(t.windows(), vec![1, 2, 3]);
    }

    #[test]
    fn duplicate_insert_is_ignored() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(1, None, AREA, 0);
        assert_eq!(t.windows(), vec![1]);
    }

    #[test]
    fn remove_gives_space_to_sibling() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(2, Some(1), AREA, 0);
        t.insert(3, Some(2), AREA, 0);
        assert!(t.remove(2));
        let l = t.layout(AREA, 0);
        assert_eq!(rect_of(&l, 3), Rect::new(500, 0, 500, 600));
        assert!(t.remove(1));
        assert!(t.remove(3));
        assert!(t.is_empty());
        assert!(!t.remove(3));
    }

    #[test]
    fn swap_exchanges_tiles() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(2, Some(1), AREA, 0);
        t.swap(1, 2);
        let l = t.layout(AREA, 0);
        assert_eq!(rect_of(&l, 2).x, 0);
        assert_eq!(rect_of(&l, 1).x, 500);
    }

    #[test]
    fn toggle_split_flips_direction() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(2, Some(1), AREA, 0);
        assert!(t.toggle_split(2));
        let l = t.layout(AREA, 0);
        assert_eq!(rect_of(&l, 1), Rect::new(0, 0, 1000, 300));
        assert_eq!(rect_of(&l, 2), Rect::new(0, 300, 1000, 300));
    }

    #[test]
    fn resize_moves_matching_divider_and_clamps() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(2, Some(1), AREA, 0);
        assert!(t.resize(1, Dir::Right, 0.1));
        assert_eq!(rect_of(&t.layout(AREA, 0), 1).w, 600);
        // No vertical divider exists yet.
        assert!(!t.resize(1, Dir::Down, 0.1));
        for _ in 0..20 {
            t.resize(2, Dir::Left, 0.1);
        }
        assert_eq!(rect_of(&t.layout(AREA, 0), 1).w, 100);
    }

    #[test]
    fn neighbor_finds_adjacent_tiles() {
        let mut t = Dwindle::default();
        t.insert(1, None, AREA, 0);
        t.insert(2, Some(1), AREA, 0);
        t.insert(3, Some(2), AREA, 0);
        let l = t.layout(AREA, 0);
        assert_eq!(neighbor(&l, 1, Dir::Right), Some(2));
        assert_eq!(neighbor(&l, 3, Dir::Left), Some(1));
        assert_eq!(neighbor(&l, 2, Dir::Down), Some(3));
        assert_eq!(neighbor(&l, 3, Dir::Up), Some(2));
        assert_eq!(neighbor(&l, 1, Dir::Left), None);
        assert_eq!(neighbor(&l, 2, Dir::Right), None);
    }

    #[test]
    fn rect_helpers() {
        let r = Rect::new(10, 20, 100, 50);
        assert_eq!(r.inset(5), Rect::new(15, 25, 90, 40));
        assert_eq!(r.centered(50, 10), Rect::new(35, 40, 50, 10));
        assert!(r.contains(10, 20));
        assert!(!r.contains(110, 20));
    }
}
