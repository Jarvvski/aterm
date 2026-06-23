//! VT integration via `alacritty_terminal` (the published crate, NOT Zed's fork).
//!
//! [`Terminal`] owns an alacritty `Term` plus the `vte` ANSI `Processor` and
//! drives them by feeding PTY bytes through [`Terminal::advance`]. A renderable
//! [`Snapshot`] (rows of [`SnapshotCell`]) is exposed for the UI layer so the
//! renderer never touches alacritty types directly.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

/// Color of a cell as resolved by the VT engine, in a renderer-neutral form.
///
/// We deliberately keep alacritty's `Color` semantics (named / indexed / true
/// color) so the UI layer can map named/indexed colors onto the active
/// `aterm-tokens` theme palette rather than baking colors in here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    /// A semantic/named slot (foreground, background, cursor, ...). Carries the
    /// raw alacritty named index so the UI can theme it.
    Named(u8),
    /// ANSI/256 palette index.
    Indexed(u8),
    /// 24-bit true color.
    Rgb(u8, u8, u8),
}

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
}

impl Default for SnapshotCell {
    fn default() -> Self {
        Self {
            c: ' ',
            fg: CellColor::Named(0),
            bg: CellColor::Named(0),
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
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
    pub rows: usize,
    pub cols: usize,
    /// Row-major: `cells[row * cols + col]`.
    pub cells: Vec<SnapshotCell>,
    pub cursor: CursorPos,
}

impl Snapshot {
    /// Borrow the cells of `row` (0-based from viewport top).
    pub fn row(&self, row: usize) -> &[SnapshotCell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }
}

/// A no-op event listener. Alacritty emits bell/title/clipboard/etc. events; for
/// the scaffold we discard them.
/// TODO(ticket EPIC-1.2): forward title/bell/clipboard events to the app layer.
#[derive(Clone, Default)]
pub struct VoidListener;

impl EventListener for VoidListener {
    fn send_event(&self, _event: Event) {}
}

/// Owns the alacritty `Term` + VT parser and exposes a renderable snapshot.
pub struct Terminal {
    term: Term<VoidListener>,
    parser: Processor,
    rows: usize,
    cols: usize,
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

impl Terminal {
    /// Construct a terminal sized `rows` x `cols`.
    pub fn new(rows: usize, cols: usize) -> Self {
        let dims = GridDims { rows, cols };
        let term = Term::new(Config::default(), &dims, VoidListener);
        Self {
            term,
            parser: Processor::new(),
            rows,
            cols,
        }
    }

    /// Feed raw VT bytes through the parser into the grid. Call this with the
    /// OSC-pre-parser's passthrough stream.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the grid (on window resize).
    pub fn resize(&mut self, rows: usize, cols: usize) {
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

    /// Produce a renderable snapshot of the current visible viewport.
    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let rows = self.rows;
        let cols = self.cols;
        let mut cells = vec![SnapshotCell::default(); rows * cols];

        for row in 0..rows {
            let line = Line(row as i32);
            for col in 0..cols {
                let point = Point::new(line, Column(col));
                let cell = &grid[point];
                let flags = cell.flags;
                cells[row * cols + col] = SnapshotCell {
                    c: cell.c,
                    fg: map_color(cell.fg),
                    bg: map_color(cell.bg),
                    bold: flags.contains(Flags::BOLD),
                    italic: flags.contains(Flags::ITALIC),
                    underline: flags.contains(Flags::UNDERLINE),
                    inverse: flags.contains(Flags::INVERSE),
                };
            }
        }

        let cursor_point = self.term.grid().cursor.point;
        let cursor = CursorPos {
            row: cursor_point.line.0.max(0) as usize,
            col: cursor_point.column.0,
        };

        Snapshot {
            rows,
            cols,
            cells,
            cursor,
        }
    }
}

/// Map alacritty's `Color` to our renderer-neutral [`CellColor`].
fn map_color(c: alacritty_terminal::vte::ansi::Color) -> CellColor {
    use alacritty_terminal::vte::ansi::Color as AC;
    match c {
        AC::Named(named) => CellColor::Named(named as u8),
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
        t.advance(b"hi");
        let snap = t.snapshot();
        assert_eq!(snap.rows, 5);
        assert_eq!(snap.cols, 20);
        assert_eq!(snap.row(0)[0].c, 'h');
        assert_eq!(snap.row(0)[1].c, 'i');
    }

    #[test]
    fn newline_advances_row() {
        let mut t = Terminal::new(5, 20);
        // CRLF so the column resets too.
        t.advance(b"a\r\nb");
        let snap = t.snapshot();
        assert_eq!(snap.row(0)[0].c, 'a');
        assert_eq!(snap.row(1)[0].c, 'b');
    }

    #[test]
    fn sgr_bold_sets_flag() {
        let mut t = Terminal::new(3, 10);
        t.advance(b"\x1b[1mX\x1b[0m");
        let snap = t.snapshot();
        assert!(snap.row(0)[0].bold);
    }

    #[test]
    fn resize_changes_dims() {
        let mut t = Terminal::new(5, 20);
        t.resize(10, 40);
        assert_eq!(t.rows(), 10);
        assert_eq!(t.cols(), 40);
        assert_eq!(t.snapshot().cells.len(), 400);
    }
}
