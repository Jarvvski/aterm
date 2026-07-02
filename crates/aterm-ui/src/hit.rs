//! Pure pointer hit-testing (ticket T-9.8): the one shared map from a pointer
//! position to the clickable/hoverable UI target under it.
//!
//! The app handled only keys + the scroll wheel; every hover/click affordance the
//! vision mock (ADR-0011) calls for shipped as a keyboard-only stub. This module is
//! the crown of the plumbing that completes them: a [`HitTarget`] enum and a
//! [`HitMap`] the render front-ends populate with `(rect, target)` regions as they
//! compute geometry, plus a pure [`HitMap::hit`] the pointer path queries. No winit,
//! no GPU, no clock - so the whole contract is unit-tested on every platform.
//!
//! ## Coordinate space
//!
//! All rects AND the query point are in **physical px** relative to the surface
//! top-left - the renderer's native draw space (every front-end emits its rects in
//! physical px against the physical viewport). Physical-vs-physical is inherently
//! scale-correct: the pointer (winit reports `CursorMoved` in physical px, relative
//! to the content-area origin, which is exactly where our surface `(0, 0)` sits) and
//! the rects scale together, so no title-bar inset offset is needed and no rounding
//! round-trip through logical px is introduced.
//!
//! ## Overlap rule
//!
//! Regions are pushed in DRAW order (bottom layer first, topmost last), mirroring the
//! renderer's back-to-front pass. [`HitMap::hit`] therefore returns the LAST-inserted
//! region that contains the point - the topmost one - so a target drawn over another
//! (a popover row over a timeline block) wins, matching what the user sees.

/// A borderless-window control dot (ticket T-9.9): the mock's warm traffic-light dots,
/// now wired to real window operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControl {
    /// Close the window (the red dot).
    Close,
    /// Minimize/miniaturize the window (the amber dot).
    Minimize,
    /// Zoom/maximize toggle (the green dot).
    Zoom,
}

/// A clickable / hoverable UI target the pointer can land on. Carries only the
/// identity the host needs to map a click onto its EXISTING keyboard intent - never
/// any action semantics (those live in `aterm-app`, ticket T-9.8). The
/// borderless-window controls (T-9.9) are handled in `aterm-ui` itself (the winit
/// window + event loop), not forwarded to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// The custom title bar's sidebar-toggle glyph - the pointer twin of `Cmd-B`
    /// ([`crate::title_bar`]).
    SidebarToggle,
    /// The unified input box's mode PILL chip - the pointer twin of `Cmd-/`
    /// ([`crate::input_widget`]).
    ModeChip,
    /// A command block's hover region, carrying the block's INDEX in the
    /// [`aterm_core::BlockList`] (`aterm-core` has no stable `BlockId`, so the index -
    /// matching [`crate::timeline::VisibleBlock::index`] - is the identity). Hovering
    /// it reveals the block-meta (the mock's `.block:hover .block-meta`); it has no
    /// click action yet.
    BlockMeta(usize),
    /// A tab-completion popover row, carrying its 0-based index into the ranked items -
    /// the pointer twin of moving to that row and pressing `Enter`
    /// ([`crate::completion_render`]).
    CompletionRow(usize),
    /// A borderless-window control dot (ticket T-9.9). Handled in [`crate::app`] against
    /// the winit window / event loop (close / minimize / zoom), not the host.
    WindowControl(WindowControl),
}

/// A rect in physical px: `[x, y, w, h]`.
pub type HitRect = [f32; 4];

/// A retained map of the frame's clickable regions, rebuilt each rendered frame.
///
/// The renderer owns one and repopulates it during its build pass from each drawn
/// front-end's cached geometry; [`Self::clear`] keeps the backing capacity so a warm
/// frame's rebuild allocates nothing (the T-1.8 60fps floor). It survives idle frames
/// untouched: geometry only changes when a front-end rebuilds, so the last populated
/// map stays valid for a pointer query between presents.
#[derive(Debug, Default, Clone)]
pub struct HitMap {
    regions: Vec<(HitRect, HitTarget)>,
}

impl HitMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop all regions but keep the backing capacity (so the next rebuild reuses the
    /// warm `Vec` with zero allocation).
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    /// Add one clickable region. Push in DRAW order (topmost last) so [`Self::hit`]'s
    /// last-wins scan returns the topmost target.
    pub fn push(&mut self, rect: HitRect, target: HitTarget) {
        self.regions.push((rect, target));
    }

    /// Whether the map has no regions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// The number of regions (test / instrumentation).
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// The topmost target containing `(x, y)`, or `None` for a miss / empty map. Scans
    /// newest-first so the LAST-inserted (topmost, drawn-over) region wins on overlap.
    /// Rects are half-open on the right/bottom edge (`x0 <= x < x0 + w`) so abutting
    /// regions never both match a shared edge pixel.
    #[must_use]
    pub fn hit(&self, x: f32, y: f32) -> Option<HitTarget> {
        self.regions
            .iter()
            .rev()
            .find(|([rx, ry, rw, rh], _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
            .map(|(_, target)| *target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map_never_hits() {
        let m = HitMap::new();
        assert!(m.is_empty());
        assert_eq!(m.hit(0.0, 0.0), None);
        assert_eq!(m.hit(10.0, 10.0), None);
    }

    #[test]
    fn hit_inside_and_miss_outside() {
        let mut m = HitMap::new();
        m.push([10.0, 20.0, 30.0, 40.0], HitTarget::SidebarToggle);
        // Inside.
        assert_eq!(m.hit(10.0, 20.0), Some(HitTarget::SidebarToggle)); // top-left corner (inclusive)
        assert_eq!(m.hit(25.0, 40.0), Some(HitTarget::SidebarToggle)); // interior
        assert_eq!(m.hit(39.9, 59.9), Some(HitTarget::SidebarToggle)); // near bottom-right
                                                                       // Outside on each side.
        assert_eq!(m.hit(9.9, 30.0), None); // left of
        assert_eq!(m.hit(40.0, 30.0), None); // right edge is exclusive
        assert_eq!(m.hit(25.0, 19.9), None); // above
        assert_eq!(m.hit(25.0, 60.0), None); // bottom edge is exclusive
    }

    #[test]
    fn overlap_returns_topmost_last_inserted() {
        let mut m = HitMap::new();
        // Two overlapping regions; the second is drawn on top.
        m.push([0.0, 0.0, 100.0, 100.0], HitTarget::BlockMeta(3));
        m.push([10.0, 10.0, 20.0, 20.0], HitTarget::CompletionRow(1));
        // In the overlap the topmost (last-inserted) wins.
        assert_eq!(m.hit(15.0, 15.0), Some(HitTarget::CompletionRow(1)));
        // Outside the top region but inside the bottom one, the bottom wins.
        assert_eq!(m.hit(50.0, 50.0), Some(HitTarget::BlockMeta(3)));
    }

    #[test]
    fn carries_the_target_payload() {
        let mut m = HitMap::new();
        m.push([0.0, 0.0, 10.0, 10.0], HitTarget::BlockMeta(7));
        m.push([0.0, 20.0, 10.0, 10.0], HitTarget::CompletionRow(4));
        assert_eq!(m.hit(5.0, 5.0), Some(HitTarget::BlockMeta(7)));
        assert_eq!(m.hit(5.0, 25.0), Some(HitTarget::CompletionRow(4)));
    }

    #[test]
    fn clear_empties_but_keeps_capacity() {
        let mut m = HitMap::new();
        m.push([0.0, 0.0, 10.0, 10.0], HitTarget::ModeChip);
        m.push([0.0, 20.0, 10.0, 10.0], HitTarget::SidebarToggle);
        assert_eq!(m.len(), 2);
        let cap = m.regions.capacity();
        m.clear();
        assert!(m.is_empty());
        assert_eq!(m.hit(5.0, 5.0), None);
        // Capacity is retained so the next rebuild is allocation-free.
        assert_eq!(m.regions.capacity(), cap);
    }

    #[test]
    fn zero_size_rect_never_hits() {
        let mut m = HitMap::new();
        m.push([10.0, 10.0, 0.0, 0.0], HitTarget::ModeChip);
        assert_eq!(m.hit(10.0, 10.0), None, "a degenerate rect matches nothing");
    }
}
