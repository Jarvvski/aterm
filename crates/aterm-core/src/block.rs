//! Block model: the Warp-style "command + its output" unit. A `Block` is opened
//! by OSC-133 `B`/`C` (command typed / output begins) and closed by `D` (with an
//! exit code). `BlockList` holds the session's blocks; `BlockSegmenter` drives
//! segmentation from the typed [`Mark`]s produced by [`crate::osc`].

use std::time::Instant;

use crate::osc::{Mark, PromptKind};

/// A half-open byte span `[start, end)` into the session's logical output stream.
/// `end == None` means the span is still open (command running).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSpan {
    pub start: usize,
    pub end: Option<usize>,
}

impl OutputSpan {
    fn open(start: usize) -> Self {
        Self { start, end: None }
    }
}

/// One command block.
#[derive(Debug, Clone)]
pub struct Block {
    /// The command line as typed (between OSC-133 `B` and `C`). May be empty if
    /// the shell did not report it or output started immediately.
    pub command: String,
    /// Byte span of this command's output in the logical stream.
    pub output_span: OutputSpan,
    /// Exit code from OSC-133 `D`, once the command finished.
    pub exit_code: Option<i32>,
    /// When the prompt for this block started (OSC-133 `A`).
    pub started_at: Instant,
    /// When the command finished (OSC-133 `D`).
    pub finished_at: Option<Instant>,
    /// Reported working directory at block start (from OSC-7), if known.
    pub cwd: Option<String>,
}

impl Block {
    /// Is this block still running (no `D` seen)?
    pub fn is_running(&self) -> bool {
        self.finished_at.is_none()
    }

    /// Did the command succeed (exit 0)? `None` until it finishes.
    pub fn succeeded(&self) -> Option<bool> {
        self.exit_code.map(|c| c == 0)
    }
}

/// The ordered list of blocks for a session.
#[derive(Debug, Default)]
pub struct BlockList {
    blocks: Vec<Block>,
}

impl BlockList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Block> {
        self.blocks.iter()
    }

    /// The most recent block, if any.
    pub fn last(&self) -> Option<&Block> {
        self.blocks.last()
    }

    fn last_mut(&mut self) -> Option<&mut Block> {
        self.blocks.last_mut()
    }

    fn push(&mut self, block: Block) {
        self.blocks.push(block);
    }
}

/// Coarse phase the segmenter is in, mirroring the OSC-133 lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Before any prompt, or after `D` and before the next `A`.
    Idle,
    /// Saw `A`: a prompt is being drawn; user is (about to be) typing.
    Prompt,
    /// Saw `B`: command input captured; awaiting `C`.
    Command,
    /// Saw `C`: command is running, output is accumulating.
    Output,
}

/// Drives [`BlockList`] from a stream of [`Mark`]s plus the running byte offset.
///
/// The caller advances `offset` as it appends bytes to the logical stream and
/// hands marks here in stream order. Command-text capture between `B` and `C`
/// is partial: we record the offset where it begins, and the app layer is
/// expected to fill `command` from the captured prompt-input bytes.
/// TODO(ticket EPIC-2): capture the literal command text between `B` and `C`
/// directly here once the input echo path feeds this module.
#[derive(Debug)]
pub struct BlockSegmenter {
    phase: Phase,
    /// Offset into the logical stream where the *current* command's input began
    /// (set at `B`), used to slice the command text later.
    command_start_offset: Option<usize>,
    /// CWD reported most recently via OSC-7, applied to the next opened block.
    pending_cwd: Option<String>,
}

impl Default for BlockSegmenter {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockSegmenter {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            command_start_offset: None,
            pending_cwd: None,
        }
    }

    /// Apply one mark at byte `offset` in the logical stream, mutating `list`.
    pub fn apply(&mut self, mark: &Mark, offset: usize, list: &mut BlockList) {
        match mark {
            Mark::Cwd(path) => {
                self.pending_cwd = Some(path.clone());
                // If a block is open, update its cwd too.
                if let Some(b) = list.last_mut() {
                    if b.is_running() {
                        b.cwd = Some(path.clone());
                    }
                }
            }
            Mark::Prompt(PromptKind::PromptStart) => {
                self.phase = Phase::Prompt;
                self.command_start_offset = None;
            }
            Mark::Prompt(PromptKind::CommandStart) => {
                // `B`: command input begins.
                self.phase = Phase::Command;
                self.command_start_offset = Some(offset);
            }
            Mark::Prompt(PromptKind::OutputStart) => {
                // `C`: a new block opens; its output span starts here.
                self.phase = Phase::Output;
                list.push(Block {
                    command: String::new(),
                    output_span: OutputSpan::open(offset),
                    exit_code: None,
                    started_at: Instant::now(),
                    finished_at: None,
                    cwd: self.pending_cwd.clone(),
                });
            }
            Mark::Prompt(PromptKind::CommandDone { exit_code }) => {
                // `D`: close the current block.
                if let Some(b) = list.last_mut() {
                    if b.is_running() {
                        b.output_span.end = Some(offset);
                        b.exit_code = *exit_code;
                        b.finished_at = Some(Instant::now());
                    }
                }
                self.phase = Phase::Idle;
                self.command_start_offset = None;
            }
        }
    }

    /// Set the command text for the most recently opened (running) block. The app
    /// layer calls this once it has sliced the input echo between `B` and `C`.
    pub fn set_last_command(&self, list: &mut BlockList, command: impl Into<String>) {
        if let Some(b) = list.last_mut() {
            b.command = command.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> Mark {
        Mark::Prompt(PromptKind::PromptStart)
    }
    fn b() -> Mark {
        Mark::Prompt(PromptKind::CommandStart)
    }
    fn c() -> Mark {
        Mark::Prompt(PromptKind::OutputStart)
    }
    fn d(code: i32) -> Mark {
        Mark::Prompt(PromptKind::CommandDone {
            exit_code: Some(code),
        })
    }

    #[test]
    fn full_lifecycle_creates_one_closed_block() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();

        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 10, &mut list); // command input at 10
        seg.apply(&c(), 17, &mut list); // output starts at 17
        seg.apply(&d(0), 42, &mut list); // done at 42

        assert_eq!(list.len(), 1);
        let blk = list.last().unwrap();
        assert_eq!(blk.output_span.start, 17);
        assert_eq!(blk.output_span.end, Some(42));
        assert_eq!(blk.exit_code, Some(0));
        assert_eq!(blk.succeeded(), Some(true));
        assert!(!blk.is_running());
    }

    #[test]
    fn block_is_running_before_done() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 5, &mut list);
        seg.apply(&c(), 8, &mut list);
        let blk = list.last().unwrap();
        assert!(blk.is_running());
        assert_eq!(blk.output_span.end, None);
        assert_eq!(blk.succeeded(), None);
    }

    #[test]
    fn two_commands_make_two_blocks() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        for (off, m) in [
            (0, a()),
            (3, b()),
            (6, c()),
            (20, d(0)),
            (21, a()),
            (24, b()),
            (27, c()),
            (50, d(1)),
        ] {
            seg.apply(&m, off, &mut list);
        }
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().next().unwrap().exit_code, Some(0));
        assert_eq!(list.iter().nth(1).unwrap().exit_code, Some(1));
        assert_eq!(list.iter().nth(1).unwrap().succeeded(), Some(false));
    }

    #[test]
    fn cwd_mark_applied_to_open_block() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&Mark::Cwd("/home/me".into()), 0, &mut list);
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list);
        assert_eq!(list.last().unwrap().cwd.as_deref(), Some("/home/me"));
    }

    #[test]
    fn set_last_command_fills_text() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.set_last_command(&mut list, "ls -la");
        assert_eq!(list.last().unwrap().command, "ls -la");
    }
}
