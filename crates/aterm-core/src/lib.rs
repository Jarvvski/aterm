//! aterm-core — engine: PTY, VT/grid, block model, OSC-133/OSC-7 marks.
//!
//! No UI, no LLM. This crate is the headless terminal engine: it spawns a shell
//! over a PTY ([`pty`]), parses the byte stream through `alacritty_terminal`
//! ([`terminal`]), intercepts shell-integration marks ([`osc`]), and segments the
//! stream into command blocks ([`block`]), and owns the pure unified-input reducer
//! ([`input`]). Everything above (rendering, agent) consumes these types.

pub mod block;
pub mod engine;
pub mod input;
pub mod osc;
pub mod pty;
pub mod shell_integration;
pub mod terminal;

// Re-export the load-bearing public types at the crate root for ergonomics.
pub use block::{Block, BlockList, BlockSegmenter, OutputSpan};
pub use engine::{Engine, EngineMetrics, ToModel};
pub use input::{InputEvent, InputMode, InputModel, InputOutcome};
pub use osc::{Mark, OscScanner, PromptKind, ScanResult};
pub use pty::{Pty, PtyDimensions, PtyError, PtyEvent};
pub use terminal::{
    CellColor, CursorPos, Damage, LineDamage, Snapshot, SnapshotCell, Terminal, TerminalEvent,
    DEFAULT_SCROLLBACK,
};
