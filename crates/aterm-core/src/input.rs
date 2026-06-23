//! The unified-input reducer. aterm has ONE input field that is either in
//! `Shell` mode (keystrokes go to the PTY) or `Agent` mode (the line is a prompt
//! to the LLM). The headline behavior: the mode-toggle hotkey mutates ONLY the
//! `mode` - the typed text and cursor are preserved across the toggle, so you can
//! start typing a command, realize you want the agent, hit the hotkey, and your
//! text is still there.
//!
//! Pure logic, no UI and no LLM, so it lives in `aterm-core`: both `aterm-ui`
//! (the input widget that renders it) and `aterm-app` (the routing brain) consume
//! it, and `aterm-ui` cannot depend on `aterm-app` (the arrow is app -> ui).

/// Which surface the input is currently driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Keystrokes are forwarded to the shell PTY.
    Shell,
    /// The line is composed as an agent prompt.
    Agent,
}

/// Events the reducer understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// Insert text at the cursor.
    Insert(String),
    /// Backspace (delete char before cursor).
    Backspace,
    /// Move cursor left / right by one char.
    CursorLeft,
    CursorRight,
    /// Move cursor to start / end.
    Home,
    End,
    /// Submit the current line (returns it to the caller).
    Submit,
    /// Toggle Shell <-> Agent WITHOUT touching the text/cursor.
    ToggleMode,
}

/// The reducer state: text + cursor (char index) + mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputModel {
    text: String,
    /// Cursor position as a char index into `text` (0..=chars).
    cursor: usize,
    mode: InputMode,
}

/// What the host should do as a result of an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    /// Nothing external; state changed internally.
    None,
    /// In Shell mode: forward these bytes to the PTY.
    ToPty(Vec<u8>),
    /// Line submitted; carries the line and the mode it was submitted in.
    Submitted { line: String, mode: InputMode },
}

impl Default for InputModel {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            mode: InputMode::Shell,
        }
    }
}

impl InputModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current text (consumed by the input-widget renderer in `aterm-ui`).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cursor position as a char index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Current routing target.
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset for a given char index.
    fn byte_at(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    /// Apply an event, returning what the host should do.
    pub fn reduce(&mut self, event: InputEvent) -> InputOutcome {
        match event {
            InputEvent::ToggleMode => {
                // ONLY the mode changes; text + cursor are preserved.
                self.mode = match self.mode {
                    InputMode::Shell => InputMode::Agent,
                    InputMode::Agent => InputMode::Shell,
                };
                InputOutcome::None
            }
            InputEvent::Insert(s) => {
                let at = self.byte_at(self.cursor);
                self.text.insert_str(at, &s);
                self.cursor += s.chars().count();
                // In Shell mode, raw typed text is also forwarded to the PTY so
                // the shell echoes/handles it. (Local echo model is the app's;
                // the scaffold forwards bytes directly.)
                if self.mode == InputMode::Shell {
                    InputOutcome::ToPty(s.into_bytes())
                } else {
                    InputOutcome::None
                }
            }
            InputEvent::Backspace => {
                if self.cursor > 0 {
                    let start = self.byte_at(self.cursor - 1);
                    let end = self.byte_at(self.cursor);
                    self.text.replace_range(start..end, "");
                    self.cursor -= 1;
                    if self.mode == InputMode::Shell {
                        return InputOutcome::ToPty(vec![0x7f]); // DEL
                    }
                }
                InputOutcome::None
            }
            InputEvent::CursorLeft => {
                self.cursor = self.cursor.saturating_sub(1);
                InputOutcome::None
            }
            InputEvent::CursorRight => {
                self.cursor = (self.cursor + 1).min(self.char_count());
                InputOutcome::None
            }
            InputEvent::Home => {
                self.cursor = 0;
                InputOutcome::None
            }
            InputEvent::End => {
                self.cursor = self.char_count();
                InputOutcome::None
            }
            InputEvent::Submit => {
                let line = std::mem::take(&mut self.text);
                self.cursor = 0;
                let mode = self.mode;
                if mode == InputMode::Shell {
                    // Forward a carriage return to the PTY to run the command.
                    // (The line itself was already forwarded char-by-char.)
                    let _ = &line;
                    return InputOutcome::ToPty(vec![b'\r']);
                }
                InputOutcome::Submitted { line, mode }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_preserves_text_and_cursor() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("git pu".into()));
        let cursor_before = m.cursor();
        let text_before = m.text().to_string();
        assert_eq!(m.mode(), InputMode::Shell);

        m.reduce(InputEvent::ToggleMode);

        // ONLY the mode changed.
        assert_eq!(m.mode(), InputMode::Agent);
        assert_eq!(m.text(), text_before);
        assert_eq!(m.cursor(), cursor_before);

        // Toggle back.
        m.reduce(InputEvent::ToggleMode);
        assert_eq!(m.mode(), InputMode::Shell);
        assert_eq!(m.text(), "git pu");
    }

    #[test]
    fn shell_insert_forwards_to_pty() {
        let mut m = InputModel::new();
        let out = m.reduce(InputEvent::Insert("ls".into()));
        assert_eq!(out, InputOutcome::ToPty(b"ls".to_vec()));
    }

    #[test]
    fn agent_insert_does_not_forward() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::ToggleMode); // -> Agent
        let out = m.reduce(InputEvent::Insert("explain this".into()));
        assert_eq!(out, InputOutcome::None);
    }

    #[test]
    fn agent_submit_returns_line() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::ToggleMode);
        m.reduce(InputEvent::Insert("what failed?".into()));
        let out = m.reduce(InputEvent::Submit);
        assert_eq!(
            out,
            InputOutcome::Submitted {
                line: "what failed?".into(),
                mode: InputMode::Agent
            }
        );
        assert_eq!(m.text(), ""); // cleared after submit
    }

    #[test]
    fn shell_submit_sends_carriage_return() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("pwd".into()));
        let out = m.reduce(InputEvent::Submit);
        assert_eq!(out, InputOutcome::ToPty(vec![b'\r']));
    }

    #[test]
    fn backspace_edits_text() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("abc".into()));
        m.reduce(InputEvent::Backspace);
        assert_eq!(m.text(), "ab");
        assert_eq!(m.cursor(), 2);
    }

    #[test]
    fn cursor_movement_clamps() {
        let mut m = InputModel::new();
        m.reduce(InputEvent::Insert("ab".into()));
        m.reduce(InputEvent::CursorRight); // already at end
        assert_eq!(m.cursor(), 2);
        m.reduce(InputEvent::Home);
        assert_eq!(m.cursor(), 0);
        m.reduce(InputEvent::CursorLeft); // clamp at 0
        assert_eq!(m.cursor(), 0);
        m.reduce(InputEvent::End);
        assert_eq!(m.cursor(), 2);
    }
}
