use std::path::{Path, PathBuf};

/// A single editable file buffer with derived state kept current behind one small
/// interface. File transport stays in `aterm-app`; this model is pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    path: PathBuf,
    text: String,
    dirty: bool,
    word_count: usize,
}

impl Document {
    /// Construct a clean document from text an adapter has already loaded.
    #[must_use]
    pub fn from_text(path: PathBuf, text: String) -> Self {
        let word_count = text.split_whitespace().count();
        Self {
            path,
            text,
            dirty: false,
            word_count,
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
        &self.text
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
        self.text = text;
        self.dirty = true;
    }

    /// Record that an adapter successfully persisted the current contents.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Document;

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
