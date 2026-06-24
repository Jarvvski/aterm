//! The shared input-history ring (ticket T-3.7). aterm keeps ONE wall-clock-ordered
//! history of everything the user has submitted through the unified input box -
//! both shell commands and agent prompts - each tagged with the [`InputMode`] it was
//! submitted in. Two query *lenses* read that one ring: in `Shell` mode Up-arrow /
//! Ctrl-R see only shell commands, in `Agent` mode only agent prompts, and a user
//! setting can widen either lens to "all" (see [`HistoryScope`]).
//!
//! Why one ring with lenses rather than two rings: it keeps history coherent with the
//! single wall-clock timeline (a prototype "keep") without polluting shell history
//! with agent prose or vice versa - see `docs/research/05-unified-input-ux.md` §4 and
//! Recommendation 8.
//!
//! ## Purity and the shell-history file
//!
//! This module is pure in-memory data: it performs **no I/O** and in particular never
//! writes to the user's real shell history file (`~/.zsh_history`, `~/.bash_history`,
//! fish's `history`). aterm's history is its own; persistence to aterm's data dir is a
//! later concern (ticket T-8.3). Keeping agent prompts out of the shell's own history
//! is therefore true by construction here.
//!
//! ## What lives here vs. the consumer
//!
//! The ring and the [`Recall`] cursor are pure data, consumed by `aterm-ui` /
//! `aterm-app`. The *policy* decisions - when Up-arrow means "recall" vs. in-buffer
//! vertical motion, which key opens Ctrl-R, where the "widen to all" toggle lives -
//! belong to the routing brain (T-3.3) and the input widget (T-3.6); they are out of
//! scope for this module.

use std::collections::VecDeque;
use std::time::SystemTime;

use crate::input::InputMode;

/// Default ring capacity. Generous (zsh's `HISTSIZE` defaults to a few thousand); the
/// oldest entries are evicted once it is full. The ring is one small struct per entry,
/// so this bounds memory without ever being a hot-path concern.
pub const DEFAULT_HISTORY_CAP: usize = 10_000;

/// One stored submission: the verbatim text, the mode it was submitted in, and the
/// wall-clock time of submission. Text is stored exactly as submitted (the ring never
/// trims or rewrites it); only fully blank submissions are dropped at [`HistoryRing::push`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The submitted line, verbatim.
    pub text: String,
    /// Which surface it was submitted to.
    pub mode: InputMode,
    /// Wall-clock submission time, supplied by the caller (the ring keeps no clock of
    /// its own, so it stays pure and deterministically testable). Used for display
    /// ("2 min ago"); ordering is insertion order, not this field (see the type docs).
    pub at: SystemTime,
}

/// Which entries a query sees - the "lens". `Mode(m)` is the per-mode lens (the
/// default: Shell mode sees shell entries, Agent mode sees agent entries); `All` is
/// the widened lens that surfaces both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryScope {
    /// Only entries submitted in this mode.
    Mode(InputMode),
    /// Every entry, regardless of mode (the user "widen to all" setting).
    All,
}

impl HistoryScope {
    /// The lens for the current input mode, widened to [`HistoryScope::All`] when the
    /// user's "widen" setting is on. The setting itself lives in the consumer.
    pub fn for_mode(mode: InputMode, widen: bool) -> Self {
        if widen {
            HistoryScope::All
        } else {
            HistoryScope::Mode(mode)
        }
    }

    /// Whether an entry in `mode` is visible through this lens.
    fn matches(self, mode: InputMode) -> bool {
        match self {
            HistoryScope::All => true,
            HistoryScope::Mode(m) => m == mode,
        }
    }
}

/// The shared, bounded, wall-clock-ordered history ring.
///
/// Entries are kept in submission (insertion) order; the newest is at the back. That
/// insertion order *is* the wall-clock order - submissions happen one after another -
/// so the ring does not sort by [`HistoryEntry::at`], which sidesteps any
/// non-monotonic system-clock surprises. When the ring is full the oldest entry is
/// evicted (FIFO).
#[derive(Debug, Clone)]
pub struct HistoryRing {
    entries: VecDeque<HistoryEntry>,
    cap: usize,
}

impl Default for HistoryRing {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryRing {
    /// A ring with the [`DEFAULT_HISTORY_CAP`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_HISTORY_CAP)
    }

    /// A ring holding at most `cap` entries (clamped to at least 1).
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            entries: VecDeque::with_capacity(cap.min(1024)),
            cap,
        }
    }

    /// Record a submission. Fully blank submissions (empty or whitespace-only) are
    /// ignored, matching shells that do not store a bare Enter. The text is stored
    /// verbatim otherwise. When the ring is full the oldest entry is evicted first.
    pub fn push(&mut self, text: impl Into<String>, mode: InputMode, at: SystemTime) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        if self.entries.len() == self.cap {
            self.entries.pop_front();
        }
        self.entries.push_back(HistoryEntry { text, mode, at });
    }

    /// Number of stored entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ring holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The maximum number of entries this ring retains.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// All entries visible through `scope`, **newest first** (the order Up-arrow recall
    /// and Ctrl-R want). This is the single primitive every other reader is built on.
    pub fn scoped(&self, scope: HistoryScope) -> impl Iterator<Item = &HistoryEntry> {
        self.entries
            .iter()
            .rev()
            .filter(move |e| scope.matches(e.mode))
    }

    /// Ctrl-R reverse-i-search: entries within `scope` whose text contains `query`
    /// (case-insensitive), newest first. An empty `query` returns everything in scope
    /// (newest first), which is what an Ctrl-R prompt shows before the user types.
    pub fn search(&self, scope: HistoryScope, query: &str) -> Vec<&HistoryEntry> {
        let needle = query.to_lowercase();
        self.scoped(scope)
            .filter(|e| e.text.to_lowercase().contains(&needle))
            .collect()
    }

    /// fish-style ghost text: the most-recent entry within `scope` whose text begins
    /// with `prefix` and has a non-empty tail to suggest. Returns `None` for an empty
    /// prefix (no suggestion on a blank line) or when nothing matches. The match is
    /// case-sensitive, matching `zsh-autosuggestions` prefix semantics.
    pub fn suggest(&self, scope: HistoryScope, prefix: &str) -> Option<&HistoryEntry> {
        if prefix.is_empty() {
            return None;
        }
        self.scoped(scope)
            .find(|e| e.text.len() > prefix.len() && e.text.starts_with(prefix))
    }
}

/// A stateful recall cursor for Up/Down history walking through one lens. It holds a
/// position among the scoped (newest-first) entries: `None` is the live draft (the
/// in-progress line, owned by the consumer), `Some(0)` is the most-recent match,
/// `Some(1)` the next older, and so on.
///
/// The consumer owns the draft text and the policy for when Up means "recall" (e.g.
/// only when the caret is on the first line). The cursor indexes the ring's current
/// state; a [`HistoryRing::push`] during an in-progress walk invalidates it, so the
/// consumer should [`Recall::reset`] on submit or on edit.
#[derive(Debug, Clone)]
pub struct Recall {
    scope: HistoryScope,
    /// Position among scoped newest-first entries; `None` is the draft.
    cursor: Option<usize>,
}

impl Recall {
    /// A fresh cursor positioned at the draft (no entry selected) for `scope`.
    pub fn new(scope: HistoryScope) -> Self {
        Self {
            scope,
            cursor: None,
        }
    }

    /// The lens this cursor walks.
    pub fn scope(&self) -> HistoryScope {
        self.scope
    }

    /// Whether the cursor is at the live draft (not inside history).
    pub fn at_draft(&self) -> bool {
        self.cursor.is_none()
    }

    /// The current position among scoped newest-first entries, or `None` at the draft.
    pub fn position(&self) -> Option<usize> {
        self.cursor
    }

    /// Return to the draft.
    pub fn reset(&mut self) {
        self.cursor = None;
    }

    /// Up-arrow: step one entry older within the lens. Returns the newly-selected
    /// entry's text, or `None` (leaving the cursor unmoved) when there is no older
    /// entry - i.e. the ring is empty or the cursor is already at the oldest match.
    pub fn older<'r>(&mut self, ring: &'r HistoryRing) -> Option<&'r str> {
        let next = self.cursor.map_or(0, |n| n + 1);
        let entry = ring.scoped(self.scope).nth(next)?;
        self.cursor = Some(next);
        Some(entry.text.as_str())
    }

    /// Down-arrow: step one entry newer within the lens. Returns the newly-selected
    /// entry's text; returns `None` when the step lands back on the draft (the cursor
    /// becomes [`Recall::at_draft`]) or when already at the draft. A `None` return is
    /// the consumer's cue to restore the saved draft line.
    pub fn newer<'r>(&mut self, ring: &'r HistoryRing) -> Option<&'r str> {
        match self.cursor {
            None => None,
            Some(0) => {
                self.cursor = None;
                None
            }
            Some(n) => {
                let prev = n - 1;
                let text = ring.scoped(self.scope).nth(prev).map(|e| e.text.as_str());
                self.cursor = Some(prev);
                text
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A deterministic wall-clock stamp `secs` after the epoch - tests supply their own
    /// timestamps so the ring stays clock-free and reproducible.
    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn shell(text: &str, secs: u64) -> (String, InputMode, SystemTime) {
        (text.to_string(), InputMode::Shell, t(secs))
    }

    fn agent(text: &str, secs: u64) -> (String, InputMode, SystemTime) {
        (text.to_string(), InputMode::Agent, t(secs))
    }

    fn push(ring: &mut HistoryRing, e: (String, InputMode, SystemTime)) {
        ring.push(e.0, e.1, e.2);
    }

    /// AC1: submitting a shell command and an agent prompt stores both with the correct
    /// mode tags and timestamps.
    #[test]
    fn push_stores_both_with_mode_and_timestamp() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("git status", 10));
        push(&mut ring, agent("how do I rebase?", 20));

        assert_eq!(ring.len(), 2);
        // Oldest-first via reversing the newest-first scoped(All) iterator.
        let all: Vec<&HistoryEntry> = ring.scoped(HistoryScope::All).collect();
        // newest first
        assert_eq!(all[0].text, "how do I rebase?");
        assert_eq!(all[0].mode, InputMode::Agent);
        assert_eq!(all[0].at, t(20));
        assert_eq!(all[1].text, "git status");
        assert_eq!(all[1].mode, InputMode::Shell);
        assert_eq!(all[1].at, t(10));
    }

    /// Ordering follows insertion, NOT the `at` field (see the type docs): a later push
    /// carrying an *earlier* wall-clock stamp still sorts newest. Pins the documented
    /// clock-skew invariant - a sort-by-`at` regression would reorder these and fail.
    #[test]
    fn ordering_is_insertion_order_not_the_timestamp() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("typed first", 500));
        push(&mut ring, shell("typed second", 100)); // clock went backwards

        let order: Vec<&str> = ring
            .scoped(HistoryScope::All)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(order, vec!["typed second", "typed first"]);
    }

    /// AC2 (data): the per-mode lens surfaces only that mode's entries, newest first.
    #[test]
    fn scoped_lens_filters_by_mode_newest_first() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("ls", 1));
        push(&mut ring, agent("explain ls", 2));
        push(&mut ring, shell("cd /tmp", 3));
        push(&mut ring, agent("what is /tmp", 4));

        let shell_lens: Vec<&str> = ring
            .scoped(HistoryScope::Mode(InputMode::Shell))
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(shell_lens, vec!["cd /tmp", "ls"]);

        let agent_lens: Vec<&str> = ring
            .scoped(HistoryScope::Mode(InputMode::Agent))
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(agent_lens, vec!["what is /tmp", "explain ls"]);
    }

    /// AC2 (behavior): Up-arrow recall cycles within the lens (shell entries only),
    /// clamps at the oldest, and Down walks back through to the draft.
    #[test]
    fn recall_cycles_within_lens_and_returns_to_draft() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("first", 1));
        push(&mut ring, agent("noise", 2)); // must be skipped by the shell lens
        push(&mut ring, shell("second", 3));
        push(&mut ring, shell("third", 4));

        let mut r = Recall::new(HistoryScope::Mode(InputMode::Shell));
        assert!(r.at_draft());

        // Up walks newest -> oldest, agent entry never appears.
        assert_eq!(r.older(&ring), Some("third"));
        assert_eq!(r.older(&ring), Some("second"));
        assert_eq!(r.older(&ring), Some("first"));
        // At the oldest: Up is a no-op (None) and the cursor does not move.
        assert_eq!(r.older(&ring), None);
        assert_eq!(r.position(), Some(2));

        // Down walks back toward newest, then off the top into the draft.
        assert_eq!(r.newer(&ring), Some("second"));
        assert_eq!(r.newer(&ring), Some("third"));
        assert_eq!(r.newer(&ring), None); // stepped past newest -> draft
        assert!(r.at_draft());
        // Down at the draft stays at the draft.
        assert_eq!(r.newer(&ring), None);
        assert!(r.at_draft());
    }

    /// Recall on an empty ring yields nothing and never panics.
    #[test]
    fn recall_on_empty_ring_is_none() {
        let ring = HistoryRing::new();
        let mut r = Recall::new(HistoryScope::Mode(InputMode::Shell));
        assert_eq!(r.older(&ring), None);
        assert!(r.at_draft());
        assert_eq!(r.newer(&ring), None);
    }

    /// AC3: Ctrl-R substring search respects the lens and is case-insensitive,
    /// returning matches newest first.
    #[test]
    fn search_substring_respects_lens() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("git commit", 1));
        push(&mut ring, agent("git is hard", 2)); // agent entry containing "git"
        push(&mut ring, shell("git push", 3));
        push(&mut ring, shell("ls -la", 4));

        let hits: Vec<&str> = ring
            .search(HistoryScope::Mode(InputMode::Shell), "GIT")
            .into_iter()
            .map(|e| e.text.as_str())
            .collect();
        // newest-first, shell-only, case-insensitive: no "git is hard".
        assert_eq!(hits, vec!["git push", "git commit"]);

        // Empty query returns everything in scope, newest first.
        let all_shell: Vec<&str> = ring
            .search(HistoryScope::Mode(InputMode::Shell), "")
            .into_iter()
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(all_shell, vec!["ls -la", "git push", "git commit"]);
    }

    /// AC4: the widened "all" lens surfaces both modes in either query path.
    #[test]
    fn widen_to_all_surfaces_both_modes() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("git status", 1));
        push(&mut ring, agent("git status meaning", 2));

        assert_eq!(
            HistoryScope::for_mode(InputMode::Shell, true),
            HistoryScope::All
        );
        assert_eq!(
            HistoryScope::for_mode(InputMode::Agent, false),
            HistoryScope::Mode(InputMode::Agent)
        );

        let hits: Vec<&str> = ring
            .search(HistoryScope::All, "git status")
            .into_iter()
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(hits, vec!["git status meaning", "git status"]);

        // And the same widening works for recall.
        let mut r = Recall::new(HistoryScope::All);
        assert_eq!(r.older(&ring), Some("git status meaning"));
        assert_eq!(r.older(&ring), Some("git status"));
    }

    /// AC (T-3.5 support): ghost text is the most-recent prefix match with a tail.
    #[test]
    fn suggest_returns_most_recent_prefix_match() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("git commit -m old", 1));
        push(&mut ring, shell("git commit -m new", 2));
        push(&mut ring, agent("git pull please", 3)); // wrong lens

        let s = ring
            .suggest(HistoryScope::Mode(InputMode::Shell), "git c")
            .map(|e| e.text.as_str());
        assert_eq!(s, Some("git commit -m new"));

        // Empty prefix never suggests; an exact full match has no tail to offer.
        assert!(ring
            .suggest(HistoryScope::Mode(InputMode::Shell), "")
            .is_none());
        assert!(ring
            .suggest(HistoryScope::Mode(InputMode::Shell), "git commit -m new")
            .is_none());
        // No shell entry starts with "cargo".
        assert!(ring
            .suggest(HistoryScope::Mode(InputMode::Shell), "cargo")
            .is_none());
    }

    /// AC5: agent prompts stay out of the user's shell history. The *file* half is true
    /// by construction - this module imports no `std::fs`, takes no path, and writes
    /// nowhere, so the real `~/.zsh_history` cannot be touched (and the end-to-end
    /// guarantee that an agent prompt never reaches the shell at all is the routing
    /// brain's, T-3.3: agent-mode submit bypasses the PTY). What this layer owns and
    /// CAN be asserted red-capably is partitioning: agent prompts live only in aterm's
    /// own ring, tagged `Agent`, and never surface through the Shell lens that drives
    /// shell recall / search / ghost text. A regression in [`HistoryScope::matches`]
    /// that leaked agent entries into the shell view fails this test.
    #[test]
    fn agent_prompts_are_absent_from_the_shell_lens() {
        let mut ring = HistoryRing::new();
        push(&mut ring, agent("delete all my files", 1));
        push(&mut ring, agent("rm -rf / please", 2));
        push(&mut ring, shell("echo hi", 3));

        let shell_lens: Vec<&str> = ring
            .scoped(HistoryScope::Mode(InputMode::Shell))
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(shell_lens, vec!["echo hi"], "no agent prompt may leak in");
        assert!(ring
            .search(HistoryScope::Mode(InputMode::Shell), "rm -rf")
            .is_empty());
        assert!(ring
            .suggest(HistoryScope::Mode(InputMode::Shell), "delete")
            .is_none());
    }

    /// Blank submissions (empty / whitespace-only) are dropped; meaningful leading or
    /// trailing whitespace inside a real command is preserved verbatim.
    #[test]
    fn blank_submissions_are_dropped_real_whitespace_preserved() {
        let mut ring = HistoryRing::new();
        push(&mut ring, shell("", 1));
        push(&mut ring, shell("   ", 2));
        push(&mut ring, shell("\t\n", 3));
        assert!(ring.is_empty());

        ring.push("  spaced cmd  ", InputMode::Shell, t(4));
        assert_eq!(ring.len(), 1);
        assert_eq!(
            ring.scoped(HistoryScope::All).next().unwrap().text,
            "  spaced cmd  "
        );
    }

    /// A full ring evicts the oldest entry first (FIFO) and preserves order.
    #[test]
    fn capacity_evicts_oldest_first() {
        let mut ring = HistoryRing::with_capacity(3);
        assert_eq!(ring.capacity(), 3);
        push(&mut ring, shell("one", 1));
        push(&mut ring, shell("two", 2));
        push(&mut ring, shell("three", 3));
        push(&mut ring, shell("four", 4)); // evicts "one"

        assert_eq!(ring.len(), 3);
        let texts: Vec<&str> = ring
            .scoped(HistoryScope::All)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(texts, vec!["four", "three", "two"]);
    }

    /// Eviction is mode-blind global FIFO: the single oldest entry goes regardless of
    /// mode. Pins the documented behavior against a regression that made eviction
    /// mode-aware (e.g. evicting the oldest *of the pushed mode*).
    #[test]
    fn eviction_is_mode_blind_fifo() {
        let mut ring = HistoryRing::with_capacity(3);
        push(&mut ring, agent("oldest agent", 1));
        push(&mut ring, shell("a shell", 2));
        push(&mut ring, agent("a second agent", 3));
        push(&mut ring, shell("newest shell", 4)); // evicts the oldest entry: the agent one

        let texts: Vec<&str> = ring
            .scoped(HistoryScope::All)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(texts, vec!["newest shell", "a second agent", "a shell"]);
        // The evicted agent prompt is gone from its own lens too.
        let agents: Vec<&str> = ring
            .scoped(HistoryScope::Mode(InputMode::Agent))
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(agents, vec!["a second agent"]);
    }

    /// `with_capacity(0)` is clamped to 1 rather than producing a ring that can never
    /// hold anything.
    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let mut ring = HistoryRing::with_capacity(0);
        assert_eq!(ring.capacity(), 1);
        push(&mut ring, shell("only", 1));
        push(&mut ring, shell("latest", 2));
        assert_eq!(ring.len(), 1);
        assert_eq!(
            ring.scoped(HistoryScope::All).next().unwrap().text,
            "latest"
        );
    }
}
