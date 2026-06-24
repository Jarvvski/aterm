//! aterm-core — engine: PTY, VT/grid, block model, OSC-133/OSC-7 marks.
//!
//! No UI, no LLM. This crate is the headless terminal engine: it spawns a shell
//! over a PTY ([`pty`]), parses the byte stream through `alacritty_terminal`
//! ([`terminal`]), intercepts shell-integration marks ([`osc`]), and segments the
//! stream into command blocks ([`block`]), owns the pure unified-input reducer
//! ([`input`]), and keeps the shared input-history ring ([`history`]). Everything
//! above (rendering, agent) consumes these types.

pub mod block;
pub mod engine;
pub mod history;
pub mod input;
pub mod integration;
pub mod osc;
pub mod pty;
pub mod shell_integration;
pub mod terminal;

// Re-export the load-bearing public types at the crate root for ergonomics.
pub use block::{
    Block, BlockList, BlockSegmenter, HeightIndex, HeuristicSegmenter, OutputSpan, RowSnapshot,
    COLLAPSED_OUTPUT_ROWS,
};
pub use engine::{Engine, EngineMetrics, ToModel};
pub use history::{HistoryEntry, HistoryRing, HistoryScope, Recall, DEFAULT_HISTORY_CAP};
pub use input::{
    GhostText, Highlight, InputEvent, InputMode, InputModel, Motion, Preedit, Selection,
};
pub use integration::{Integration, IntegrationMonitor, IntegrationReason, IntegrationStatus};
pub use osc::{Mark, OscScanner, PromptKind, ScanResult};
pub use pty::{Pty, PtyDimensions, PtyError, PtyEvent, Signal};
pub use shell_integration::{IntegrationDir, ShellKind, ShimNonce};
pub use terminal::{
    CellColor, CursorPos, Damage, LineDamage, Snapshot, SnapshotCell, Terminal, TerminalEvent,
    DEFAULT_SCROLLBACK,
};
