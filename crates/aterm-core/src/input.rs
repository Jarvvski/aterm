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

use crate::editing::{EditBuffer, EditEvent};
pub use crate::editing::{Motion, Preedit, Selection};

/// Which surface the input is currently driving. The hotkey flips ONLY this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// The finished line is committed to the shell PTY.
    #[default]
    Shell,
    /// The line is composed as an agent prompt.
    Agent,
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

/// The pure unified-input reducer: text + selection + mode, plus undo history and a
/// vertical goal column. See the module docs for the caller-owns-submit contract.
#[derive(Debug, Clone, Default)]
pub struct InputModel {
    /// Shared inert text editing behavior. Routing-specific state remains in this module.
    editor: EditBuffer,
    /// Where Enter routes; flipped by the hotkey, text untouched.
    mode: InputMode,
    /// Async syntax-highlight overlay (T-3.5), applied by the off-thread worker via
    /// [`Self::set_highlight`]; the render path only reads it. Empty until computed.
    overlay: Highlight,
    /// Async ghost-text suggestion (T-3.5), applied via [`Self::set_ghost`]; the visible
    /// tail is derived live by [`Self::ghost_tail`].
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
        self.editor.text()
    }

    /// The current selection (caret + anchor).
    pub fn selection(&self) -> Selection {
        self.editor.selection()
    }

    /// The caret position as a char offset (the moving end of the selection).
    pub fn caret(&self) -> usize {
        self.editor.caret()
    }

    /// The current routing target.
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// True when the buffer holds no text.
    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    /// The active IME composition, if any (reserved for T-3.2).
    pub fn preedit(&self) -> Option<&Preedit> {
        self.editor.preedit()
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
    /// prefix, or `None`. This is the SINGLE source of truth for whether a suggestion is
    /// live - both what the widget renders after the caret and what [`Self::accept_ghost`]
    /// accepts - so display and acceptance can never disagree. It returns `None` unless,
    /// matching zsh-autosuggestions:
    /// - there is a ghost and the line is non-empty;
    /// - the selection is collapsed (a region is being selected -> no suggestion);
    /// - the caret is at the END of the text (a mid-line caret hides it - the suggestion
    ///   only extends the tail, so showing it away from the end would be unacceptable);
    /// - the suggestion still strict-prefixes the current text (a debounced worker can
    ///   lag; a diverged buffer neither shows nor accepts a stale tail).
    pub fn ghost_tail(&self) -> Option<&str> {
        let text = self.editor.text();
        let selection = self.editor.selection();
        if text.is_empty() || !selection.is_empty() || selection.caret != text.chars().count() {
            return None;
        }
        let tail = self.ghost.as_ref()?.suggestion.strip_prefix(text)?;
        if tail.is_empty() {
            None
        } else {
            Some(tail)
        }
    }

    /// Accept the ghost-text suggestion (zsh-autosuggestions semantics: `Right`/`End`
    /// at end of line). Inserts the live tail as one undo unit and clears the ghost;
    /// returns whether anything was accepted. Delegates the "is a suggestion live?"
    /// decision entirely to [`Self::ghost_tail`] (end-of-line + collapsed selection +
    /// strict-prefix), so acceptance can never fire where the tail is not shown. The
    /// widget (T-3.6) binds the keys; this is the pure operation they invoke.
    pub fn accept_ghost(&mut self) -> bool {
        // `ghost_tail` already enforces collapsed-selection + caret-at-end + live-prefix,
        // so it is the sole gate: no separate caret check can drift out of sync with it.
        let tail = match self.ghost_tail() {
            Some(t) => t.to_string(),
            None => return false,
        };
        self.ghost = None;
        self.editor.reduce(EditEvent::Insert(tail));
        true
    }

    // --- T-3.2 IME mutators (driven by the winit `Ime` event feed) ----------

    /// Set (or clear) the active IME composition (ticket T-3.2). The preedit is a
    /// **transient overlay**: it does NOT touch the committed text, selection, or undo
    /// history (the composition is not part of the buffer until it commits). The aterm-ui
    /// IME feed maps a winit `Ime::Preedit(text, cursor)` to `Some(Preedit { text, cursor })`
    /// here; `None` clears it (winit `Ime::Disabled`, a focus loss, or an empty
    /// `Ime::Preedit` - winit sends an empty preedit to signal "composition cleared").
    /// The routing brain (T-3.3) reads [`Self::preedit`] first, so while this is `Some`
    /// the IME owns Enter/Tab/Esc and nothing submits or routes.
    pub fn set_preedit(&mut self, preedit: Option<Preedit>) {
        self.editor.reduce(EditEvent::SetPreedit(preedit));
    }

    /// Commit an IME composition (ticket T-3.2): clear any active preedit, then insert
    /// `text` as inert literal characters through the ordinary [`Self::insert`] path -
    /// so it is ONE undo unit, replaces an active selection, and is never interpreted
    /// (T-3.1 semantics). Called on winit `Ime::Commit`. An empty `text` only clears the
    /// preedit (no insert, no undo entry), matching the empty-preedit winit sends just
    /// before a commit.
    pub fn commit_ime(&mut self, text: &str) {
        self.editor.reduce(EditEvent::CommitIme(text.to_string()));
    }

    // --- the reducer --------------------------------------------------------

    /// Apply an event to the model. Pure: no I/O, no interpretation of the text.
    pub fn reduce(&mut self, event: InputEvent) {
        match event {
            InputEvent::Insert(text) => {
                self.editor.reduce(EditEvent::Insert(text));
            }
            InputEvent::Backspace => {
                self.editor.reduce(EditEvent::Backspace);
            }
            InputEvent::Delete => {
                self.editor.reduce(EditEvent::Delete);
            }
            InputEvent::Move(motion, extend) => {
                self.editor.reduce(EditEvent::Move(motion, extend));
            }
            InputEvent::Undo => {
                self.editor.reduce(EditEvent::Undo);
            }
            InputEvent::Redo => {
                self.editor.reduce(EditEvent::Redo);
            }
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
        let line = self.editor.take();
        // The suggestion + highlight were for the now-submitted line; drop them so they
        // cannot carry onto the fresh empty buffer (the worker recomputes for the new
        // line). Without clearing `overlay` a stale span set would linger on the next
        // line's first characters until the debounced worker catches up.
        self.ghost = None;
        self.overlay = Highlight::default();
        line
    }

    /// Reset the model to empty, discarding the text (see [`take`](Self::take)).
    pub fn reset(&mut self) {
        let _ = self.take();
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
    fn ghost_tail_hides_when_the_caret_is_not_at_end_or_a_selection_is_active() {
        // Review finding: the ghost is only acceptable at end-of-line, so it must not be
        // SHOWN elsewhere (display and acceptance share `ghost_tail`). Moving the caret
        // off the end (or opening a selection) hides it, even though the text is unchanged
        // and the suggestion is still a prefix.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("git st".to_string()));
        m.set_ghost(Some(GhostText {
            suggestion: "git status".to_string(),
        }));
        assert_eq!(m.ghost_tail(), Some("atus"), "shown at end of line");
        // Caret moved off the end -> hidden (and unacceptable).
        m.reduce(InputEvent::Move(Motion::Left, false));
        assert_eq!(m.ghost_tail(), None, "hidden when the caret is mid-line");
        assert!(!m.accept_ghost(), "and cannot be accepted mid-line");
        assert!(
            m.ghost().is_some(),
            "the ghost itself is retained for later"
        );
        // Back to the end -> shown again.
        m.reduce(InputEvent::Move(Motion::End, false));
        assert_eq!(m.ghost_tail(), Some("atus"), "shown again at end of line");
        // A selection (even with the caret at the end) hides it too.
        m.reduce(InputEvent::Move(Motion::Left, true));
        assert_eq!(m.ghost_tail(), None, "hidden while a selection is active");
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

    #[test]
    fn take_clears_the_highlight_overlay() {
        // T-3.5: a submitted line's spans must not linger over the fresh empty buffer.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ls -la".to_string()));
        m.set_highlight(Highlight {
            spans: vec![StyleSpan {
                start: 0,
                end: 2,
                kind: SpanKind::Command,
            }],
        });
        assert_eq!(m.take(), "ls -la");
        assert_eq!(
            m.highlight(),
            &Highlight::default(),
            "take() drops the stale highlight overlay"
        );
    }

    // --- T-3.2 IME mutators -------------------------------------------------

    #[test]
    fn set_preedit_is_a_transient_overlay_that_never_touches_the_buffer() {
        // A preedit is the in-progress composition, NOT committed text: setting or
        // clearing it must leave text/selection/undo untouched.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ko".to_string()));
        let text_before = m.text().to_string();
        let sel_before = m.selection();
        m.set_preedit(Some(Preedit {
            text: "ni".to_string(),
            cursor: Some((2, 2)),
        }));
        assert_eq!(
            m.preedit(),
            Some(&Preedit {
                text: "ni".to_string(),
                cursor: Some((2, 2)),
            })
        );
        assert_eq!(m.text(), text_before, "preedit does not change the buffer");
        assert_eq!(m.selection(), sel_before, "preedit does not move the caret");
        // Undo is untouched: undoing pops the "ko" insert, not the preedit.
        m.set_preedit(None);
        assert!(m.preedit().is_none());
        m.reduce(InputEvent::Undo);
        assert_eq!(
            m.text(),
            "",
            "undo history is intact across preedit set/clear"
        );
    }

    #[test]
    fn commit_ime_inserts_inert_text_clears_preedit_and_is_one_undo_unit() {
        // AC (T-3.2): Commit inserts the final text as inert characters (T-3.1 Insert
        // semantics) and clears the preedit.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ko".to_string()));
        m.set_preedit(Some(Preedit {
            text: "ni".to_string(),
            cursor: None,
        }));
        m.commit_ime("に");
        assert_eq!(m.text(), "koに", "committed text is appended at the caret");
        assert!(m.preedit().is_none(), "commit clears the preedit");
        // Inert + one undo unit: undoing the commit removes exactly the committed run.
        m.reduce(InputEvent::Undo);
        assert_eq!(m.text(), "ko");
    }

    #[test]
    fn commit_ime_replaces_an_active_selection() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("abc".to_string()));
        m.reduce(InputEvent::Move(Motion::Home, false));
        m.reduce(InputEvent::Move(Motion::Right, true));
        m.reduce(InputEvent::Move(Motion::Right, true)); // select [0,2) = "ab"
        m.commit_ime("X");
        assert_eq!(m.text(), "Xc", "commit replaces the selection as one edit");
        m.reduce(InputEvent::Undo);
        assert_eq!(m.text(), "abc");
    }

    #[test]
    fn commit_ime_with_empty_text_only_clears_the_preedit() {
        // winit sends an empty Preedit right before a Commit; an empty Commit must not
        // push an undo entry or change the buffer - just drop the composition.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ls".to_string()));
        m.set_preedit(Some(Preedit {
            text: "x".to_string(),
            cursor: None,
        }));
        m.commit_ime("");
        assert_eq!(m.text(), "ls");
        assert!(m.preedit().is_none());
        // No spurious undo unit was pushed by the empty commit.
        m.reduce(InputEvent::Undo);
        assert_eq!(
            m.text(),
            "",
            "undo pops the original insert, not an empty commit"
        );
    }

    #[test]
    fn commit_ime_embedded_control_chars_are_literal_and_inert() {
        // Defensive: a commit string is stored verbatim, never interpreted.
        let mut m = InputModel::new();
        m.commit_ime("a\nb");
        assert_eq!(m.text(), "a\nb");
        assert!(m.text().contains('\n'));
    }

    #[test]
    fn take_clears_a_dangling_preedit() {
        // A submit must not carry a half-composed preedit onto the fresh buffer.
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ls".to_string()));
        m.set_preedit(Some(Preedit {
            text: "x".to_string(),
            cursor: None,
        }));
        assert_eq!(m.take(), "ls");
        assert!(m.preedit().is_none(), "take() drops the dangling preedit");
    }
}
