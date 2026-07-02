//! The tab-completion model (ticket T-9.5): a pure, allocation-transparent fuzzy finder
//! that hugs the prompt. It mirrors the vision mock's completion behavior
//! (`docs/design/vision-mock/AtermWindow.dc.html` `fuzzyParts` / `computeCompletions`): a
//! case-insensitive SUBSEQUENCE match with a rank score, matched letters flagged per
//! character so the renderer can highlight them in the accent color, plus the open/index
//! navigation state (Tab / up / down / Enter / Esc).
//!
//! This is the VISUAL + interaction half only. The candidate SOURCES (shell history, `$PATH`
//! binaries, Fig-spec argument specs) are ticket T-8.5; the host feeds candidates in and this
//! ranks + navigates them. It is pure (no GPU, no window, no clock), so it is unit-tested on
//! every platform, like [`crate::input`].

/// A fuzzy SUBSEQUENCE match of a query against one candidate: a per-character hit flag
/// (one entry per `char` of the candidate, in order) plus a rank `score` where LOWER is
/// better. Mirrors the mock's `fuzzyParts` return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// One flag per candidate `char`: `true` where a query char matched. The renderer
    /// draws a `true` char in the accent color, a `false` char in `fg.secondary`.
    pub hits: Vec<bool>,
    /// Rank score, LOWER is better: `first_hit + (span - query_len)`, where `span` is the
    /// index distance from the first to the last matched char (inclusive). So an earlier,
    /// tighter match ranks above a later, looser one. `0` for an empty query.
    pub score: i64,
}

/// Fuzzy-match `query` against `candidate`, case-insensitively, as an ordered subsequence:
/// every char of `query` must appear in `candidate` in order (not necessarily contiguously).
/// Returns `None` when it does not (no match). An empty query matches every candidate with
/// no hits and score `0` (so Tab on an empty line offers everything, as the mock does).
///
/// Pure. Allocates a few short-lived `Vec`s sized to the candidate/query (the candidate's
/// chars, the per-char `hits`, and the lowercased query); a no-match still allocates the
/// candidate chars + hits before returning `None`. Off the render hot path (called only on
/// Tab-open / text-change-while-open, never per frame). The comparison is
/// ASCII-case-insensitive on both sides (the query is expected pre-trimmed by the caller).
#[must_use]
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    let cand: Vec<char> = candidate.chars().collect();
    if query.is_empty() {
        return Some(FuzzyMatch {
            hits: vec![false; cand.len()],
            score: 0,
        });
    }
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut hits = vec![false; cand.len()];
    let mut qi = 0usize;
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    for (i, &c) in cand.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if c.to_ascii_lowercase() == q[qi] {
            hits[i] = true;
            first.get_or_insert(i);
            last = i;
            qi += 1;
        }
    }
    if qi < q.len() {
        return None; // not every query char was consumed in order -> no match
    }
    let first = first.unwrap_or(0);
    let span = (last - first + 1) as i64;
    let score = first as i64 + (span - q.len() as i64);
    Some(FuzzyMatch { hits, score })
}

/// One ranked completion candidate: the full replacement `text`, a short `desc` shown faint
/// beside it, the per-char `hits` (for accent highlighting), and its rank `score`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub text: String,
    pub desc: String,
    pub hits: Vec<bool>,
    pub score: i64,
}

/// Rank `candidates` (each a `(text, desc)` pair) against `query`, dropping non-matches,
/// sorting best-first (lowest score, ties keeping input order via a stable sort), and taking
/// at most `limit`. Mirrors the mock's `computeCompletions`. The caller supplies the
/// candidate set (history / path / spec - T-8.5); this is the pure ranking.
#[must_use]
pub fn rank(query: &str, candidates: &[(&str, &str)], limit: usize) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = candidates
        .iter()
        .filter_map(|(text, desc)| {
            fuzzy_match(query, text).map(|m| CompletionItem {
                text: (*text).to_string(),
                desc: (*desc).to_string(),
                hits: m.hits,
                score: m.score,
            })
        })
        .collect();
    out.sort_by_key(|c| c.score); // stable: equal scores keep candidate order
    out.truncate(limit);
    out
}

/// The default maximum number of completion rows shown at once (the mock's `slice(0, 6)`).
pub const DEFAULT_COMPLETION_LIMIT: usize = 6;

/// The tab-completion popover STATE: whether it is open, the ranked items, and the active
/// row index. Pure navigation (Tab / up / down / Enter / Esc map onto these); the host owns
/// *when* to open (Tab) and *what* to do on accept (fill the input from [`Self::active`]).
#[derive(Debug, Clone, Default)]
pub struct Completion {
    open: bool,
    items: Vec<CompletionItem>,
    index: usize,
}

impl Completion {
    /// A fresh, closed completion with no items.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the popover is currently open (and therefore capturing Tab/up/down/Enter/Esc).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The ranked items currently shown (empty when closed).
    #[must_use]
    pub fn items(&self) -> &[CompletionItem] {
        &self.items
    }

    /// The active row index (clamped into `items`), or `0` when empty.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// The active (highlighted) item, or `None` when there are none.
    #[must_use]
    pub fn active(&self) -> Option<&CompletionItem> {
        self.items.get(self.index)
    }

    /// Open with a freshly-ranked item set, resetting the active row to the top. A no-op
    /// (stays closed) when `items` is empty - Tab with nothing to offer opens nothing.
    pub fn open_with(&mut self, items: Vec<CompletionItem>) {
        self.items = items;
        self.index = 0;
        self.open = !self.items.is_empty();
    }

    /// Refresh the items while open (the query changed as the user typed): swap the set,
    /// clamp the active row, and CLOSE if nothing matches any more. A no-op when closed.
    pub fn refresh(&mut self, items: Vec<CompletionItem>) {
        if !self.open {
            return;
        }
        self.items = items;
        if self.items.is_empty() {
            self.close();
        } else {
            self.index = self.index.min(self.items.len() - 1);
        }
    }

    /// Close the popover and drop its items.
    pub fn close(&mut self) {
        self.open = false;
        self.items.clear();
        self.index = 0;
    }

    /// Move the active row up one (toward the top), saturating at the first row.
    pub fn move_up(&mut self) {
        self.index = self.index.saturating_sub(1);
    }

    /// Move the active row down one (toward the bottom), clamping at the last row.
    pub fn move_down(&mut self) {
        if !self.items.is_empty() {
            self.index = (self.index + 1).min(self.items.len() - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything_with_no_hits() {
        let m = fuzzy_match("", "git status").expect("empty query matches");
        assert_eq!(m.score, 0);
        assert_eq!(m.hits.len(), "git status".chars().count());
        assert!(m.hits.iter().all(|h| !h), "no chars are highlighted");
    }

    #[test]
    fn subsequence_match_flags_the_matched_chars_case_insensitively() {
        // "gs" fuzzy-matches "git status": g(0) and s(4, the start of "status").
        let m = fuzzy_match("gs", "git status").expect("gs matches git status");
        let s: Vec<char> = "git status".chars().collect();
        for (i, &h) in m.hits.iter().enumerate() {
            let expect = i == 0 || s[i] == 's' && i == 4;
            assert_eq!(h, expect, "char {i} ({:?}) hit flag", s[i]);
        }
        // Case-insensitive on both sides.
        assert!(fuzzy_match("GS", "git status").is_some());
        assert!(fuzzy_match("gs", "GIT STATUS").is_some());
    }

    #[test]
    fn non_subsequence_does_not_match() {
        // 'z' is not present; and order matters: "sg" cannot match "git status" (s after g).
        assert!(fuzzy_match("z", "git status").is_none());
        assert!(
            fuzzy_match("tg", "git").is_none(),
            "order matters: t before g fails on 'git'"
        );
    }

    #[test]
    fn score_prefers_earlier_and_tighter_matches() {
        // "gi" is a tight prefix of "git" (first=0, span=2, len=2 -> score 0).
        let tight = fuzzy_match("gi", "git commit").unwrap();
        assert_eq!(tight.score, 0);
        // "gc" in "git commit": g(0), c(4) -> first=0, span=5, len=2 -> score 3. Looser.
        let loose = fuzzy_match("gc", "git commit").unwrap();
        assert!(loose.score > tight.score, "a looser spread scores worse");
    }

    #[test]
    fn rank_sorts_best_first_and_truncates() {
        let cands = [
            ("git status", "working tree"),
            ("cargo test", "test"),
            ("git commit -m", "record"),
            ("grep -r", "search"),
        ];
        // Query "gi": matches the two git commands (tight) and "grep" (g..i? grep has no 'i'
        // -> no match); "cargo" has no subsequence "gi". So two matches, git first.
        let ranked = rank("gi", &cands, 6);
        assert_eq!(ranked.len(), 2, "only the two git commands match 'gi'");
        assert_eq!(ranked[0].text, "git status");
        assert!(ranked[0].score <= ranked[1].score, "sorted best-first");
        // The limit truncates.
        let all = rank("", &cands, 2);
        assert_eq!(
            all.len(),
            2,
            "empty query matches all, truncated to the limit"
        );
    }

    fn items(texts: &[&str]) -> Vec<CompletionItem> {
        texts
            .iter()
            .map(|t| CompletionItem {
                text: (*t).to_string(),
                desc: String::new(),
                hits: vec![false; t.chars().count()],
                score: 0,
            })
            .collect()
    }

    #[test]
    fn open_with_empty_stays_closed() {
        let mut c = Completion::new();
        c.open_with(vec![]);
        assert!(!c.is_open(), "nothing to offer -> opens nothing");
        assert!(c.active().is_none());
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let mut c = Completion::new();
        c.open_with(items(&["a", "b", "c"]));
        assert!(c.is_open());
        assert_eq!(c.index(), 0);
        c.move_up(); // saturates at the top
        assert_eq!(c.index(), 0);
        c.move_down();
        c.move_down();
        assert_eq!(c.index(), 2);
        c.move_down(); // clamps at the bottom
        assert_eq!(c.index(), 2);
        assert_eq!(c.active().unwrap().text, "c");
    }

    #[test]
    fn refresh_clamps_the_index_and_closes_when_empty() {
        let mut c = Completion::new();
        c.open_with(items(&["a", "b", "c"]));
        c.move_down();
        c.move_down(); // index 2
        c.refresh(items(&["x"])); // fewer items -> index clamps to 0
        assert!(c.is_open());
        assert_eq!(c.index(), 0);
        c.refresh(vec![]); // no matches -> close
        assert!(!c.is_open());
        assert!(c.active().is_none());
    }

    #[test]
    fn refresh_is_a_noop_when_closed() {
        let mut c = Completion::new();
        c.refresh(items(&["a"]));
        assert!(!c.is_open(), "refresh never opens a closed popover");
    }

    #[test]
    fn close_resets_state() {
        let mut c = Completion::new();
        c.open_with(items(&["a", "b"]));
        c.move_down();
        c.close();
        assert!(!c.is_open());
        assert_eq!(c.index(), 0);
        assert!(c.items().is_empty());
    }
}
