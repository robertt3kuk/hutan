//! The scroll commands of Normal and Visual mode (`:help scrolling`) over the host's window
//! ([`VimHost::view`]): `⌃D` / `⌃U` half a window (a count sets the amount, Vim's `'scroll'`),
//! `⌃F` / `⌃B` a page (two lines overlap), `⌃E` / `⌃Y` one line with the caret staying unless
//! the line leaves the window. Half pages and pages put the caret on the first non-blank
//! (`'startofline'`).

use crate::command::Scroll;
use crate::host::VimHost;
use crate::text::Text;
use crate::vim::Core;

impl Core {
    pub(crate) fn scroll<H: VimHost + ?Sized>(
        &mut self,
        host: &mut H,
        how: Scroll,
        count: Option<usize>,
    ) {
        let Some((top, height)) = host.view() else {
            return;
        };
        let height = height.max(1);
        let buf = Text::new(&*host);
        let last = buf.line_count().saturating_sub(1);
        let line = buf.line_of(host.cursor());
        let n = count.unwrap_or(1).max(1);
        let (new_top, new_line, first_nonblank) = match how {
            Scroll::HalfDown | Scroll::HalfUp => {
                if let Some(count) = count {
                    self.scroll_lines = Some(count);
                }
                let amount = self.scroll_lines.unwrap_or(height / 2).max(1);
                if how == Scroll::HalfDown {
                    if line >= last {
                        self.fail();
                        return;
                    }
                    let bottom_top = (last + 1).saturating_sub(height);
                    (
                        (top + amount).min(bottom_top.max(top)),
                        (line + amount).min(last),
                        true,
                    )
                } else {
                    if line == 0 {
                        self.fail();
                        return;
                    }
                    (
                        top.saturating_sub(amount),
                        line.saturating_sub(amount),
                        true,
                    )
                }
            }
            Scroll::PageDown => {
                if top >= last {
                    self.fail();
                    return;
                }
                let new_top = (top + n * height.saturating_sub(2).max(1)).min(last);
                (new_top, new_top, true)
            }
            Scroll::PageUp => {
                if top == 0 {
                    self.fail();
                    return;
                }
                let new_top = top.saturating_sub(n * height.saturating_sub(2).max(1));
                // The caret goes to the window's last line, as `⌃F` puts it on the first.
                (new_top, (new_top + height - 1).min(last), true)
            }
            Scroll::LineDown => {
                let new_top = (top + n).min(last);
                (new_top, line.max(new_top), false)
            }
            Scroll::LineUp => {
                let new_top = top.saturating_sub(n);
                (new_top, line.min(new_top + height - 1), false)
            }
        };
        let to = (new_line != line).then(|| {
            let ls = buf.line_start_of(new_line);
            if first_nonblank {
                self.want_col = None;
                buf.first_nonblank(ls)
            } else {
                self.column_on(host, ls)
            }
        });
        host.scroll_to(new_top);
        if let Some(to) = to {
            host.set_cursor(to);
        }
    }

    /// Where the column `j` and `k` aim for falls on the line that starts at `ls`.
    fn column_on<H: VimHost + ?Sized>(&mut self, host: &H, ls: usize) -> usize {
        let buf = Text::new(host);
        let at = host.cursor();
        let want = *self
            .want_col
            .get_or_insert_with(|| host.slice(buf.line_start(at)..at).chars().count());
        let le = buf.line_end(ls);
        let to = buf.advance(ls, want, le);
        buf.normal_clamp(to)
    }
}
