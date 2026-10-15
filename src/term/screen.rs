//! Double-buffered cell grid with diff-based VT output.
//!
//! Each frame is rendered into `cur`, diffed against `prev`, and the minimal
//! set of cursor moves / SGR changes / glyphs is written in one synchronized
//! update (`CSI ?2026 h` ... `CSI ?2026 l`, honoured by Windows Terminal).

use std::io::Write;

use unicode_width::UnicodeWidthChar;

use super::console::Console;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Rgb(u8, u8, u8),
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color::Rgb(r, g, b)
    }
}

pub const BOLD: u8 = 1 << 0;
pub const DIM: u8 = 1 << 1;
pub const ITALIC: u8 = 1 << 2;
pub const UNDERLINE: u8 = 1 << 3;
pub const REVERSE: u8 = 1 << 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: u8,
}

impl Style {
    pub const fn new() -> Self {
        Style {
            fg: Color::Default,
            bg: Color::Default,
            attrs: 0,
        }
    }
    pub const fn fg(mut self, c: Color) -> Self {
        self.fg = c;
        self
    }
    pub const fn bg(mut self, c: Color) -> Self {
        self.bg = c;
        self
    }
    pub const fn bold(mut self) -> Self {
        self.attrs |= BOLD;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
    /// Second half of a double-width glyph; never emitted.
    pub continuation: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: Style::new(),
            continuation: false,
        }
    }
}

/// A rectangular region, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: u16, py: u16) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn right(&self) -> u16 {
        self.x + self.w
    }
    pub fn bottom(&self) -> u16 {
        self.y + self.h
    }
}

pub struct Screen {
    w: u16,
    h: u16,
    cur: Vec<Cell>,
    prev: Vec<Cell>,
    out: Vec<u8>,
    full_redraw: bool,
}

impl Screen {
    pub fn new(w: u16, h: u16) -> Self {
        let n = w as usize * h as usize;
        Screen {
            w,
            h,
            cur: vec![Cell::default(); n],
            prev: vec![Cell::default(); n],
            out: Vec::with_capacity(64 * 1024),
            full_redraw: true,
        }
    }

    pub fn width(&self) -> u16 {
        self.w
    }

    pub fn height(&self) -> u16 {
        self.h
    }

    /// Resize buffers; forces a full repaint on the next flush.
    pub fn resize(&mut self, w: u16, h: u16) {
        if w == self.w && h == self.h {
            return;
        }
        self.w = w;
        self.h = h;
        let n = w as usize * h as usize;
        self.cur = vec![Cell::default(); n];
        self.prev = vec![Cell::default(); n];
        self.full_redraw = true;
    }

    /// Clear the current frame to blank default cells.
    pub fn clear(&mut self) {
        self.cur.fill(Cell::default());
    }

    #[inline]
    fn idx(&self, x: u16, y: u16) -> usize {
        y as usize * self.w as usize + x as usize
    }

    /// Put a single character; returns the width consumed (0 if clipped).
    pub fn put_char(&mut self, x: u16, y: u16, ch: char, style: Style) -> u16 {
        if x >= self.w || y >= self.h {
            return 0;
        }
        let ch = sanitize(ch);
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).clamp(1, 2) as u16;
        if width == 2 && x + 1 >= self.w {
            // Not enough room for a wide glyph: pad with a space instead.
            let i = self.idx(x, y);
            self.cur[i] = Cell {
                ch: ' ',
                style,
                continuation: false,
            };
            return 1;
        }
        let i = self.idx(x, y);
        // If we overwrite the second half of a wide glyph, blank its first half.
        if self.cur[i].continuation && x > 0 {
            self.cur[i - 1].ch = ' ';
        }
        self.cur[i] = Cell {
            ch,
            style,
            continuation: false,
        };
        if width == 2 {
            self.cur[i + 1] = Cell {
                ch: ' ',
                style,
                continuation: true,
            };
        } else if x + 1 < self.w && self.cur[i + 1].continuation {
            // We overwrote the first half of a wide glyph: blank its tail.
            self.cur[i + 1] = Cell {
                ch: ' ',
                style,
                continuation: false,
            };
        }
        width
    }

    /// Put a string clipped to `max_width` cells; returns the width used.
    pub fn put_str(&mut self, x: u16, y: u16, s: &str, style: Style, max_width: u16) -> u16 {
        if y >= self.h || x >= self.w {
            return 0;
        }
        let limit = max_width.min(self.w - x);
        let mut used = 0u16;
        for ch in s.chars() {
            let ch = sanitize(ch);
            let cw = UnicodeWidthChar::width(ch).unwrap_or(1).clamp(1, 2) as u16;
            if used + cw > limit {
                break;
            }
            used += self.put_char(x + used, y, ch, style);
        }
        used
    }

    /// Put a string, truncating with an ellipsis when it does not fit.
    pub fn put_str_ellipsis(
        &mut self,
        x: u16,
        y: u16,
        s: &str,
        style: Style,
        max_width: u16,
    ) -> u16 {
        if max_width == 0 {
            return 0;
        }
        let total = str_width(s);
        if total <= max_width as usize {
            return self.put_str(x, y, s, style, max_width);
        }
        let used = self.put_str(x, y, s, style, max_width.saturating_sub(1));
        self.put_char(x + used, y, '…', style);
        used + 1
    }

    pub fn fill(&mut self, rect: Rect, ch: char, style: Style) {
        for y in rect.y..rect.bottom().min(self.h) {
            for x in rect.x..rect.right().min(self.w) {
                let i = self.idx(x, y);
                self.cur[i] = Cell {
                    ch,
                    style,
                    continuation: false,
                };
            }
        }
    }

    /// Apply a background colour to a horizontal run without changing glyphs.
    pub fn paint_bg(&mut self, x: u16, y: u16, w: u16, bg: Color) {
        if y >= self.h {
            return;
        }
        for xx in x..(x + w).min(self.w) {
            let i = self.idx(xx, y);
            self.cur[i].style.bg = bg;
        }
    }

    /// Diff against the previous frame and write the result to the console.
    pub fn flush(&mut self, console: &Console) {
        self.out.clear();
        self.out.extend_from_slice(b"\x1b[?2026h");
        if self.full_redraw {
            self.out.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.prev.fill(Cell {
                ch: '\0',
                style: Style::new(),
                continuation: false,
            });
            self.full_redraw = false;
        }

        let mut last_style: Option<Style> = None;
        // Cursor position known to the terminal, if any.
        let mut cursor: Option<(u16, u16)> = None;
        let w = self.w as usize;
        let mut tmp = [0u8; 4];

        for y in 0..self.h as usize {
            let row = &self.cur[y * w..(y + 1) * w];
            let prow = &self.prev[y * w..(y + 1) * w];
            let mut x = 0usize;
            while x < w {
                let c = row[x];
                if c.continuation {
                    x += 1;
                    continue;
                }
                let width = if x + 1 < w && row[x + 1].continuation {
                    2
                } else {
                    1
                };
                let changed = c != prow[x] || (width == 2 && row[x + 1] != prow[x + 1]);
                if !changed {
                    x += 1;
                    continue;
                }
                if cursor != Some((x as u16, y as u16)) {
                    let _ = write!(self.out, "\x1b[{};{}H", y + 1, x + 1);
                }
                if last_style != Some(c.style) {
                    emit_sgr(&mut self.out, c.style);
                    last_style = Some(c.style);
                }
                self.out
                    .extend_from_slice(c.ch.encode_utf8(&mut tmp).as_bytes());
                let nx = x + width;
                // After writing into the last column the cursor state is host-dependent.
                cursor = if nx < w {
                    Some((nx as u16, y as u16))
                } else {
                    None
                };
                x = nx;
            }
        }

        self.out.extend_from_slice(b"\x1b[0m\x1b[?2026l");
        console.write_all(&self.out);
        std::mem::swap(&mut self.cur, &mut self.prev);
        // `cur` now holds the stale frame; the renderer clears it before drawing.
    }
}

impl Screen {
    /// Plain-text rendering of the last flushed frame (debugging aid).
    pub fn last_frame_text(&self) -> String {
        let w = self.w as usize;
        let mut out = String::with_capacity(self.prev.len() + self.h as usize);
        for y in 0..self.h as usize {
            let row = &self.prev[y * w..(y + 1) * w];
            let line: String = row
                .iter()
                .filter(|c| !c.continuation)
                .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                .collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}

fn sanitize(ch: char) -> char {
    if ch.is_control() || ch == '\u{FEFF}' {
        ' '
    } else {
        ch
    }
}

pub fn str_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(sanitize(c)).unwrap_or(1).clamp(1, 2))
        .sum()
}

fn emit_sgr(out: &mut Vec<u8>, s: Style) {
    out.extend_from_slice(b"\x1b[0");
    if s.attrs & BOLD != 0 {
        out.extend_from_slice(b";1");
    }
    if s.attrs & DIM != 0 {
        out.extend_from_slice(b";2");
    }
    if s.attrs & ITALIC != 0 {
        out.extend_from_slice(b";3");
    }
    if s.attrs & UNDERLINE != 0 {
        out.extend_from_slice(b";4");
    }
    if s.attrs & REVERSE != 0 {
        out.extend_from_slice(b";7");
    }
    if let Color::Rgb(r, g, b) = s.fg {
        let _ = write!(out, ";38;2;{r};{g};{b}");
    }
    if let Color::Rgb(r, g, b) = s.bg {
        let _ = write!(out, ";48;2;{r};{g};{b}");
    }
    out.push(b'm');
}
