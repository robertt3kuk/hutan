//! `⌃A` / `⌃X` (`:help CTRL-A`): add to or subtract from the number at or after the caret, with
//! Vim's default `'nrformats'` (`bin,octal,hex`): `0x1f` and `0b101` keep their base, width and
//! the case of their letters, a number with a leading `0` and only octal digits is octal, a
//! decimal one is negative after a `-`. In Visual mode each line's first number in the
//! selection changes; `g⌃A` / `g⌃X` add one more on each line that has a number.

use crate::host::{Edit, Transaction, VimHost};
use crate::key::{Key, Mode};
use crate::text::Text;
use crate::vim::Core;

/// A number found on a line: its bytes (a `-` included) and its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Found {
    start: usize,
    end: usize,
    base: u32,
    negative: bool,
}

/// The first number of `line` that ends after byte `from` and starts before `until` (scanning
/// from `scan`): hex, binary, octal or decimal, as Vim reads it.
fn find(line: &str, scan: usize, from: usize, until: usize) -> Option<Found> {
    let bytes = line.as_bytes();
    let mut i = scan;
    while i < until.min(bytes.len()) {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let digits = |start: usize, ok: fn(u8) -> bool| {
            let mut end = start;
            while end < bytes.len() && ok(bytes[end]) {
                end += 1;
            }
            end
        };
        let prefixed = |marks: &[u8], ok: fn(u8) -> bool| {
            (bytes[i] == b'0'
                && bytes.get(i + 1).is_some_and(|b| marks.contains(b))
                && bytes.get(i + 2).copied().is_some_and(ok))
            .then(|| digits(i + 2, ok))
        };
        let (end, base) = if let Some(end) = prefixed(b"xX", |b| b.is_ascii_hexdigit()) {
            (end, 16)
        } else if let Some(end) = prefixed(b"bB", |b| b == b'0' || b == b'1') {
            (end, 2)
        } else {
            let end = digits(i, |b| b.is_ascii_digit());
            let octal = bytes[i] == b'0'
                && end - i > 1
                && bytes[i..end].iter().all(|b| (b'0'..=b'7').contains(b));
            (end, if octal { 8 } else { 10 })
        };
        if end > from {
            // In Visual mode a `-` before the selection is not part of the number.
            let negative = base == 10 && i > scan && bytes[i - 1] == b'-';
            return Some(Found {
                start: if negative { i - 1 } else { i },
                end,
                base,
                negative,
            });
        }
        i = end;
    }
    None
}

/// The number `text` (`found` in it) after adding `delta`, written like it.
fn changed(text: &str, found: Found, delta: i128) -> String {
    let body = &text[found.start..found.end];
    match found.base {
        10 => {
            let digits = body.trim_start_matches('-');
            let value: i128 = digits.parse::<i128>().unwrap_or(i128::MAX);
            let value = if found.negative { -value } else { value };
            value.saturating_add(delta).to_string()
        }
        base => {
            let (prefix, digits) = if base == 8 {
                ("", body)
            } else {
                body.split_at(2)
            };
            let value = u64::from_str_radix(digits, base).unwrap_or(u64::MAX);
            // Unsigned: below zero wraps around, as in Vim.
            let value = (i128::from(value) + delta).rem_euclid(1 << 64) as u64;
            let upper = digits
                .chars()
                .rev()
                .find(char::is_ascii_alphabetic)
                .is_some_and(|c| c.is_ascii_uppercase());
            let mut out = match base {
                16 if upper => format!("{value:X}"),
                16 => format!("{value:x}"),
                2 => format!("{value:b}"),
                _ => format!("{value:o}"),
            };
            if out.len() < digits.len() {
                out = format!("{}{out}", "0".repeat(digits.len() - out.len()));
            }
            format!("{prefix}{out}")
        }
    }
}

impl Core {
    /// `⌃A` / `⌃X` in Normal mode: `count` added to the number at or after the caret, the
    /// caret on its last character.
    pub(crate) fn increment<H: VimHost + ?Sized>(&mut self, host: &mut H, add: bool, count: usize) {
        let buf = Text::new(&*host);
        let at = host.cursor();
        let ls = buf.line_start(at);
        let le = buf.line_end(at);
        let line = host.slice(ls..le).into_owned();
        let col = at - ls;
        let Some(found) = find(&line, 0, col, line.len()) else {
            self.fail();
            return;
        };
        let delta = if add { count as i128 } else { -(count as i128) };
        let new = changed(&line, found, delta);
        let start = ls + found.start;
        let cursor = start + new.len() - 1;
        self.apply(
            host,
            Transaction::replace(start..ls + found.end, new, cursor),
        );
        self.changed = true;
    }

    /// `⌃A` / `⌃X` (`g⌃A` / `g⌃X` with `progressive`) on the Visual selection; Visual ends with
    /// the caret at its start, and `.` repeats it on as much text from the caret.
    pub(crate) fn visual_increment<H: VimHost + ?Sized>(
        &mut self,
        host: &mut H,
        add: bool,
        progressive: bool,
        count: usize,
    ) {
        let mode = self.mode;
        let ranges = self.visual_ranges(host);
        let (top, bottom) = self.visual_lines(host);
        let block = (mode == Mode::VisualBlock).then(|| self.block(host));
        let width = ranges
            .first()
            .map_or(1, |r| host.slice(r.clone()).chars().count().max(1));
        let start = ranges
            .first()
            .map_or(host.cursor(), |r| r.start)
            .min(host.len());
        self.leave_visual(host);
        let buf = Text::new(&*host);
        let mut edits = Vec::new();
        let mut step = 0;
        for line in top..=bottom.min(buf.line_count().saturating_sub(1)) {
            let ls = buf.line_start_of(line);
            let le = buf.line_end(ls);
            let text = host.slice(ls..le).into_owned();
            let (from, until) = match mode {
                Mode::VisualLine => (0, text.len()),
                Mode::VisualBlock => match block.as_ref().and_then(|b| b.row(&buf, line)) {
                    Some(row) => (row.start - ls, row.end - ls),
                    None => continue,
                },
                _ => {
                    let from = if line == top { start - ls } else { 0 };
                    let until = ranges
                        .first()
                        .filter(|_| line == bottom)
                        .map_or(text.len(), |r| r.end.min(le) - ls);
                    (from, until)
                }
            };
            let Some(found) = find(&text[..until.min(text.len())], from, from, until) else {
                continue;
            };
            step += 1;
            let amount = if progressive { count * step } else { count } as i128;
            let delta = if add { amount } else { -amount };
            edits.push(Edit {
                range: ls + found.start..ls + found.end,
                text: changed(&text, found, delta),
            });
        }
        let cursor = Text::new(&*host).normal_clamp(start);
        if edits.is_empty() {
            host.set_cursor(cursor);
            self.fail();
            return;
        }
        // Edits before the caret do not move it: they all start on or after its line's start
        // and the first one starts at or after it.
        self.apply(host, Transaction { edits, cursor });
        self.changed = true;
        // `.` does the same to as many lines (and characters) from the caret.
        let mut keys = vec![Key::Char(match mode {
            Mode::VisualLine => 'V',
            Mode::VisualBlock => {
                return self.repeat_keys_block(bottom - top, width, add, progressive, count);
            }
            _ => 'v',
        })];
        if bottom > top {
            keys.extend((bottom - top).to_string().chars().map(Key::Char));
            keys.push(Key::Char('j'));
        } else if mode == Mode::Visual && width > 1 {
            keys.extend((width - 1).to_string().chars().map(Key::Char));
            keys.push(Key::Char('l'));
        }
        self.rec = Self::increment_keys(keys, add, progressive, count);
    }

    fn repeat_keys_block(
        &mut self,
        lines: usize,
        width: usize,
        add: bool,
        progressive: bool,
        count: usize,
    ) {
        let mut keys = vec![Key::Ctrl('v')];
        if lines > 0 {
            keys.extend(lines.to_string().chars().map(Key::Char));
            keys.push(Key::Char('j'));
        }
        if width > 1 {
            keys.extend((width - 1).to_string().chars().map(Key::Char));
            keys.push(Key::Char('l'));
        }
        self.rec = Self::increment_keys(keys, add, progressive, count);
    }

    /// The Visual keys `selection` followed by the increment command itself.
    fn increment_keys(mut keys: Vec<Key>, add: bool, progressive: bool, count: usize) -> Vec<Key> {
        if count > 1 {
            keys.extend(count.to_string().chars().map(Key::Char));
        }
        if progressive {
            keys.push(Key::Char('g'));
        }
        keys.push(Key::Ctrl(if add { 'a' } else { 'x' }));
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump(text: &str, col: usize, delta: i128) -> Option<String> {
        let found = find(text, 0, col, text.len())?;
        Some(format!(
            "{}{}{}",
            &text[..found.start],
            changed(text, found, delta),
            &text[found.end..]
        ))
    }

    #[test]
    fn numbers_keep_their_base_width_and_case() {
        assert_eq!(bump("x 7 y", 0, 1).as_deref(), Some("x 8 y"));
        assert_eq!(bump("a -5 b", 0, 1).as_deref(), Some("a -4 b"));
        assert_eq!(bump("-1", 0, 1).as_deref(), Some("0"));
        assert_eq!(bump("1", 0, -2).as_deref(), Some("-1"));
        assert_eq!(bump("0xFF", 0, 1).as_deref(), Some("0x100"));
        assert_eq!(bump("0x00ff", 0, 1).as_deref(), Some("0x0100"));
        assert_eq!(bump("0x00", 0, -1).as_deref(), Some("0xffffffffffffffff"));
        assert_eq!(bump("007", 0, 1).as_deref(), Some("010"));
        assert_eq!(bump("09", 0, 1).as_deref(), Some("10"));
        assert_eq!(bump("0b101", 0, 1).as_deref(), Some("0b110"));
        assert_eq!(bump("12 abc", 4, 1), None);
    }
}
