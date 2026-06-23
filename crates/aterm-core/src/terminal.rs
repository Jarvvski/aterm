//! VT integration via `alacritty_terminal` (the published crate 0.26, NOT Zed's
//! fork). Ticket T-1.2.
//!
//! [`Terminal`] owns an alacritty `Term` plus the `vte` ANSI `Processor` and drives
//! them by feeding (already OSC-filtered) PTY bytes through [`Terminal::feed`]. A
//! renderer-neutral [`Snapshot`] (rows of [`SnapshotCell`]) is exposed for the UI
//! layer so the renderer never touches alacritty types directly - the snapshot is
//! built *via* `Term::renderable_content()` so it honours scrollback display
//! offset and the alt-screen flag. Window events the VT engine raises (title,
//! bell, clipboard, `PtyWrite`, ...) are mapped to a neutral [`TerminalEvent`] and
//! forwarded over a channel for the app to drain.
//!
//! API pin note - re-verified against `alacritty_terminal` 0.26.0 (ADR-0007 asks
//! for this before bumping the pin):
//! - `Term::new<D: Dimensions>(config: term::Config, dims: &D, listener: T) -> Term<T>`.
//! - `vte::ansi::Processor::advance<H: Handler>(&mut self, handler: &mut H, &[u8])`;
//!   `Term<T: EventListener>` implements `Handler`, so we pass `&mut Term`.
//! - `Term::renderable_content(&self) -> RenderableContent { display_iter, cursor,
//!   display_offset, selection, colors, mode }`; `display_iter` yields
//!   `Indexed<&Cell> { point, cell }`.
//! - `Term::damage(&mut self) -> TermDamage` (`Full` | `Partial(iter LineDamageBounds
//!   { line, left, right })`), cleared by `Term::reset_damage`.
//! - `Term::mode(&self) -> &TermMode` (alt-screen is `TermMode::ALT_SCREEN`).
//! - `EventListener::send_event(&self, Event)`; `Event::{Title, ResetTitle, Bell,
//!   ClipboardStore, ClipboardLoad, PtyWrite, CursorBlinkingChange, Exit, ...}`.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{point_to_viewport, Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use crossbeam_channel::{Receiver, Sender};

/// Default scrollback (lines of history). Matches alacritty's own default and the
/// dossier; surfaced as a config knob via [`Terminal::with_scrollback`].
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// Bound on the VT window-event channel (title/bell/clipboard/`PtyWrite`/...). The
/// channel is drained by the app at frame rate, but the VT engine *produces* into
/// it synchronously on the model thread while parsing, so an adversarial child (a
/// tight `\x1b[6n` DSR or bell loop) could otherwise enqueue events without bound.
/// A bounded channel plus drop-on-full (see [`ChannelListener::send_event`]) keeps
/// engine memory bounded by construction (ticket T-1.3). Generous enough that a
/// legitimate burst (a TUI redraw's handful of title/cursor events) never drops.
pub(crate) const EVENT_CHANNEL_CAP: usize = 1_024;

/// Color of a cell as resolved by the VT engine, in a renderer-neutral form.
///
/// We deliberately keep alacritty's `Color` semantics (named / indexed / true
/// color) so the UI layer can map named/indexed colors onto the active
/// `aterm-tokens` theme palette rather than baking colors in here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    /// A semantic/named slot. Carries the raw alacritty `NamedColor` discriminant
    /// so the UI can theme it: `0..=15` are the ANSI base+bright slots, and the
    /// non-contiguous high values are the semantic ones - `256` Foreground, `257`
    /// Background, `258` Cursor, `259..=267` Dim ANSI, `268` BrightForeground,
    /// `269` DimForeground. **Must be `u16`**: a plain default-fg/bg cell carries
    /// `256`/`257`, which a `u8` would truncate to `0`/`1` (Black/Red).
    Named(u16),
    /// ANSI/256 palette index.
    Indexed(u8),
    /// 24-bit true color.
    Rgb(u8, u8, u8),
}

/// `NamedColor::Foreground` - the default text color slot.
const NAMED_FOREGROUND: u16 = 256;
/// `NamedColor::Background` - the default background color slot.
const NAMED_BACKGROUND: u16 = 257;

/// A single renderable cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCell {
    pub c: char,
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    /// The leading cell of a wide (e.g. CJK) glyph that occupies two columns.
    pub wide: bool,
    /// The trailing spacer cell of a wide glyph; the renderer skips it (the glyph
    /// is drawn from the [`SnapshotCell::wide`] cell across both columns).
    pub wide_spacer: bool,
}

impl Default for SnapshotCell {
    fn default() -> Self {
        Self {
            c: ' ',
            // Default text on the default background - the semantic slots, NOT
            // ANSI Black/White, so the UI themes a blank cell correctly.
            fg: CellColor::Named(NAMED_FOREGROUND),
            bg: CellColor::Named(NAMED_BACKGROUND),
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            wide: false,
            wide_spacer: false,
        }
    }
}

/// Cursor position in the visible grid (row/col, 0-based from the viewport top).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPos {
    pub row: usize,
    pub col: usize,
}

/// An immutable snapshot of the visible grid for one frame.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Monotonic publish version, assigned by the engine's model thread on each
    /// publish (ticket T-1.3) so a consumer can tell two successive snapshots
    /// apart and detect a missed frame. A snapshot produced by a bare
    /// [`Terminal::snapshot`] (no engine) is version `0`; the engine stamps
    /// `1, 2, 3, ...` as it publishes.
    pub version: u64,
    pub rows: usize,
    pub cols: usize,
    /// Row-major: `cells[row * cols + col]`.
    pub cells: Vec<SnapshotCell>,
    pub cursor: CursorPos,
    /// Scrollback display offset (0 = bottom / live). Mirrors
    /// `RenderableContent::display_offset` so the renderer knows the scroll
    /// position without touching alacritty types.
    pub display_offset: usize,
    /// Whether the alt-screen (a full-screen app) is active. The UI renders the
    /// alt grid as one surface and suppresses block marks while set (ADR-0007).
    pub alt_screen: bool,
}

impl Snapshot {
    /// An empty `rows` x `cols` snapshot of blank default cells at version 0.
    ///
    /// Used to seed the engine's publish handle before the model thread has
    /// produced its first real snapshot (ticket T-1.3), so a consumer that polls
    /// early sees a coherent (blank) grid rather than a sentinel.
    pub fn empty(rows: usize, cols: usize) -> Self {
        let (rows, cols) = (rows.max(1), cols.max(1));
        Self {
            version: 0,
            rows,
            cols,
            cells: vec![SnapshotCell::default(); rows * cols],
            cursor: CursorPos::default(),
            display_offset: 0,
            alt_screen: false,
        }
    }

    /// Borrow the cells of `row` (0-based from viewport top).
    pub fn row(&self, row: usize) -> &[SnapshotCell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }
}

/// Line-level damage since the last snapshot, in a renderer-neutral form, so the
/// damage-tracking renderer (ticket T-1.8) can redraw only what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    /// The whole viewport changed (alt-screen switch, scroll, insert mode, ...).
    Full,
    /// Only these viewport lines changed (column bounds inclusive).
    Lines(Vec<LineDamage>),
}

/// One damaged viewport line and its inclusive `[left, right]` column bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineDamage {
    pub line: usize,
    pub left: usize,
    pub right: usize,
}

/// A window/control event the VT engine raised, mapped to a renderer-neutral form.
///
/// Only the variants aterm acts on are surfaced; reply-formatter events
/// (`ColorRequest`, `TextAreaSizeRequest`) and pure redraw hints (`Wakeup`,
/// `MouseCursorDirty`) are dropped here. `PtyWrite` carries DA/DSR/cursor-query
/// replies that ticket T-1.9 wires back to the PTY master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// OSC 0/2 window title set.
    Title(String),
    /// Reset to the default title.
    ResetTitle,
    /// Terminal bell.
    Bell,
    /// OSC 52 clipboard store (the text to copy).
    ClipboardStore(String),
    /// A clipboard-load (paste) request; the reply path is ticket T-1.9.
    ClipboardLoad,
    /// Text the engine wants written back to the PTY (DA/DSR/CPR replies). Wired
    /// to the master writer in ticket T-1.9.
    PtyWrite(String),
    /// Cursor blink state changed.
    CursorBlinkingChange,
    /// The engine requested shutdown.
    Exit,
}

/// Map an alacritty [`Event`] to the neutral [`TerminalEvent`], or `None` to drop.
fn map_event(event: Event) -> Option<TerminalEvent> {
    Some(match event {
        Event::Title(t) => TerminalEvent::Title(t),
        Event::ResetTitle => TerminalEvent::ResetTitle,
        Event::Bell => TerminalEvent::Bell,
        Event::ClipboardStore(_, s) => TerminalEvent::ClipboardStore(s),
        Event::ClipboardLoad(_, _) => TerminalEvent::ClipboardLoad,
        Event::PtyWrite(s) => TerminalEvent::PtyWrite(s),
        Event::CursorBlinkingChange => TerminalEvent::CursorBlinkingChange,
        Event::Exit => TerminalEvent::Exit,
        // MouseCursorDirty, ColorRequest, TextAreaSizeRequest, Wakeup, ChildExit:
        // either pure redraw hints or reply-formatters not used by aterm's loop.
        _ => return None,
    })
}

/// The `EventListener` alacritty calls synchronously during parsing; it forwards
/// mapped events over a *bounded* channel. Cheap to clone (just clones the
/// `Sender`).
#[derive(Clone)]
struct ChannelListener {
    tx: Sender<TerminalEvent>,
}

impl EventListener for ChannelListener {
    fn send_event(&self, event: Event) {
        if let Some(ev) = map_event(event) {
            // `try_send`, never blocking. This runs synchronously inside the VT
            // parser on the model thread, which is also the only guaranteed
            // drainer - so a *blocking* send on a full channel could deadlock the
            // model thread against itself. Under an adversarial control-sequence
            // flood (a tight DSR/bell/title loop) we therefore DROP rather than
            // grow the queue without bound, keeping engine memory bounded by
            // construction (ticket T-1.3). The forwarded events are latest-wins or
            // coalescable (Title/Bell/CursorBlink), and `PtyWrite` query replies
            // degrade gracefully for a child querying faster than we can answer
            // (the reply path is ticket T-1.9). `Err` (Full or Disconnected) is
            // the intended drop.
            let _ = self.tx.try_send(ev);
        }
    }
}

/// Dimensions newtype implementing alacritty's `Dimensions` so we can construct
/// and resize `Term` without leaking alacritty types into our call sites.
#[derive(Debug, Clone, Copy)]
struct GridDims {
    rows: usize,
    cols: usize,
}

impl Dimensions for GridDims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Owns the alacritty `Term` + VT parser and exposes a renderable snapshot, the
/// alt-screen mode, line damage, and a channel of window events.
pub struct Terminal {
    term: Term<ChannelListener>,
    parser: Processor,
    rows: usize,
    cols: usize,
    scrollback: usize,
    events_rx: Receiver<TerminalEvent>,
}

impl Terminal {
    /// Construct a terminal sized `rows` x `cols` with the default scrollback.
    pub fn new(rows: usize, cols: usize) -> Self {
        Self::with_scrollback(rows, cols, DEFAULT_SCROLLBACK)
    }

    /// Construct a terminal with an explicit scrollback (history) line count.
    pub fn with_scrollback(rows: usize, cols: usize, scrollback: usize) -> Self {
        // alacritty's grid underflows on a 0 dimension; a terminal is at least 1x1.
        let (rows, cols) = (rows.max(1), cols.max(1));
        let (tx, events_rx) = crossbeam_channel::bounded(EVENT_CHANNEL_CAP);
        let dims = GridDims { rows, cols };
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &dims, ChannelListener { tx });
        Self {
            term,
            parser: Processor::new(),
            rows,
            cols,
            scrollback,
            events_rx,
        }
    }

    /// Feed raw VT bytes through the parser into the grid. Call this with the
    /// OSC-pre-parser's *already-filtered* passthrough stream (the OSC-133/7 filter
    /// of ticket T-2.1 sits in front of this, with detected marks travelling a
    /// separate side channel - this entry only parses bytes into the grid).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the grid (on window resize); alacritty reflows the live grid and
    /// scrollback. Debouncing is the caller's responsibility. Dimensions are
    /// clamped to a 1x1 minimum (alacritty's grid underflows on a 0 dimension).
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        self.rows = rows;
        self.cols = cols;
        self.term.resize(GridDims { rows, cols });
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn scrollback(&self) -> usize {
        self.scrollback
    }

    /// Whether the alternate screen (a full-screen app like vim) is active.
    pub fn is_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Borrow the channel of window events (title/bell/clipboard/PtyWrite/...).
    pub fn events(&self) -> &Receiver<TerminalEvent> {
        &self.events_rx
    }

    /// Read and clear the accumulated line damage since the last call. The
    /// damage-tracking renderer (ticket T-1.8) drains this each frame.
    pub fn take_damage(&mut self) -> Damage {
        let damage = match self.term.damage() {
            TermDamage::Full => Damage::Full,
            TermDamage::Partial(iter) => Damage::Lines(
                iter.map(|b| LineDamage {
                    line: b.line,
                    left: b.left,
                    right: b.right,
                })
                .collect(),
            ),
        };
        self.term.reset_damage();
        damage
    }

    /// Produce a renderer-neutral snapshot of the current visible viewport, built
    /// from `renderable_content()` so it honours the scrollback display offset.
    ///
    /// Allocates a fresh `rows * cols` cell buffer; the zero-allocation path that
    /// reuses a buffer across publishes is [`Terminal::snapshot_into`] (the engine
    /// uses it from T-1.4). Intentionally drops selection/true-color-palette for
    /// now (added with the renderer in Epic 1.5/1.6).
    pub fn snapshot(&self) -> Snapshot {
        let mut out = Snapshot::empty(self.rows, self.cols);
        self.snapshot_into(&mut out);
        out
    }

    /// Render the current viewport into `out` **in place**, reusing its `cells`
    /// buffer (it reallocates only when the grid dimensions grow past capacity).
    /// This is the zero-per-publish-allocation path the engine's double-buffer
    /// pool drives (ticket T-1.4). The `version` field is left untouched - the
    /// engine stamps the publish version after rendering.
    pub fn snapshot_into(&self, out: &mut Snapshot) {
        let rows = self.rows;
        let cols = self.cols;

        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let alt_screen = content.mode.contains(TermMode::ALT_SCREEN);
        let cursor_point = content.cursor.point;

        out.rows = rows;
        out.cols = cols;
        out.display_offset = display_offset;
        out.alt_screen = alt_screen;

        // Reuse the existing allocation: clear to length 0 (capacity retained)
        // then refill with blank cells, so a same-size grid never reallocates.
        out.cells.clear();
        out.cells.resize(rows * cols, SnapshotCell::default());
        for indexed in content.display_iter {
            let Some(vp) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            let (row, col) = (vp.line, vp.column.0);
            if row >= rows || col >= cols {
                continue;
            }
            let cell = indexed.cell;
            let flags = cell.flags;
            out.cells[row * cols + col] = SnapshotCell {
                c: cell.c,
                fg: map_color(cell.fg),
                bg: map_color(cell.bg),
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.contains(Flags::UNDERLINE),
                inverse: flags.contains(Flags::INVERSE),
                wide: flags.contains(Flags::WIDE_CHAR),
                wide_spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
            };
        }

        out.cursor = point_to_viewport(display_offset, cursor_point)
            .map(|vp| CursorPos {
                row: vp.line,
                col: vp.column.0,
            })
            .unwrap_or_default();
    }
}

/// Map alacritty's `Color` to our renderer-neutral [`CellColor`].
fn map_color(c: alacritty_terminal::vte::ansi::Color) -> CellColor {
    use alacritty_terminal::vte::ansi::Color as AC;
    match c {
        // `NamedColor` is non-contiguous (Foreground=256, Background=257, ...), so
        // the carrier must be u16 - `as u8` would alias 256/257 onto Black/Red.
        AC::Named(named) => CellColor::Named(named as u16),
        AC::Indexed(i) => CellColor::Indexed(i),
        AC::Spec(rgb) => CellColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_lands_in_grid() {
        let mut t = Terminal::new(5, 20);
        t.feed(b"hi");
        let snap = t.snapshot();
        assert_eq!(snap.rows, 5);
        assert_eq!(snap.cols, 20);
        assert_eq!(snap.row(0)[0].c, 'h');
        assert_eq!(snap.row(0)[1].c, 'i');
        assert_eq!(snap.display_offset, 0);
        assert!(!snap.alt_screen);
    }

    #[test]
    fn newline_advances_row() {
        let mut t = Terminal::new(5, 20);
        // CRLF so the column resets too.
        t.feed(b"a\r\nb");
        let snap = t.snapshot();
        assert_eq!(snap.row(0)[0].c, 'a');
        assert_eq!(snap.row(1)[0].c, 'b');
    }

    #[test]
    fn sgr_bold_sets_flag() {
        let mut t = Terminal::new(3, 10);
        t.feed(b"\x1b[1mX\x1b[0m");
        let snap = t.snapshot();
        assert!(snap.row(0)[0].bold);
    }

    #[test]
    fn sgr_heavy_indexed_and_truecolor() {
        let mut t = Terminal::new(3, 10);
        // 256-color fg on 'R', then 24-bit bg on 'G', then reset.
        t.feed(b"\x1b[38;5;196mR\x1b[48;2;0;255;0mG\x1b[0m");
        let snap = t.snapshot();
        assert_eq!(snap.row(0)[0].c, 'R');
        assert_eq!(snap.row(0)[0].fg, CellColor::Indexed(196));
        assert_eq!(snap.row(0)[1].c, 'G');
        assert_eq!(snap.row(0)[1].bg, CellColor::Rgb(0, 255, 0));
    }

    #[test]
    fn unicode_and_cjk_wide_chars() {
        let mut t = Terminal::new(3, 20);
        // ASCII + accented (width 1) + two CJK (width 2 each).
        t.feed("café 你好".as_bytes());
        let snap = t.snapshot();
        assert_eq!(snap.row(0)[0].c, 'c');
        assert_eq!(snap.row(0)[3].c, 'é');
        // space at col 4, then the wide CJK glyphs occupy cols 5-6 and 7-8: the
        // leading cell carries the glyph + wide flag, the next is its spacer.
        assert_eq!(snap.row(0)[5].c, '你');
        assert!(snap.row(0)[5].wide, "leading wide cell should be flagged");
        assert!(
            snap.row(0)[6].wide_spacer,
            "trailing cell should be a spacer"
        );
        assert_eq!(snap.row(0)[7].c, '好');
        assert!(snap.row(0)[7].wide);
    }

    #[test]
    fn alt_screen_mode_toggles() {
        let mut t = Terminal::new(10, 40);
        assert!(!t.is_alt_screen());
        t.feed(b"\x1b[?1049h");
        assert!(t.is_alt_screen(), "?1049h should enter alt-screen");
        assert!(t.snapshot().alt_screen);
        t.feed(b"\x1b[?1049l");
        assert!(!t.is_alt_screen(), "?1049l should leave alt-screen");
        assert!(!t.snapshot().alt_screen);
    }

    #[test]
    fn alt_screen_redraw_then_restore_cells() {
        // Covers the AC's "alt-screen vim redraw produces expected cells" case.
        let mut t = Terminal::new(5, 20);
        t.feed(b"primary");
        assert_eq!(t.snapshot().row(0)[0].c, 'p');
        // Enter alt-screen and draw a minimal redraw; primary content is hidden.
        // A full-screen app positions the cursor explicitly (here: home) before
        // drawing - the saved primary cursor column does not carry over.
        t.feed(b"\x1b[?1049h");
        t.feed(b"\x1b[H");
        t.feed(b"ALT");
        let alt = t.snapshot();
        assert!(alt.alt_screen);
        assert_eq!(alt.row(0)[0].c, 'A');
        assert_eq!(alt.row(0)[2].c, 'T');
        // Leaving restores the primary screen and its content.
        t.feed(b"\x1b[?1049l");
        let restored = t.snapshot();
        assert!(!restored.alt_screen);
        assert_eq!(
            restored.row(0)[0].c,
            'p',
            "primary content restored on exit"
        );
    }

    #[test]
    fn osc_title_fires_event() {
        let mut t = Terminal::new(5, 20);
        t.feed(b"\x1b]0;hello-title\x07");
        let ev = t.events().try_recv().expect("a title event");
        assert_eq!(ev, TerminalEvent::Title("hello-title".to_string()));
        // OSC 2 also sets the title.
        t.feed(b"\x1b]2;second\x07");
        assert_eq!(
            t.events().try_recv().expect("a second title event"),
            TerminalEvent::Title("second".to_string())
        );
    }

    #[test]
    fn maximized_resize_reflows_without_panic() {
        let mut t = Terminal::new(24, 80);
        t.feed(b"some content that will reflow\r\nacross multiple lines\r\n");
        t.resize(60, 200);
        assert_eq!(t.rows(), 60);
        assert_eq!(t.cols(), 200);
        let snap = t.snapshot();
        assert_eq!(snap.cells.len(), 60 * 200);
        // Shrinking back also must not panic.
        t.resize(24, 80);
        assert_eq!(t.snapshot().cells.len(), 24 * 80);
    }

    #[test]
    fn scrollback_is_config_surfaced() {
        assert_eq!(Terminal::new(24, 80).scrollback(), DEFAULT_SCROLLBACK);
        assert_eq!(Terminal::with_scrollback(24, 80, 500).scrollback(), 500);
    }

    #[test]
    fn feed_produces_partial_damage_after_initial_full() {
        let mut t = Terminal::new(5, 20);
        // A fresh Term is initialized fully damaged; drain that so we exercise the
        // real change tracking (and prove take_damage's reset clears the flag).
        assert!(
            matches!(t.take_damage(), Damage::Full),
            "a fresh terminal reports Full damage"
        );
        t.feed(b"hello");
        match t.take_damage() {
            Damage::Full => panic!("after reset, one written line should be Partial, not Full"),
            Damage::Lines(lines) => {
                let line0 = lines.iter().find(|l| l.line == 0).expect("line 0 damaged");
                assert!(
                    line0.right >= 4,
                    "damage should reach the last written column, got {line0:?}"
                );
            }
        }
    }

    #[test]
    fn default_and_ansi_named_colors_are_distinct() {
        // Regression: a default-fg cell must map to the Foreground slot (256), NOT
        // collapse onto ANSI Black/Red through a lossy u8 cast.
        let mut t = Terminal::new(3, 10);
        t.feed(b"Z");
        assert_eq!(t.snapshot().row(0)[0].fg, CellColor::Named(256)); // Foreground

        // An explicit ANSI red must stay distinct from the default foreground.
        let mut t2 = Terminal::new(3, 10);
        t2.feed(b"\x1b[31mX");
        assert_eq!(t2.snapshot().row(0)[0].fg, CellColor::Named(1)); // Red
        assert_ne!(CellColor::Named(256), CellColor::Named(1));
    }

    #[test]
    fn zero_dimensions_are_clamped_not_panicking() {
        // 0x0 construction must clamp to 1x1 and not panic on snapshot.
        let mut t = Terminal::new(0, 0);
        assert_eq!((t.rows(), t.cols()), (1, 1));
        let _ = t.snapshot();
        // Resizing to a 0 dimension must not panic either.
        t.resize(0, 0);
        assert_eq!((t.rows(), t.cols()), (1, 1));
        let _ = t.snapshot();
    }

    #[test]
    fn snapshot_into_reuses_buffer_at_stable_dims() {
        // The zero-per-publish-allocation guarantee (ticket T-1.4): re-rendering
        // into the same buffer at unchanged dimensions must NOT reallocate the
        // cells Vec - same capacity, same backing pointer.
        let mut t = Terminal::new(10, 40);
        t.feed(b"hello");
        let mut snap = Snapshot::empty(10, 40);
        t.snapshot_into(&mut snap);
        let cap0 = snap.cells.capacity();
        let ptr0 = snap.cells.as_ptr() as usize;
        assert_eq!(snap.row(0)[0].c, 'h');

        for i in 0..200 {
            t.feed(format!("\r\n{i}").as_bytes());
            t.snapshot_into(&mut snap);
        }
        assert_eq!(
            snap.cells.capacity(),
            cap0,
            "cells capacity must be stable across re-renders (no realloc)"
        );
        assert_eq!(
            snap.cells.as_ptr() as usize,
            ptr0,
            "cells buffer must be reused in place at stable dims"
        );
        assert_eq!(snap.rows, 10);
        assert_eq!(snap.cols, 40);
    }

    #[test]
    fn snapshot_into_matches_snapshot() {
        // snapshot() must be a thin wrapper over snapshot_into(): identical cells.
        let mut t = Terminal::new(5, 20);
        t.feed("\x1b[1mBold\x1b[0m and 你好".as_bytes());
        let owned = t.snapshot();
        let mut into = Snapshot::empty(5, 20);
        t.snapshot_into(&mut into);
        assert_eq!(owned.cells, into.cells);
        assert_eq!(owned.cursor, into.cursor);
        assert_eq!(owned.alt_screen, into.alt_screen);
        assert_eq!(owned.display_offset, into.display_offset);
    }
}
