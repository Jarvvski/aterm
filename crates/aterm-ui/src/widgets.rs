//! Widget stubs: the timeline / block / input widgets that compose the aterm
//! surface. These are structural placeholders — the real layout + render is
//! EPIC-4 (design system) and EPIC-3 (unified input). They exist so the app's
//! composition tree has stable types to reference.

use aterm_core::Block;

/// The scrolling timeline of command [`Block`]s and agent cards.
/// TODO(ticket EPIC-4): real layout (vertical rhythm from `aterm_tokens::space`),
/// block cards, agent cards, and the hairline separators.
#[derive(Default)]
pub struct Timeline;

impl Timeline {
    pub fn new() -> Self {
        Self
    }
    /// Number of laid-out rows for `blocks` (stub: one per block).
    pub fn measure(&self, blocks: &[Block]) -> usize {
        blocks.len()
    }
}

/// A single command block widget (header chip + output body + gate badge).
/// TODO(ticket EPIC-4): render header (cwd, exit badge, duration) + body.
#[derive(Default)]
pub struct BlockWidget;

impl BlockWidget {
    pub fn new() -> Self {
        Self
    }
}

/// The unified input widget (the single field that is Shell OR Agent mode).
/// TODO(ticket EPIC-3): caret, mode chip, routing affordance, completion.
#[derive(Default)]
pub struct InputWidget;

impl InputWidget {
    pub fn new() -> Self {
        Self
    }
}
