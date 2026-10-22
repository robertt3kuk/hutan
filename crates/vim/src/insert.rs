//! Vim's ⌃ keys of Insert mode (`:help ins-special-keys`): `⌃W` `⌃U` `⌃H` delete back, `⌃J`
//! `⌃M` break the line, `⌃T` `⌃D` change the line's indent, `⌃O` runs one Normal command,
//! `⌃R {reg}` inserts a register, `⌃A` the last inserted text, `⌃E` `⌃Y` the character below or
//! above. They are the modal layer's also in a field that types for itself.

use crate::host::{Clipboard, Edit, Transaction, VimHost};
use crate::key::{Key, Mode, Options};
use crate::register::RegisterKind;
use crate::text::{Text, class};
use crate::vim::{Core, Repeat};

/// `⌃O` in Insert mode: Insert comes back after one Normal command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InsertOnce {
    /// The caret was past the end of its line (`A`): it goes back there when the command left
    /// it on the line's last character.
    pub eol: bool,
    pub line: usize,
}

/// What `⌃W` and `⌃U` delete back to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Back {
    Word,
    Line,
}

impl Core {
    /// An Insert-mode ⌃ key. `None` when the key is not one of them; else the text `".`
    /// records for it (`None` inside: nothing).
    pub(crate) fn insert_control<H: VimHost>(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Option<Option<String>> {
        if self.insert_register {
            self.insert_register = false;
            return Some(match key {
                Key::Char(name) => self.insert_register_text(host, name, clip),
                _ => None,
            });
        }
        let Key::Ctrl(c) = key else {
            return None;
        };
        let code = || Some(crate::vim::key_text(key));
        Some(match c {
            'w' => {
                self.delete_back(host, Back::Word);
                code()
            }
            'u' => {
                self.delete_back(host, Back::Line);
                code()
            }
            'h' => {
                self.backspace(host);
                code()
            }
            'j' | 'm' if !opts.single_line => {
                self.type_text(host, "\n");
                Some("\n".to_owned())
            }
            't' | 'd' => {
                self.change_indent(host, c == 'd');
                code()
            }
            'o' => {
                self.insert_once_start(host, opts);
                None
            }
            'r' => {
                self.insert_register = true;
                None
            }
            'a' => match self.regs.last_insert.clone() {
                Some(text) => {
                    self.type_text(host, &text);
                    Some(text)
                }
                None => {
                    self.error("E29: No inserted text yet");
                    None
                }
            },
            'e' | 'y' => match self.char_beside(host, c == 'e') {
                Some(c) => {
                    let text = c.to_string();
                    self.type_text(host, &text);
                    Some(text)
                }
                None => None,
            },
            _ => return None,
        })
    }

    /// Whether `key`, in Insert mode, is the modal layer's in a field that types for itself.
    pub(crate) fn takes_insert_key(&self, key: Key) -> bool {
        self.insert_register || key.is_insert_command()
    }

    /// `⌃W` / `⌃U`: back over a word (blanks first) or the whole line before the caret; at the
    /// start of a line, the line break. Both stop once where the insert started.
    fn delete_back<H: VimHost>(&mut self, host: &mut H, what: Back) {
        let buf = Text::new(&*host);
        let at = host.cursor();
        let ls = buf.line_start(at);
        if at == ls {
            self.backspace(host);
            return;
        }
        let mut from = at;
        match what {
            Back::Line => from = ls,
            Back::Word => {
                while from > ls && buf.before(from).is_some_and(char::is_whitespace) {
                    from = buf.prev(from);
                }
                if let Some(first) = buf.before(from) {
                    let kind = class(first, false);
                    while from > ls && buf.before(from).is_some_and(|c| class(c, false) == kind) {
                        from = buf.prev(from);
                    }
                }
            }
        }
        // Once at the start of the insert (Vim's `'backspace'` `start`).
        if let Some(session) = &mut self.insert {
            if session.origin < at && from < session.origin && session.origin >= ls {
                from = session.origin;
            } else if from < session.origin {
                session.origin = from;
            }
        }
        if from < at {
            self.apply(host, Transaction::delete(from..at, from));
        }
    }

    /// Backspace through the host when it types for itself, else in the layer.
    fn backspace<H: VimHost>(&mut self, host: &mut H) {
        let at = host.cursor();
        if at == 0 {
            return;
        }
        let buf = Text::new(&*host);
        let from = buf.prev(at);
        if let Some(session) = &mut self.insert
            && from < session.origin
        {
            session.origin = from;
        }
        self.apply(host, Transaction::delete(from..at, from));
    }

    /// `⌃T` / `⌃D`: the line's indent one `'shiftwidth'` more or less, rounded to a multiple
    /// of it; the caret stays on its character (in the indent: at the same distance from the
    /// text, or the line's start).
    fn change_indent<H: VimHost>(&mut self, host: &mut H, less: bool) {
        let buf = self.text(&*host);
        let at = host.cursor();
        let ls = buf.line_start(at);
        let le = buf.line_end(at);
        let line = host.slice(ls..le).into_owned();
        let (width, bytes) = self.indent_of(&line);
        let sw = self.settings.shift();
        let new = if less {
            if width % sw != 0 {
                width - width % sw
            } else {
                width.saturating_sub(sw)
            }
        } else {
            width - width % sw + sw
        };
        let indent = self.indent_text(new);
        if indent == line[..bytes] {
            return;
        }
        let cursor = if at >= ls + bytes {
            at - bytes + indent.len()
        } else {
            let vcol = buf.column(at) + new;
            let vcol = vcol.saturating_sub(width);
            if vcol == 0 {
                ls
            } else {
                ls + vcol.min(indent.len())
            }
        };
        if let Some(session) = &mut self.insert
            && session.origin > ls
        {
            session.origin = if session.origin >= ls + bytes {
                session.origin - bytes + indent.len()
            } else {
                ls
            };
        }
        self.apply(
            host,
            Transaction {
                edits: vec![Edit {
                    range: ls..ls + bytes,
                    text: indent,
                }],
                cursor,
            },
        );
    }

    /// `⌃E` (`below`) / `⌃Y`: the character at the caret's column on the next or previous line.
    fn char_beside<H: VimHost>(&mut self, host: &H, below: bool) -> Option<char> {
        let buf = self.text(host);
        let at = host.cursor();
        let line = buf.line_of(at);
        let other = if below {
            (line + 1 < buf.line_count()).then_some(line + 1)?
        } else {
            line.checked_sub(1)?
        };
        let vcol = buf.column(at);
        let ls = buf.line_start_of(other);
        let target = buf.at_vcol(ls, vcol);
        if buf.column(target) != vcol {
            return None;
        }
        buf.at(target).filter(|c| *c != '\n')
    }

    /// `⌃R {name}`: the register's text typed at the caret (a linewise one with its line
    /// break). The text `".` records.
    fn insert_register_text<H: VimHost>(
        &mut self,
        host: &mut H,
        name: char,
        clip: &mut dyn Clipboard,
    ) -> Option<String> {
        let text = match name {
            '+' | '*' => clip.read()?,
            _ => {
                let reg = self.regs.get(name)?;
                if reg.kind == RegisterKind::Line {
                    format!("{}\n", reg.text)
                } else {
                    reg.text
                }
            }
        };
        if text.is_empty() {
            return None;
        }
        self.type_text(host, &text);
        Some(text)
    }

    /// `⌃O`: Insert ends where the caret is (it does not step back) and Normal mode runs one
    /// command; [`insert_once_end`](Self::insert_once_end) comes back.
    fn insert_once_start<H: VimHost>(&mut self, host: &mut H, opts: Options) {
        let buf = Text::new(&*host);
        let at = host.cursor();
        let eol = at == buf.line_end(at) && at > buf.line_start(at);
        let line = buf.line_of(at);
        let col = host.slice(buf.line_start(at)..at).chars().count();
        self.finish_insert(host, opts, false);
        // Normal mode rests on a character (`eol` brings the caret back after the command);
        // `j` and `k` still aim for the column the caret was in.
        self.clamp_normal(host);
        self.want_col = Some(col);
        self.insert_once = Some(crate::insert::InsertOnce { eol, line });
    }

    /// The command after `⌃O` is done: Insert again, past the end of the line when the caret
    /// was there and is still on that line's last character (or went there with `$`).
    pub(crate) fn insert_once_end<H: VimHost + ?Sized>(&mut self, host: &mut H) {
        let Some(once) = self.insert_once.take() else {
            return;
        };
        if self.mode != Mode::Normal {
            return;
        }
        let buf = Text::new(&*host);
        let at = host.cursor();
        let le = buf.line_end(at);
        let on_last = at < le && buf.next(at) == le;
        if on_last
            && ((once.eol && buf.line_of(at) == once.line) || self.want_col == Some(usize::MAX))
        {
            host.set_cursor(le);
        }
        self.enter_insert(host, 1, Repeat::Inline, None);
        // A command that ends Insert again is a new change of its own (`.`).
        self.changed = false;
    }
}
