/// Maximum undo depth shared by the unified input and document editor.
const MAX_UNDO: usize = 1024;

/// A caret with an optional selection anchor, expressed as character offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    /// The fixed end of the selection.
    pub anchor: usize,
    /// The moving end of the selection.
    pub caret: usize,
}

impl Selection {
    #[must_use]
    pub fn at(pos: usize) -> Self {
        Self {
            anchor: pos,
            caret: pos,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.caret
    }

    #[must_use]
    pub fn start(&self) -> usize {
        self.anchor.min(self.caret)
    }

    #[must_use]
    pub fn end(&self) -> usize {
        self.anchor.max(self.caret)
    }
}

/// A cursor motion shared by every aterm-owned editing surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    BufferStart,
    BufferEnd,
    Up,
    Down,
}

/// Active IME composition. It is a transient overlay until committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// Byte-indexed candidate cursor range, matching winit.
    pub cursor: Option<(usize, usize)>,
}

/// A text edit shared by every aterm-owned editing surface. Routing and submission stay
/// with the caller, so this interface contains only inert buffer transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditEvent {
    Insert(String),
    Backspace,
    Delete,
    Move(Motion, bool),
    Undo,
    Redo,
    SetPreedit(Option<Preedit>),
    CommitIme(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    selection: Selection,
}

/// The shared in-process editing implementation. This stays crate-private: callers use
/// the deeper `InputModel` or `Document` module interfaces rather than reaching past them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditBuffer {
    text: String,
    selection: Selection,
    goal_col: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    preedit: Option<Preedit>,
    version: u64,
}

impl Default for EditBuffer {
    fn default() -> Self {
        Self::from_text(String::new())
    }
}

impl EditBuffer {
    pub(crate) fn from_text(text: String) -> Self {
        Self {
            text,
            selection: Selection::default(),
            goal_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            preedit: None,
            version: 0,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn selection(&self) -> Selection {
        self.selection
    }

    pub(crate) fn caret(&self) -> usize {
        self.selection.caret
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(crate) fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }

    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    /// Apply one edit and report whether the committed text changed.
    pub(crate) fn reduce(&mut self, event: EditEvent) -> bool {
        match event {
            EditEvent::Insert(text) => self.insert(&text),
            EditEvent::Backspace => self.backspace(),
            EditEvent::Delete => self.delete_forward(),
            EditEvent::Move(motion, extend) => {
                self.move_caret(motion, extend);
                false
            }
            EditEvent::Undo => self.undo(),
            EditEvent::Redo => self.redo(),
            EditEvent::SetPreedit(preedit) => {
                self.set_preedit(preedit);
                false
            }
            EditEvent::CommitIme(text) => self.commit_ime(&text),
        }
    }

    pub(crate) fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        let changed = !text.is_empty()
            || !self.selection.is_empty()
            || !self.undo.is_empty()
            || !self.redo.is_empty()
            || self.preedit.is_some();
        self.selection = Selection::default();
        self.goal_col = None;
        self.undo.clear();
        self.redo.clear();
        self.preedit = None;
        if changed {
            self.bump();
        }
        text
    }

    fn insert(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        self.push_undo();
        self.delete_selection();
        let at = self.byte_of(self.selection.caret);
        self.text.insert_str(at, text);
        self.selection = Selection::at(self.selection.caret + text.chars().count());
        self.goal_col = None;
        self.bump();
        true
    }

    fn backspace(&mut self) -> bool {
        if !self.selection.is_empty() {
            self.push_undo();
            self.delete_selection();
            self.goal_col = None;
            self.bump();
            return true;
        }
        if self.selection.caret == 0 {
            return false;
        }
        self.push_undo();
        let caret = self.selection.caret;
        let (start, end) = (self.byte_of(caret - 1), self.byte_of(caret));
        self.text.replace_range(start..end, "");
        self.selection = Selection::at(caret - 1);
        self.goal_col = None;
        self.bump();
        true
    }

    fn delete_forward(&mut self) -> bool {
        if !self.selection.is_empty() {
            self.push_undo();
            self.delete_selection();
            self.goal_col = None;
            self.bump();
            return true;
        }
        if self.selection.caret >= self.char_len() {
            return false;
        }
        self.push_undo();
        let caret = self.selection.caret;
        let (start, end) = (self.byte_of(caret), self.byte_of(caret + 1));
        self.text.replace_range(start..end, "");
        self.goal_col = None;
        self.bump();
        true
    }

    fn delete_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let (start, end) = (self.selection.start(), self.selection.end());
        let (start_byte, end_byte) = (self.byte_of(start), self.byte_of(end));
        self.text.replace_range(start_byte..end_byte, "");
        self.selection = Selection::at(start);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selection: self.selection,
        }
    }

    fn push_undo(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.text = previous.text;
        self.selection = previous.selection;
        self.goal_col = None;
        self.bump();
        true
    }

    fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.text = next.text;
        self.selection = next.selection;
        self.goal_col = None;
        self.bump();
        true
    }

    fn set_preedit(&mut self, preedit: Option<Preedit>) {
        if self.preedit != preedit {
            self.preedit = preedit;
            self.bump();
        }
    }

    fn commit_ime(&mut self, text: &str) -> bool {
        let had_preedit = self.preedit.take().is_some();
        if text.is_empty() {
            if had_preedit {
                self.bump();
            }
            return false;
        }
        self.insert(text)
    }

    fn move_caret(&mut self, motion: Motion, extend: bool) {
        if !matches!(motion, Motion::Up | Motion::Down) {
            self.goal_col = None;
        }
        let before = self.selection;
        let len = self.char_len();
        let caret = self.selection.caret;
        let target = match motion {
            Motion::Left => {
                if !extend && !self.selection.is_empty() {
                    self.selection.start()
                } else {
                    caret.saturating_sub(1)
                }
            }
            Motion::Right => {
                if !extend && !self.selection.is_empty() {
                    self.selection.end()
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
        self.selection.caret = target;
        if !extend {
            self.selection.anchor = target;
        }
        if self.selection != before {
            self.bump();
        }
    }

    fn word_left(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut index = pos.min(chars.len());
        while index > 0 && chars[index - 1].is_whitespace() {
            index -= 1;
        }
        while index > 0 && !chars[index - 1].is_whitespace() {
            index -= 1;
        }
        index
    }

    fn word_right(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut index = pos.min(chars.len());
        while index < chars.len() && !chars[index].is_whitespace() {
            index += 1;
        }
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        index
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

    fn vertical(&mut self, pos: usize, down: bool) -> usize {
        let lines = self.lines();
        let (line, col) = self.line_col(pos);
        let goal = *self.goal_col.get_or_insert(col);
        if down {
            if line + 1 >= lines.len() {
                self.char_len()
            } else {
                let (start, len) = lines[line + 1];
                start + goal.min(len)
            }
        } else if line == 0 {
            0
        } else {
            let (start, len) = lines[line - 1];
            start + goal.min(len)
        }
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte_of(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(byte, _)| byte)
    }

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

    fn lines(&self) -> Vec<(usize, usize)> {
        let mut lines = Vec::new();
        let mut start = 0;
        let mut count = 0;
        for (index, ch) in self.text.chars().enumerate() {
            if ch == '\n' {
                lines.push((start, count));
                start = index + 1;
                count = 0;
            } else {
                count += 1;
            }
        }
        lines.push((start, count));
        lines
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
}
