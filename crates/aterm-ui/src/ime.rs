//! macOS IME integration (ticket T-3.2): the winit `Ime` event feed for the
//! self-drawn input box, plus the hand-rolled `NSTextInputClient` escape-hatch seam.
//!
//! aterm draws its own input box, so it must drive composition itself rather than
//! leaning on a native text field. winit 0.30 surfaces the platform IME as
//! [`winit::event::Ime`] events; this module maps them to the renderer-neutral
//! [`ImeEvent`] the host routes on (mirroring how [`crate::app::KeyPress`] abstracts
//! winit key events), so `aterm-app` populates [`aterm_core::InputModel`]'s `preedit`
//! field without naming winit beyond this seam.
//!
//! The `preedit-active` routing gate itself lives in the routing brain (T-3.3): it
//! reads `preedit.is_some()` FIRST, so Enter during composition confirms the IME
//! candidate and never submits (the Zed terminal #23003 trap). This module's job is
//! only to keep `preedit` populated/cleared correctly and to position the candidate
//! window under the caret (via [`winit::window::Window::set_ime_cursor_area`], driven
//! from [`crate::app`] using the caret rect the input widget records each frame).
//!
//! ## Decision: winit's IME is sufficient for v1 (AC5)
//!
//! For the target IMEs (Japanese, Pinyin) winit 0.30's `Ime` events cover everything
//! the box needs: inline preedit with a byte-indexed cursor range
//! ([`ImeEvent::Preedit`]), the committed string ([`ImeEvent::Commit`]),
//! enable/disable ([`ImeEvent::Enabled`] / [`ImeEvent::Disabled`]), and
//! `set_ime_cursor_area` to place the candidate window. The known winit gaps -
//! `_selected_range`/`_replacement_range` ignored (winit #3617) and the historical
//! Pinyin `set_marked_text` OOB crash - do not affect inline composition + commit for
//! these IMEs, so we ship on winit and do NOT hand-roll `NSTextInputClient`.
//!
//! The escape hatch is nonetheless designed as a seam ([`NativeTextInput`]): if a
//! real CJK gap is reported, a hand-rolled `NSTextInputClient` on a single raw
//! `NSView` (the Zed/GPUI model, via `objc2`) can be slotted in behind that trait
//! without disturbing the host, which only speaks [`ImeEvent`]. It is a marker, not an
//! implementation.

/// A renderer-neutral IME event, mapped from [`winit::event::Ime`] by
/// [`ImeEvent::from_winit`]. The host ([`crate::app::UiCallbacks::on_ime`]) drives its
/// [`aterm_core::InputModel`] from these without depending on winit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEvent {
    /// The IME was enabled; composition may follow. The host should begin issuing
    /// `set_ime_cursor_area` (which [`crate::app`] does from the recorded caret rect).
    Enabled,
    /// A new composition string should be shown at the caret. `cursor` is the
    /// candidate cursor range as **byte** indices into `text` (winit's units;
    /// `None` = hide the cursor). An empty `text` signals the preedit was cleared.
    Preedit {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    /// The final text to insert into the buffer (composition finished). winit sends an
    /// empty [`Self::Preedit`] immediately before this.
    Commit(String),
    /// The IME was disabled; any dangling preedit must be cleared and IME requests
    /// stopped until the next [`Self::Enabled`].
    Disabled,
}

impl ImeEvent {
    /// Map a winit [`winit::event::Ime`] to the neutral event. Total (every winit
    /// variant maps), so the `window_event` handler stays a one-liner.
    #[must_use]
    pub fn from_winit(ime: winit::event::Ime) -> Self {
        use winit::event::Ime;
        match ime {
            Ime::Enabled => ImeEvent::Enabled,
            Ime::Preedit(text, cursor) => ImeEvent::Preedit { text, cursor },
            Ime::Commit(text) => ImeEvent::Commit(text),
            Ime::Disabled => ImeEvent::Disabled,
        }
    }
}

/// The `NSTextInputClient` escape-hatch seam (ticket T-3.2). NOT implemented - winit's
/// IME is sufficient for v1 (see the module docs). This trait marks where a hand-rolled
/// `NSTextInputClient` on a single raw `NSView` (via `objc2`) would attach if a CJK gap
/// in winit is reported, so the swap is a localized change: the host would keep speaking
/// [`ImeEvent`] and only the source of those events would change. The method set mirrors
/// the `NSTextInputClient` protocol surface the box needs.
pub trait NativeTextInput {
    /// The composition string was set (`NSTextInputClient::setMarkedText:...`); `cursor`
    /// is the byte-indexed candidate range, matching [`ImeEvent::Preedit`].
    fn set_marked_text(&mut self, text: &str, cursor: Option<(usize, usize)>);
    /// The composition committed (`NSTextInputClient::insertText:...`).
    fn insert_text(&mut self, text: &str);
    /// The composition was abandoned (`NSTextInputClient::unmarkText`).
    fn unmark_text(&mut self);
    /// Whether a composition is currently active (`NSTextInputClient::hasMarkedText`).
    fn has_marked_text(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::Ime;

    #[test]
    fn from_winit_maps_every_variant() {
        assert_eq!(ImeEvent::from_winit(Ime::Enabled), ImeEvent::Enabled);
        assert_eq!(ImeEvent::from_winit(Ime::Disabled), ImeEvent::Disabled);
        assert_eq!(
            ImeEvent::from_winit(Ime::Preedit("にほ".to_string(), Some((3, 6)))),
            ImeEvent::Preedit {
                text: "にほ".to_string(),
                cursor: Some((3, 6)),
            }
        );
        assert_eq!(
            ImeEvent::from_winit(Ime::Preedit(String::new(), None)),
            ImeEvent::Preedit {
                text: String::new(),
                cursor: None,
            },
            "an empty preedit maps through as-is (the host treats it as 'cleared')"
        );
        assert_eq!(
            ImeEvent::from_winit(Ime::Commit("日本".to_string())),
            ImeEvent::Commit("日本".to_string())
        );
    }
}
