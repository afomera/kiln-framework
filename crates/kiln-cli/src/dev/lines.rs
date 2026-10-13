//! Process output as lines: bytes from a pipe or PTY become [`Line`]s that keep their
//! colors (SGR escapes) but drop everything else a terminal would interpret (cursor
//! moves, line clears, titles), and [`Ring`] keeps the last few thousand of them.

use std::collections::VecDeque;

/// Longest line kept, in bytes; anything past it is dropped (a process printing megabytes
/// without a newline can't grow memory without bound).
const MAX_LINE: usize = 16 * 1024;

/// One line of output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The text with its color escapes (`ESC [ ... m`) and nothing else.
    pub text: String,
    /// The text without escapes, for search and filtering.
    pub plain: String,
    /// Written by `kiln dev` itself (started, exited, restarting), not the process.
    pub system: bool,
}

impl Line {
    /// A line of process output; `text` may contain color escapes.
    pub fn new(text: String) -> Self {
        Self { plain: strip(&text), text, system: false }
    }

    /// A message from `kiln dev` about the process.
    pub fn system(message: impl Into<String>) -> Self {
        let message = message.into();
        Self { text: message.clone(), plain: message, system: true }
    }
}

/// `text` without its color escapes.
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Only SGR escapes survive the assembler: skip to the final `m`.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// After ESC.
    Start,
    /// After ESC and one of `( ) * + # %`: one more byte.
    Charset,
    /// In `ESC [ ...`.
    Csi,
    /// In `ESC ] ...` (titles, hyperlinks), until BEL or `ESC \`.
    Osc,
    OscEnd,
}

/// Turns a stream of bytes into lines. Carriage returns without a newline (progress bars)
/// replace the line so far, and colors carry over to the next line until they're reset.
#[derive(Debug)]
pub struct Assembler {
    /// The bytes as written, minus the newline, for plain output.
    raw: Vec<u8>,
    /// The bytes as displayed: color escapes and text.
    display: Vec<u8>,
    escape: Escape,
    csi: Vec<u8>,
    /// A `\r` that may be the start of `\r\n`.
    carriage_return: bool,
    /// Visible columns in `display`, for tab stops.
    column: usize,
    /// Color escapes in effect at the end of the line so far.
    colors: Vec<u8>,
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

/// A finished line: the bytes as written (for plain output) and as displayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// What the process wrote, without the newline (and a trailing `\r`).
    pub raw: String,
    /// The line as displayed.
    pub line: Line,
}

impl Assembler {
    /// An assembler at the start of a line.
    pub fn new() -> Self {
        Self {
            raw: vec![],
            display: vec![],
            escape: Escape::None,
            csi: vec![],
            carriage_return: false,
            column: 0,
            colors: vec![],
        }
    }

    /// Feeds `bytes` and returns the lines they finished.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Assembled> {
        let mut done = vec![];
        for &byte in bytes {
            if let Some(line) = self.byte(byte) {
                done.push(line);
            }
        }
        done
    }

    /// The unfinished line, if it has any text (a progress bar, a prompt).
    pub fn partial(&self) -> Option<Line> {
        let line = Line::new(String::from_utf8_lossy(&self.display).into_owned());
        (!line.plain.trim().is_empty()).then_some(line)
    }

    /// The unfinished line as a finished one, at end of output.
    pub fn finish(&mut self) -> Option<Assembled> {
        let has_text = !self.raw.is_empty();
        let line = self.end_line();
        has_text.then_some(line)
    }

    fn end_line(&mut self) -> Assembled {
        let mut raw = std::mem::take(&mut self.raw);
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        let display = std::mem::replace(&mut self.display, self.colors.clone());
        self.column = 0;
        self.carriage_return = false;
        Assembled {
            raw: String::from_utf8_lossy(&raw).into_owned(),
            line: Line::new(String::from_utf8_lossy(&display).into_owned()),
        }
    }

    fn byte(&mut self, byte: u8) -> Option<Assembled> {
        if byte == b'\n' {
            self.escape = Escape::None;
            return Some(self.end_line());
        }
        if self.raw.len() < MAX_LINE {
            self.raw.push(byte);
        }
        match self.escape {
            Escape::None => {}
            Escape::Start => {
                self.escape = match byte {
                    b'[' => {
                        self.csi.clear();
                        Escape::Csi
                    }
                    b']' => Escape::Osc,
                    b'(' | b')' | b'*' | b'+' | b'#' | b'%' => Escape::Charset,
                    _ => Escape::None,
                };
                return None;
            }
            Escape::Charset => {
                self.escape = Escape::None;
                return None;
            }
            Escape::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = Escape::None;
                    if byte == b'm' {
                        self.color(byte);
                    }
                } else if self.csi.len() < 64 {
                    self.csi.push(byte);
                } else {
                    self.escape = Escape::None;
                }
                return None;
            }
            Escape::Osc => {
                self.escape = match byte {
                    0x07 => Escape::None,
                    0x1b => Escape::OscEnd,
                    _ => Escape::Osc,
                };
                return None;
            }
            Escape::OscEnd => {
                self.escape = if byte == b'\\' { Escape::None } else { Escape::Osc };
                return None;
            }
        }
        if byte == b'\r' {
            self.carriage_return = true;
            return None;
        }
        if self.carriage_return {
            // `\r` then more text: the line is being redrawn (a progress bar).
            self.carriage_return = false;
            self.display = self.colors.clone();
            self.column = 0;
        }
        match byte {
            0x1b => self.escape = Escape::Start,
            b'\t' => {
                let spaces = 8 - self.column % 8;
                self.push_display(&b"        "[..spaces]);
                self.column += spaces;
            }
            0..0x20 | 0x7f => {}
            _ => {
                self.push_display(&[byte]);
                // Count characters, not UTF-8 continuation bytes.
                if byte & 0xc0 != 0x80 {
                    self.column += 1;
                }
            }
        }
        None
    }

    fn push_display(&mut self, bytes: &[u8]) {
        if self.display.len() + bytes.len() <= MAX_LINE {
            self.display.extend_from_slice(bytes);
        }
    }

    /// An SGR escape: keep it in the line, and remember it for the next line.
    fn color(&mut self, end: u8) {
        let mut escape = vec![0x1b, b'['];
        escape.extend_from_slice(&self.csi);
        escape.push(end);
        let params = &self.csi;
        if params.is_empty() || params.iter().all(|&b| b == b'0' || b == b';') {
            self.colors.clear();
        } else if self.colors.len() < 256 {
            self.colors.extend_from_slice(&escape);
        }
        self.push_display(&escape);
    }
}

/// The last `capacity` items pushed, and a count of every item ever pushed.
#[derive(Debug)]
pub struct Ring<T> {
    items: VecDeque<T>,
    capacity: usize,
    total: u64,
}

impl<T> Ring<T> {
    /// An empty ring holding at most `capacity` items.
    pub fn new(capacity: usize) -> Self {
        Self { items: VecDeque::new(), capacity: capacity.max(1), total: 0 }
    }

    /// Adds `item`, dropping the oldest one if full.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(item);
        self.total += 1;
    }

    /// Items held now.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Every item held, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.items.iter()
    }

    /// Items ever pushed, including dropped and cleared ones.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Drops everything held (the total keeps counting).
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(bytes: &[u8]) -> Vec<Assembled> {
        Assembler::new().feed(bytes)
    }

    #[test]
    fn splits_lines_and_keeps_raw_bytes() {
        let out = lines(b"one\r\ntwo\nthree");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].raw, "one");
        assert_eq!(out[0].line.plain, "one");
        assert_eq!(out[1].raw, "two");
    }

    #[test]
    fn keeps_colors_and_drops_other_escapes() {
        let out = lines(b"\x1b[32mok\x1b[0m \x1b[2Kdone\x1b]0;title\x07!\x1b(B\n");
        assert_eq!(out[0].line.text, "\x1b[32mok\x1b[0m done!");
        assert_eq!(out[0].line.plain, "ok done!");
        assert_eq!(out[0].raw, "\x1b[32mok\x1b[0m \x1b[2Kdone\x1b]0;title\x07!\x1b(B");
    }

    #[test]
    fn carriage_return_redraws_the_line() {
        let mut assembler = Assembler::new();
        assert!(assembler.feed(b"Building [=>  ] 1/3\rBuilding [==> ] 2/3").is_empty());
        assert_eq!(assembler.partial().unwrap().plain, "Building [==> ] 2/3");
        let out = assembler.feed(b"\r\x1b[K   Finished\n");
        assert_eq!(out[0].line.plain, "   Finished");
        assert!(assembler.partial().is_none());
    }

    #[test]
    fn colors_carry_to_the_next_line_until_reset() {
        let out = lines(b"\x1b[31merror\nstill red\x1b[0m\nplain\n");
        assert_eq!(out[1].line.text, "\x1b[31mstill red\x1b[0m");
        assert_eq!(out[2].line.text, "plain");
    }

    #[test]
    fn expands_tabs_and_handles_split_utf8() {
        let mut assembler = Assembler::new();
        let bytes = "a\tb é\n".as_bytes();
        let mut out = assembler.feed(&bytes[..5]);
        out.extend(assembler.feed(&bytes[5..]));
        assert_eq!(out[0].line.plain, "a       b é");
    }

    #[test]
    fn finish_flushes_the_last_partial_line() {
        let mut assembler = Assembler::new();
        assembler.feed(b"no newline");
        assert_eq!(assembler.finish().unwrap().raw, "no newline");
        assert!(assembler.finish().is_none());
    }

    #[test]
    fn ring_keeps_the_newest() {
        let mut ring = Ring::new(3);
        for i in 0..5 {
            ring.push(i);
        }
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(ring.total(), 5);
        ring.clear();
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.total(), 5);
    }
}
