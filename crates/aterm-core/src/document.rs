use std::path::{Path, PathBuf};

use crate::editing::{EditBuffer, EditEvent, Preedit, Selection};

/// A single editable file buffer with derived state kept current behind one small
/// interface. File transport stays in `aterm-app`; this model is pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    path: PathBuf,
    editor: EditBuffer,
    dirty: bool,
    word_count: usize,
    version: u64,
}

impl Document {
    /// Construct a clean document from text an adapter has already loaded.
    #[must_use]
    pub fn from_text(path: PathBuf, text: String) -> Self {
        let word_count = text.split_whitespace().count();
        Self {
            path,
            editor: EditBuffer::from_text(text),
            dirty: false,
            word_count,
            version: 0,
        }
    }

    /// The file this document saves to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The editable contents.
    #[must_use]
    pub fn text(&self) -> &str {
        self.editor.text()
    }

    /// The primary caret and optional selection anchor.
    #[must_use]
    pub fn selection(&self) -> Selection {
        self.editor.selection()
    }

    /// Monotonic version of the visible editing state for renderer damage gates.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The active IME composition, if one is in progress.
    #[must_use]
    pub fn preedit(&self) -> Option<&Preedit> {
        self.editor.preedit()
    }

    /// Whether the contents have changed since the last successful save.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The current Unicode-whitespace-delimited word count.
    #[must_use]
    pub fn word_count(&self) -> usize {
        self.word_count
    }

    /// Replace the editable contents as one model transition.
    pub fn replace_text(&mut self, text: String) {
        self.word_count = text.split_whitespace().count();
        self.editor = EditBuffer::from_text(text);
        self.dirty = true;
        self.version = self.version.wrapping_add(1);
    }

    /// Apply one inert text edit through the shared editing implementation.
    pub fn reduce(&mut self, event: EditEvent) -> bool {
        let editor_version = self.editor.version();
        let changed = self.editor.reduce(event);
        if self.editor.version() != editor_version {
            self.version = self.version.wrapping_add(1);
        }
        if changed {
            self.word_count = self.editor.text().split_whitespace().count();
            self.dirty = true;
        }
        changed
    }

    /// Record that an adapter successfully persisted the current contents.
    pub fn mark_saved(&mut self) {
        if self.dirty {
            self.dirty = false;
            self.version = self.version.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Document;
    use crate::{EditEvent, Motion, Preedit};

    #[test]
    fn editing_a_document_updates_text_dirty_state_and_word_count() {
        let mut document = Document::from_text(PathBuf::from("notes.md"), "draft".to_string());
        let version = document.version();

        assert!(document.reduce(EditEvent::Insert("first ".to_string())));

        assert_eq!(document.text(), "first draft");
        assert_eq!(document.word_count(), 2);
        assert!(document.is_dirty());
        assert!(document.version() > version);
    }

    #[test]
    fn document_selection_is_replaced_as_one_undoable_edit() {
        let mut document = Document::from_text(PathBuf::from("notes.md"), "alpha beta".to_string());
        document.reduce(EditEvent::Move(Motion::BufferEnd, false));
        document.reduce(EditEvent::Move(Motion::WordLeft, false));
        document.reduce(EditEvent::Move(Motion::End, true));
        assert_eq!(document.selection().start(), 6);
        assert_eq!(document.selection().end(), 10);

        document.reduce(EditEvent::Insert("gamma".to_string()));
        assert_eq!(document.text(), "alpha gamma");

        document.reduce(EditEvent::Undo);
        assert_eq!(document.text(), "alpha beta");
    }

    #[test]
    fn document_preedit_is_transient_and_commit_is_one_undo_unit() {
        let mut document = Document::from_text(PathBuf::from("notes.md"), "ko".to_string());
        document.reduce(EditEvent::Move(Motion::BufferEnd, false));

        document.reduce(EditEvent::SetPreedit(Some(Preedit {
            text: "に".to_string(),
            cursor: None,
        })));
        assert_eq!(document.text(), "ko");
        assert_eq!(
            document.preedit().map(|preedit| preedit.text.as_str()),
            Some("に")
        );

        document.reduce(EditEvent::CommitIme("に".to_string()));
        assert_eq!(document.text(), "koに");
        assert!(document.preedit().is_none());

        document.reduce(EditEvent::Undo);
        assert_eq!(document.text(), "ko");
    }

    #[test]
    fn opened_document_reports_its_path_text_and_word_count_without_being_dirty() {
        let path = PathBuf::from("notes.md");
        let document = Document::from_text(path.clone(), "one  two\nthree".to_string());

        assert_eq!(document.path(), path);
        assert_eq!(document.text(), "one  two\nthree");
        assert_eq!(document.word_count(), 3);
        assert!(!document.is_dirty());
    }

    #[test]
    fn replacing_text_marks_the_document_dirty_and_updates_the_word_count() {
        let mut document =
            Document::from_text(PathBuf::from("notes.md"), "first draft".to_string());

        document.replace_text("a sharper second draft".to_string());

        assert_eq!(document.text(), "a sharper second draft");
        assert_eq!(document.word_count(), 4);
        assert!(document.is_dirty());
    }

    #[test]
    fn successful_save_marks_the_current_contents_clean() {
        let mut document =
            Document::from_text(PathBuf::from("notes.md"), "first draft".to_string());
        document.replace_text("saved draft".to_string());

        document.mark_saved();

        assert_eq!(document.text(), "saved draft");
        assert_eq!(document.word_count(), 2);
        assert!(!document.is_dirty());
    }
}
