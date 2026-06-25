//! The unified-input reducer (ticket T-3.1). aterm has ONE input field that is
//! either in `Shell` mode (the finished line is committed to the PTY) or `Agent`
//! mode (the line is a prompt to the LLM). The headline behavior: the mode-toggle
//! hotkey mutates ONLY the `mode` - the typed text, selection, and undo history are
//! preserved across the toggle by construction (see ADR-0004). You can start typing
//! a command, realize you want the agent, hit the hotkey, and your text is intact.
//!
//! This module is a **pure editor**: it owns the in-progress command line and the
//! editing model (caret, selection, motions, undo) and nothing else. It performs no
//! I/O, never interprets the buffer's contents, and - critically - **does not decide
//! whether Enter submits**. Submission is the caller's job: the routing brain
//! (ticket T-3.3) reads [`InputModel::text`] and then calls [`InputModel::take`] to
//! reset. This caller-owns-submit split is the property ADR-0004 ports verbatim from
//! the prototype's `CommandBuffer`, and it is why a pasted `"\n; rm -rf /"` is inert
//! literal text rather than an executed command.
//!
//! Because the logic is pure (no UI, no LLM) it lives in `aterm-core`: both
//! `aterm-ui` (the input widget that renders it, T-3.6) and `aterm-app` (the routing
//! brain, T-3.3) consume it, and `aterm-ui` cannot depend on `aterm-app` (the arrow
//! is app -> ui).

/// Maximum undo depth. The buffer is one command line, so this is generous; it
/// only bounds memory if a user types an enormous amount without submitting.
const MAX_UNDO: usize = 1024;

/// Which surface the input is currently driving. The hotkey flips ONLY this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// The finished line is committed to the shell PTY.
    #[default]
    Shell,
    /// The line is composed as an agent prompt.
    Agent,
}

/// A caret with an optional selection anchor, as char offsets into the text. When
/// `anchor == caret` the selection is collapsed (a plain caret). Multi-caret is a
/// later concern; v1 is a single primary selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    /// The fixed end of the selection (where it started).
    pub anchor: usize,
    /// The moving end of the selection (where the caret is).
    pub caret: usize,
}

impl Selection {
    /// A collapsed selection (caret) at `pos`.
    pub fn at(pos: usize) -> Self {
        Self {
            anchor: pos,
            caret: pos,
        }
    }

    /// True when nothing is selected (anchor and caret coincide).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.caret
    }

    /// The lower char offset of the selection.
    pub fn start(&self) -> usize {
        self.anchor.min(self.caret)
    }

    /// The upper char offset of the selection.
    pub fn end(&self) -> usize {
        self.anchor.max(self.caret)
    }
}

/// A cursor motion. Horizontal/word/line motions clear the vertical "goal column";
/// `Up`/`Down` set and preserve it so vertical travel keeps a remembered column
/// across shorter lines (standard editor behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One char left / right.
    Left,
    Right,
    /// To the start of the previous / next whitespace-delimited token.
    WordLeft,
    WordRight,
    /// To the start / end of the current line.
    Home,
    End,
    /// To the start / end of the whole buffer.
    BufferStart,
    BufferEnd,
    /// Up / down one line, preserving the goal column.
    Up,
    Down,
}

/// Events the reducer understands. All are pure state transitions; none performs
/// I/O or interprets the text. Note there is no `Submit` event - submission is the
/// caller reading [`InputModel::text`] then calling [`InputModel::take`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// Insert a string at the caret as ONE undo unit (a paste is one event). If a
    /// selection is active it is replaced. Embedded newlines/control chars are
    /// literal and inert.
    Insert(String),
    /// Delete the char before the caret, or the selection if one is active.
    Backspace,
    /// Delete the char at the caret, or the selection if one is active.
    Delete,
    /// Move the caret. The bool extends the selection (anchor held) when `true`.
    Move(Motion, bool),
    /// Undo the last edit unit.
    Undo,
    /// Redo the last undone edit unit.
    Redo,
    /// Toggle Shell <-> Agent WITHOUT touching text/selection/undo.
    ToggleMode,
}

/// Active IME composition. Reserved for ticket T-3.2 (IME via winit `Ime` events);
/// the cursor range uses winit's byte indices. Not populated by this ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preedit {
    /// The in-progress composition string.
    pub text: String,
    /// The candidate cursor range within `text` (byte indices), per winit.
    pub cursor: Option<(usize, usize)>,
}

/// The visual category of a highlighted span - maps to a token color / decoration
/// in the renderer (T-3.6). The dossier names command/arg/flag tinting plus an
/// error underline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// The command word (first token of a pipeline stage / after a separator).
    Command,
    /// A positional argument.
    Argument,
    /// An option/flag token (`-x` / `--long`).
    Flag,
    /// A quoted string (`'...'` or `"..."`).
    QuotedString,
    /// A shell operator/separator (`|`, `&`, `;`, `&&`, `||`, `<`, `>`).
    Operator,
    /// An error decoration (e.g. an unterminated quote) - rendered as an underline.
    ErrorUnderline,
}

/// A styled run over `[start, end)` CHAR offsets (the same units as [`Selection`]).
/// Spans never overlap and are emitted left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleSpan {
    /// First char offset (inclusive).
    pub start: usize,
    /// One-past-last char offset (exclusive).
    pub end: usize,
    pub kind: SpanKind,
}

/// Non-inheritable style spans (syntax highlight + error underlines), computed async
/// off the render loop (ticket T-3.5) by [`crate::highlight`] and applied via
/// [`InputModel::set_highlight`]. "Non-inheritable": the span set is recomputed from
/// the whole text each time, so a character typed after a styled run is reclassified,
/// never tinted by the preceding token. Empty by default (no overlay computed yet).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Highlight {
    /// The styled runs, left to right, non-overlapping.
    pub spans: Vec<StyleSpan>,
}

/// A fish-style ghost-text suggestion (ticket T-3.5): the FULL suggested command
/// line (e.g. `git status -s`), not just the trailing fragment. The visible tail is
/// derived live by [`InputModel::ghost_tail`] as the suggestion with the current
/// text stripped as a prefix, so a suggestion the buffer has since diverged from
/// (the worker is debounced, so the text can advance ahead of the next recompute)
/// neither displays nor accepts - it is simply no longer a prefix. Computed by
/// [`crate::highlight::ghost_for`] and applied via [`InputModel::set_ghost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostText {
    /// The full suggested command line. The shown tail is this minus the current
    /// input prefix (see [`InputModel::ghost_tail`]).
    pub suggestion: String,
}

/// A point-in-time copy of the editable state, for undo/redo.
#[derive(Debug, Clone)]
struct Snapshot {
    text: String,
    sel: Selection,
}

/// The pure unified-input reducer: text + selection + mode, plus undo history and a
/// vertical goal column. See the module docs for the caller-owns-submit contract.
#[derive(Debug, Clone, Default)]
pub struct InputModel {
    /// Characters only; never interpreted.
    text: String,
    /// Primary caret + optional anchor, as char offsets.
    sel: Selection,
    /// Where Enter routes; flipped by the hotkey, text untouched.
    mode: InputMode,
    /// Remembered column for vertical motion; `None` resets it.
    goal_col: Option<usize>,
    /// Undo stack (oldest first); each entry is one edit unit.
    undo: Vec<Snapshot>,
    /// Redo stack; cleared by any new edit.
    redo: Vec<Snapshot>,
    /// Reserved for T-3.2 (IME); not populated here.
    preedit: Option<Preedit>,
    /// Reserved for T-3.5 (async highlight overlay); empty here.
    overlay: Highlight,
    /// Reserved for T-3.5 (ghost text); not populated here.
    ghost: Option<GhostText>,
}

impl InputModel {
    /// A fresh empty model in Shell mode.
    pub fn new() -> Self {
        Self::default()
    }

    // --- read accessors -----------------------------------------------------

    /// The current text (consumed by the input widget renderer in `aterm-ui`).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The current selection (caret + anchor).
    pub fn selection(&self) -> Selection {
        self.sel
    }

    /// The caret position as a char offset (the moving end of the selection).
    pub fn caret(&self) -> usize {
        self.sel.caret
    }

    /// The current routing target.
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// True when the buffer holds no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The active IME composition, if any (reserved for T-3.2).
    pub fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }

    /// The async style overlay (reserved for T-3.5).
    pub fn highlight(&self) -> &Highlight {
        &self.overlay
    }

    /// The async ghost-text suggestion, if any (reserved for T-3.5).
    pub fn ghost(&self) -> Option<&GhostText> {
        self.ghost.as_ref()
    }

    // --- T-3.5 overlay mutators (driven by the async highlight/ghost worker) ---

    /// Replace the style overlay. The async worker (aterm-ui) computes spans off the
    /// render thread (via [`crate::highlight::highlight_for`]) and applies the
    /// last-good set here; the render path only ever *reads* [`Self::highlight`], so
    /// it never blocks on the highlighter.
    pub fn set_highlight(&mut self, highlight: Highlight) {
        self.overlay = highlight;
    }

    /// Set (or clear) the ghost-text suggestion. The worker computes it from history
    /// ([`crate::highlight::ghost_for`]) and applies it here. Staleness is handled by
    /// [`Self::ghost_tail`]/[`Self::accept_ghost`] re-deriving the tail against the
    /// live text, so the worker need not race to clear an out-of-date suggestion.
    pub fn set_ghost(&mut self, ghost: Option<GhostText>) {
        self.ghost = ghost;
    }

    /// The visible ghost tail: the suggestion with the current text stripped as a
    /// prefix, or `None` when there is no ghost, the line is empty, or the buffer has
    /// diverged from the suggestion (so it is no longer a prefix). This is what the
    /// widget renders after the caret, and the single source of truth for whether a
    /// suggestion is still live - a debounced worker can lag without ever showing a
    /// stale tail.
    pub fn ghost_tail(&self) -> Option<&str> {
        if self.text.is_empty() {
            return None;
        }
        let tail = self
            .ghost
            .as_ref()?
            .suggestion
            .strip_prefix(self.text.as_str())?;
        if tail.is_empty() {
            None
        } else {
            Some(tail)
        }
    }

    /// Accept the ghost-text suggestion (zsh-autosuggestions semantics: `Right`/`End`
    /// at end of line). Inserts the live tail as one undo unit and clears the ghost;
    /// returns whether anything was accepted. It is a no-op unless the selection is
    /// collapsed, the caret is at the end of the text, AND a live tail exists
    /// ([`Self::ghost_tail`]) - so it never fires mid-line, over a selection, or for a
    /// stale suggestion the buffer has typed past. The widget (T-3.6) binds the keys;
    /// this is the pure operation they invoke.
    pub fn accept_ghost(&mut self) -> bool {
        if !(self.sel.is_empty() && self.sel.caret == self.char_len()) {
            return false;
        }
        // Re-derive the tail against the live buffer, so a stale ghost (the worker is
        // debounced; the text may have advanced past it) is never appended verbatim.
        let tail = match self.ghost_tail() {
            Some(t) => t.to_string(),
            None => return false,
        };
        self.ghost = None;
        self.insert(&tail);
        true
    }

    // --- the reducer --------------------------------------------------------

    /// Apply an event to the model. Pure: no I/O, no interpretation of the text.
    pub fn reduce(&mut self, event: InputEvent) {
        match event {
            InputEvent::Insert(s) => self.insert(&s),
            InputEvent::Backspace => self.backspace(),
            InputEvent::Delete => self.delete_forward(),
            InputEvent::Move(motion, extend) => self.move_caret(motion, extend),
            InputEvent::Undo => self.undo(),
            InputEvent::Redo => self.redo(),
            InputEvent::ToggleMode => self.toggle_mode(),
        }
    }

    /// Flip Shell <-> Agent. Provably touches ONLY `mode` - text, selection, and
    /// undo history are untouched. This is the structural fix for the prototype's
    /// context-clearing toggle (ADR-0004).
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            InputMode::Shell => InputMode::Agent,
            InputMode::Agent => InputMode::Shell,
        };
    }

    /// The submit/reset mechanism: return the current text and reset the model to
    /// empty (selection, undo, redo, goal column, preedit all cleared). The mode is
    /// preserved. The caller (the routing brain, T-3.3) decides *when* to call this;
    /// the buffer never decides whether Enter submits.
    pub fn take(&mut self) -> String {
        let line = std::mem::take(&mut self.text);
        self.sel = Selection::default();
        self.goal_col = None;
        self.undo.clear();
        self.redo.clear();
        self.preedit = None;
        // The suggestion was for the now-submitted line; drop it so it cannot carry
        // onto the fresh empty buffer (the worker recomputes for the new line).
        self.ghost = None;
        line
    }

    /// Reset the model to empty, discarding the text (see [`take`](Self::take)).
    pub fn reset(&mut self) {
        let _ = self.take();
    }

    // --- edits --------------------------------------------------------------

    fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        // One undo unit covers the whole replace (delete-selection + insert), so a
        // paste over a selection undoes in one step.
        self.push_undo();
        self.delete_selection();
        let at = self.byte_of(self.sel.caret);
        self.text.insert_str(at, s);
        self.sel = Selection::at(self.sel.caret + s.chars().count());
        self.goal_col = None;
    }

    fn backspace(&mut self) {
        if !self.sel.is_empty() {
            self.push_undo();
            self.delete_selection();
            self.goal_col = None;
            return;
        }
        if self.sel.caret == 0 {
            return;
        }
        self.push_undo();
        let c = self.sel.caret;
        let (start, end) = (self.byte_of(c - 1), self.byte_of(c));
        self.text.replace_range(start..end, "");
        self.sel = Selection::at(c - 1);
        self.goal_col = None;
    }

    fn delete_forward(&mut self) {
        if !self.sel.is_empty() {
            self.push_undo();
            self.delete_selection();
            self.goal_col = None;
            return;
        }
        if self.sel.caret >= self.char_len() {
            return;
        }
        self.push_undo();
        let c = self.sel.caret;
        let (start, end) = (self.byte_of(c), self.byte_of(c + 1));
        self.text.replace_range(start..end, "");
        self.goal_col = None;
    }

    /// Remove the selected range (if any) and collapse the caret to its start. Does
    /// NOT push undo - callers wrap it in a single undo unit.
    fn delete_selection(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        let (a, b) = (self.sel.start(), self.sel.end());
        let (ba, bb) = (self.byte_of(a), self.byte_of(b));
        self.text.replace_range(ba..bb, "");
        self.sel = Selection::at(a);
    }

    // --- undo / redo --------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            sel: self.sel,
        }
    }

    fn push_undo(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.text = prev.text;
            self.sel = prev.sel;
            self.goal_col = None;
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.text = next.text;
            self.sel = next.sel;
            self.goal_col = None;
        }
    }

    // --- motions ------------------------------------------------------------

    fn move_caret(&mut self, motion: Motion, extend: bool) {
        if !matches!(motion, Motion::Up | Motion::Down) {
            self.goal_col = None;
        }
        let len = self.char_len();
        let caret = self.sel.caret;
        let target = match motion {
            Motion::Left => {
                if !extend && !self.sel.is_empty() {
                    self.sel.start()
                } else {
                    caret.saturating_sub(1)
                }
            }
            Motion::Right => {
                if !extend && !self.sel.is_empty() {
                    self.sel.end()
                } else {
                    (caret + 1).min(len)
                }
            }
            Motion::WordLeft => self.word_left(caret),
            Motion::WordRight => self.word_right(caret),
            Motion::Home => self.line_start(caret),
            Motion::End => self.line_end(caret),
            Motion::BufferStart => 0,
            Motion::BufferEnd => len,
            Motion::Up => self.vertical(caret, false),
            Motion::Down => self.vertical(caret, true),
        };
        self.sel.caret = target;
        if !extend {
            self.sel.anchor = target;
        }
    }

    fn word_left(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = pos.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn word_right(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let mut i = pos.min(n);
        while i < n && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    fn line_start(&self, pos: usize) -> usize {
        let (line, _) = self.line_col(pos);
        self.lines()[line].0
    }

    fn line_end(&self, pos: usize) -> usize {
        let (line, _) = self.line_col(pos);
        let (start, len) = self.lines()[line];
        start + len
    }

    /// Move up (`down == false`) or down one line, preserving the goal column.
    fn vertical(&mut self, pos: usize, down: bool) -> usize {
        let lines = self.lines();
        let (line, col) = self.line_col(pos);
        let goal = *self.goal_col.get_or_insert(col);
        if down {
            if line + 1 >= lines.len() {
                self.char_len()
            } else {
                let (start, line_len) = lines[line + 1];
                start + goal.min(line_len)
            }
        } else if line == 0 {
            0
        } else {
            let (start, line_len) = lines[line - 1];
            start + goal.min(line_len)
        }
    }

    // --- char/line geometry (the buffer is tiny; O(n) scans are fine) -------

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset for a given char index (clamped to the end).
    fn byte_of(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// The (line, column) of a char offset.
    fn line_col(&self, pos: usize) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for ch in self.text.chars().take(pos) {
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Per-line `(start_char_offset, char_len_excluding_newline)`. Always non-empty
    /// (an empty buffer yields one zero-length line).
    fn lines(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut count = 0;
        for (i, ch) in self.text.chars().enumerate() {
            if ch == '\n' {
                out.push((start, count));
                start = i + 1;
                count = 0;
            } else {
                count += 1;
            }
        }
        out.push((start, count));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A small deterministic LCG so the toggle property test can drive "any
    // sequence of edits" without a proptest dependency.
    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    #[test]
    fn toggle_mutates_only_mode() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("git pu".into()));
        assert_eq!(m.mode(), InputMode::Shell);
        m.reduce(InputEvent::ToggleMode);
        assert_eq!(m.mode(), InputMode::Agent);
        assert_eq!(m.text(), "git pu");
        m.reduce(InputEvent::ToggleMode);
        assert_eq!(m.mode(), InputMode::Shell);
    }

    #[test]
    fn toggle_preserves_text_and_selection_after_any_edits() {
        for seed in 0..64u64 {
            let mut st = seed.wrapping_add(1);
            let mut m = InputModel::new();
            for _ in 0..40 {
                match lcg(&mut st) % 9 {
                    0 => m.reduce(InputEvent::Insert("a".into())),
                    1 => m.reduce(InputEvent::Insert("é ".into())),
                    2 => m.reduce(InputEvent::Insert("x\ny".into())),
                    3 => m.reduce(InputEvent::Backspace),
                    4 => m.reduce(InputEvent::Delete),
                    5 => m.reduce(InputEvent::Move(
                        Motion::Left,
                        lcg(&mut st).is_multiple_of(2),
                    )),
                    6 => m.reduce(InputEvent::Move(
                        Motion::Right,
                        lcg(&mut st).is_multiple_of(2),
                    )),
                    7 => m.reduce(InputEvent::Move(Motion::WordLeft, false)),
                    _ => m.reduce(InputEvent::Undo),
                }
            }
            let text_before = m.text().to_string();
            let sel_before = m.selection();
            let mode_before = m.mode();

            m.reduce(InputEvent::ToggleMode);
            assert_eq!(m.text(), text_before, "seed {seed}: text changed on toggle");
            assert_eq!(
                m.selection(),
                sel_before,
                "seed {seed}: selection changed on toggle"
            );
            assert_ne!(m.mode(), mode_before, "seed {seed}: mode did not flip");

            m.reduce(InputEvent::ToggleMode);
            assert_eq!(m.mode(), mode_before);
            assert_eq!(m.text(), text_before);
            assert_eq!(m.selection(), sel_before);
        }
    }

    #[test]
    fn paste_is_inert_literal_and_one_undo_unit() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("echo hi".into()));
        // A paste with an embedded newline and a destructive-looking tail.
        m.reduce(InputEvent::Insert("\n; rm -rf /".into()));
        // Stored verbatim - the newline is a literal, inert character, never an
        // execution boundary (the model does not interpret the buffer at all).
        assert_eq!(m.text(), "echo hi\n; rm -rf /");
        assert!(m.text().contains('\n'));
        // The whole paste is ONE undo unit.
        m.reduce(InputEvent::Undo);
        assert_eq!(m.text(), "echo hi");
    }

    #[test]
    fn word_home_end_motions() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("git commit --amend".into()));
        m.reduce(InputEvent::Move(Motion::WordLeft, false));
        assert_eq!(m.caret(), 11, "start of --amend");
        m.reduce(InputEvent::Move(Motion::WordLeft, false));
        assert_eq!(m.caret(), 4, "start of commit");
        m.reduce(InputEvent::Move(Motion::Home, false));
        assert_eq!(m.caret(), 0);
        m.reduce(InputEvent::Move(Motion::WordRight, false));
        assert_eq!(m.caret(), 4, "start of next token");
        m.reduce(InputEvent::Move(Motion::End, false));
        assert_eq!(m.caret(), 18);
    }

    #[test]
    fn vertical_motion_with_column_memory() {
        let mut m = InputModel::new();
        // line0 "abcdef" (0..6), line1 "ij" (7..9), line2 "klmnop" (10..16)
        m.reduce(InputEvent::Insert("abcdef\nij\nklmnop".into()));
        assert_eq!(m.caret(), 16);
        // Up to line1: goal col 6, clamped to line len 2 -> offset 9.
        m.reduce(InputEvent::Move(Motion::Up, false));
        assert_eq!(m.caret(), 9);
        // Up to line0: goal col 6 remembered -> offset 6.
        m.reduce(InputEvent::Move(Motion::Up, false));
        assert_eq!(m.caret(), 6);
        // Down to line1: goal still 6, clamped to 2 -> offset 9.
        m.reduce(InputEvent::Move(Motion::Down, false));
        assert_eq!(m.caret(), 9);
        // Down to line2: goal 6 fits -> offset 16.
        m.reduce(InputEvent::Move(Motion::Down, false));
        assert_eq!(m.caret(), 16);
        // A horizontal motion resets the goal column.
        m.reduce(InputEvent::Move(Motion::Left, false)); // -> 15
        m.reduce(InputEvent::Move(Motion::Up, false)); // line1, col min(5,2)=2 -> 9
        assert_eq!(m.caret(), 9);
    }

    #[test]
    fn selection_replace_and_delete_are_single_units() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("hello".into()));
        m.reduce(InputEvent::Move(Motion::Home, false));
        m.reduce(InputEvent::Move(Motion::Right, true));
        m.reduce(InputEvent::Move(Motion::Right, true));
        assert_eq!(
            m.selection(),
            Selection {
                anchor: 0,
                caret: 2
            }
        );
        // Typing over a selection replaces it as one undo unit.
        m.reduce(InputEvent::Insert("X".into()));
        assert_eq!(m.text(), "Xllo");
        assert_eq!(m.caret(), 1);
        m.reduce(InputEvent::Undo);
        assert_eq!(m.text(), "hello");
        // Backspace over a selection deletes the whole selection.
        m.reduce(InputEvent::Move(Motion::Home, false));
        m.reduce(InputEvent::Move(Motion::Right, true));
        m.reduce(InputEvent::Move(Motion::Right, true));
        m.reduce(InputEvent::Backspace);
        assert_eq!(m.text(), "llo");
        assert_eq!(m.caret(), 0);
    }

    #[test]
    fn non_extending_arrow_collapses_selection_to_edge() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("hello".into()));
        m.reduce(InputEvent::Move(Motion::Home, false));
        m.reduce(InputEvent::Move(Motion::Right, true));
        m.reduce(InputEvent::Move(Motion::Right, true)); // select [0,2)
        m.reduce(InputEvent::Move(Motion::Left, false)); // collapse to start
        assert_eq!(
            m.selection(),
            Selection {
                anchor: 0,
                caret: 0
            }
        );
        m.reduce(InputEvent::Move(Motion::End, false));
        m.reduce(InputEvent::Move(Motion::Left, true));
        m.reduce(InputEvent::Move(Motion::Left, true)); // select [5,3)
        m.reduce(InputEvent::Move(Motion::Right, false)); // collapse to end (5)
        assert_eq!(
            m.selection(),
            Selection {
                anchor: 5,
                caret: 5
            }
        );
    }

    #[test]
    fn take_returns_prior_text_and_resets() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::ToggleMode); // Agent
        m.reduce(InputEvent::Insert("what failed?".into()));
        let line = m.take();
        assert_eq!(line, "what failed?");
        assert_eq!(m.text(), "");
        assert_eq!(m.caret(), 0);
        assert!(m.is_empty());
        assert_eq!(m.mode(), InputMode::Agent, "mode preserved across submit");
        // History is cleared, so undo after submit is a no-op.
        m.reduce(InputEvent::Undo);
        assert_eq!(m.text(), "");
    }

    #[test]
    fn undo_redo_roundtrip_and_new_edit_clears_redo() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ab".into()));
        m.reduce(InputEvent::Insert("cd".into()));
        m.reduce(InputEvent::Undo);
        assert_eq!(m.text(), "ab");
        m.reduce(InputEvent::Undo);
        assert_eq!(m.text(), "");
        m.reduce(InputEvent::Redo);
        assert_eq!(m.text(), "ab");
        m.reduce(InputEvent::Redo);
        assert_eq!(m.text(), "abcd");
        // A fresh edit clears the redo stack.
        m.reduce(InputEvent::Undo); // -> "ab"
        m.reduce(InputEvent::Insert("X".into())); // -> "abX"
        m.reduce(InputEvent::Redo); // no-op
        assert_eq!(m.text(), "abX");
    }

    #[test]
    fn unicode_caret_and_backspace() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("café".into())); // 4 chars, 5 bytes
        assert_eq!(m.caret(), 4);
        m.reduce(InputEvent::Backspace); // removes 'é'
        assert_eq!(m.text(), "caf");
        assert_eq!(m.caret(), 3);
        m.reduce(InputEvent::Insert("é".into())); // "café"
        m.reduce(InputEvent::Move(Motion::Left, false)); // before 'é'
        assert_eq!(m.caret(), 3);
        m.reduce(InputEvent::Insert("X".into())); // between 'f' and 'é'
        assert_eq!(m.text(), "cafXé");
    }

    #[test]
    fn edits_on_empty_buffer_are_safe() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Backspace);
        m.reduce(InputEvent::Delete);
        m.reduce(InputEvent::Move(Motion::Left, false));
        m.reduce(InputEvent::Move(Motion::Up, false));
        m.reduce(InputEvent::Move(Motion::WordRight, false));
        m.reduce(InputEvent::Undo);
        m.reduce(InputEvent::Redo);
        assert_eq!(m.text(), "");
        assert_eq!(m.caret(), 0);
    }

    #[test]
    fn reserved_overlays_default_empty() {
        let m = InputModel::new();
        assert!(m.preedit().is_none());
        assert!(m.ghost().is_none());
        assert_eq!(m.highlight(), &Highlight::default());
    }

    #[test]
    fn set_highlight_and_ghost_round_trip_through_accessors() {
        let mut m = InputModel::new();
        let h = Highlight {
            spans: vec![StyleSpan {
                start: 0,
                end: 2,
                kind: SpanKind::Command,
            }],
        };
        m.set_highlight(h.clone());
        assert_eq!(m.highlight(), &h);
        m.set_ghost(Some(GhostText {
            suggestion: "git --all".to_string(),
        }));
        assert_eq!(m.ghost().map(|g| g.suggestion.as_str()), Some("git --all"));
        m.set_ghost(None);
        assert!(m.ghost().is_none());
    }

    #[test]
    fn ghost_tail_is_derived_live_and_hides_on_divergence_or_empty() {
        let mut m = InputModel::new();
        m.set_ghost(Some(GhostText {
            suggestion: "git status".to_string(),
        }));
        // Empty line: no tail shown even with a ghost set.
        assert_eq!(m.ghost_tail(), None, "no suggestion on a blank line");
        // A matching prefix shows the remaining tail.
        m.reduce(InputEvent::Insert("git st".to_string()));
        assert_eq!(m.ghost_tail(), Some("atus"));
        // Diverge from the suggestion: the tail disappears (no longer a prefix).
        m.reduce(InputEvent::Insert("x".to_string()));
        assert_eq!(m.text(), "git stx");
        assert_eq!(
            m.ghost_tail(),
            None,
            "a diverged buffer shows no stale tail"
        );
    }

    #[test]
    fn accept_ghost_inserts_the_live_tail_at_end_of_line_and_clears_it() {
        // AC2: the suggestion is accepted (the live tail is inserted) at end of line.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("git st".to_string()));
        m.set_ghost(Some(GhostText {
            suggestion: "git status".to_string(),
        }));
        assert!(m.accept_ghost(), "a live ghost at end of line is accepted");
        assert_eq!(m.text(), "git status");
        assert!(m.ghost().is_none(), "accepting clears the ghost");
        assert_eq!(
            m.caret(),
            "git status".chars().count(),
            "caret follows the insert"
        );
    }

    #[test]
    fn accept_ghost_rejects_a_stale_suggestion_the_buffer_typed_past() {
        // Regression (review finding): the worker is debounced, so the buffer can
        // advance past a ghost before it is recomputed. Accepting must NOT append
        // the stale tail verbatim ("git stash" + "atus" -> "git stashatus").
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("git st".to_string()));
        m.set_ghost(Some(GhostText {
            suggestion: "git status".to_string(),
        }));
        m.reduce(InputEvent::Insert("ash".to_string())); // buffer is now "git stash"
        assert_eq!(m.text(), "git stash");
        assert_eq!(
            m.ghost_tail(),
            None,
            "the stale suggestion is no longer a prefix"
        );
        assert!(!m.accept_ghost(), "a stale ghost must not be accepted");
        assert_eq!(m.text(), "git stash", "no stale tail appended");
    }

    #[test]
    fn accept_ghost_is_a_noop_mid_line_or_with_no_ghost() {
        // No ghost -> nothing to accept.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ls".to_string()));
        assert!(!m.accept_ghost());
        assert_eq!(m.text(), "ls");
        // Ghost present but caret NOT at end (moved left) -> not accepted, ghost kept.
        m.set_ghost(Some(GhostText {
            suggestion: "ls -la".to_string(),
        }));
        m.reduce(InputEvent::Move(Motion::Left, false));
        assert!(!m.accept_ghost(), "mid-line caret must not accept");
        assert_eq!(m.text(), "ls");
        assert!(
            m.ghost().is_some(),
            "a rejected accept leaves the ghost in place"
        );
    }

    #[test]
    fn accept_ghost_is_a_noop_when_a_selection_is_active() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("echo".to_string()));
        m.set_ghost(Some(GhostText {
            suggestion: "echo hi".to_string(),
        }));
        // Caret AT end but a selection is active (Home, then shift-End selects all
        // with the caret back at the end) - the collapsed-selection guard must block.
        m.reduce(InputEvent::Move(Motion::Home, false));
        m.reduce(InputEvent::Move(Motion::End, true));
        assert_eq!(m.caret(), 4, "caret is at end of line");
        assert!(!m.selection().is_empty(), "but a selection is active");
        assert!(!m.accept_ghost(), "an active selection blocks accept");
        assert_eq!(m.text(), "echo");
    }

    #[test]
    fn take_clears_a_pending_ghost() {
        // Regression (review finding): a submitted line must not carry its ghost onto
        // the fresh empty buffer.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ls".to_string()));
        m.set_ghost(Some(GhostText {
            suggestion: "ls -la".to_string(),
        }));
        assert_eq!(m.take(), "ls");
        assert!(m.ghost().is_none(), "take() drops the stale ghost");
        assert_eq!(m.ghost_tail(), None);
    }
}
