use std::io;
use std::path::Path;

use aterm_core::Document;

/// Which top-level surface currently owns the content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AppView {
    /// The terminal timeline and unified input are visible.
    #[default]
    Terminal,
    /// A file-backed writing surface owns the content area.
    Editor,
}

/// App-level editor orchestration. The pure document owns derived state; this module
/// owns file transport and the transition between top-level views.
#[derive(Debug, Default)]
pub(crate) struct EditorSession {
    view: AppView,
    document: Option<Document>,
}

impl EditorSession {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn view(&self) -> AppView {
        self.view
    }

    #[cfg(test)]
    pub(crate) fn document(&self) -> Option<&Document> {
        self.document.as_ref()
    }

    pub(crate) fn open(&mut self, path: &Path) -> io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        self.document = Some(Document::from_text(path.to_path_buf(), text));
        self.view = AppView::Editor;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn replace_text(&mut self, text: String) -> bool {
        let Some(document) = self.document.as_mut() else {
            return false;
        };
        document.replace_text(text);
        true
    }

    pub(crate) fn save(&mut self) -> io::Result<()> {
        let document = self
            .document
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no editor document"))?;
        std::fs::write(document.path(), document.text())?;
        self.document
            .as_mut()
            .expect("document exists after successful write")
            .mark_saved();
        Ok(())
    }

    pub(crate) fn exit_to_terminal(&mut self) {
        self.view = AppView::Terminal;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{AppView, EditorSession};

    fn temp_file(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aterm-{label}-{}-{nonce}.md", std::process::id()))
    }

    #[test]
    fn opening_a_file_enters_editor_with_a_clean_document() {
        let path = temp_file("open");
        fs::write(&path, "one two\nthree").expect("write fixture");
        let mut editor = EditorSession::new();

        editor.open(&path).expect("open document");

        assert_eq!(editor.view(), AppView::Editor);
        let document = editor.document().expect("opened document");
        assert_eq!(document.path(), path);
        assert_eq!(document.text(), "one two\nthree");
        assert_eq!(document.word_count(), 3);
        assert!(!document.is_dirty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn saving_writes_the_edited_text_and_marks_the_document_clean() {
        let path = temp_file("save");
        fs::write(&path, "first draft").expect("write fixture");
        let mut editor = EditorSession::new();
        editor.open(&path).expect("open document");
        editor.replace_text("finished draft".to_string());

        editor.save().expect("save document");

        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            "finished draft"
        );
        assert!(!editor.document().expect("opened document").is_dirty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn failed_save_is_reported_and_keeps_the_document_dirty() {
        let path = temp_file("save-failure");
        fs::write(&path, "first draft").expect("write fixture");
        let mut editor = EditorSession::new();
        editor.open(&path).expect("open document");
        editor.replace_text("unsaved draft".to_string());
        fs::remove_file(&path).expect("remove fixture");
        fs::create_dir(&path).expect("replace file with directory");

        let error = editor
            .save()
            .expect_err("directory path cannot be overwritten as a file");

        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(editor.document().expect("opened document").is_dirty());
        let _ = fs::remove_dir(path);
    }

    #[test]
    fn exiting_to_terminal_retains_an_unsaved_document_in_memory() {
        let path = temp_file("exit");
        fs::write(&path, "first draft").expect("write fixture");
        let mut editor = EditorSession::new();
        editor.open(&path).expect("open document");
        editor.replace_text("draft kept in memory".to_string());

        editor.exit_to_terminal();

        assert_eq!(editor.view(), AppView::Terminal);
        let document = editor.document().expect("retained document");
        assert_eq!(document.text(), "draft kept in memory");
        assert!(document.is_dirty());
        let _ = fs::remove_file(path);
    }
}
