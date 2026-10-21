//! Vim's registers (`:help registers`): the unnamed one, `"0`–`"9`, `"a`–`"z` (`"A`–`"Z`
//! append), the small delete `"-`, the black hole `"_`, the read-only `".` `":` `"/`, and the
//! system clipboard `"+` / `"*` (through [`Clipboard`](crate::Clipboard)).

/// How a register's text goes back into a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterKind {
    /// Characters (`v`).
    Char,
    /// Whole lines (`V`): the text holds them without the last line's newline.
    Line,
    /// A rectangle (`⌃V`): one line of the text per row, `width` characters wide.
    Block { width: usize },
}

/// What a register holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    pub text: String,
    pub kind: RegisterKind,
}

impl Register {
    pub fn chars(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: RegisterKind::Char,
        }
    }

    pub fn lines(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: RegisterKind::Line,
        }
    }

    pub fn is_linewise(&self) -> bool {
        self.kind == RegisterKind::Line
    }

    /// What the system clipboard gets: lines go with their newline.
    pub(crate) fn clipboard_form(&self) -> String {
        if self.kind == RegisterKind::Line {
            format!("{}\n", self.text)
        } else {
            self.text.clone()
        }
    }

    /// A register read back from the system clipboard: text ending in a newline is lines.
    pub(crate) fn from_clipboard(text: String) -> Self {
        match text.strip_suffix('\n') {
            Some(lines) => Self::lines(lines),
            None => Self::chars(text),
        }
    }

    /// `self` with `more` appended (`"A`): a linewise part makes the whole linewise, and lines
    /// are added as lines; characters to characters join the last line.
    fn append(&mut self, more: Register) {
        let lines = self.kind != RegisterKind::Char || more.kind != RegisterKind::Char;
        if more.kind == RegisterKind::Line {
            self.kind = RegisterKind::Line;
        }
        if lines {
            self.text.push('\n');
        }
        self.text.push_str(&more.text);
        if let (RegisterKind::Block { width }, RegisterKind::Block { width: w }) =
            (self.kind, more.kind)
        {
            self.kind = RegisterKind::Block {
                width: width.max(w),
            };
        }
    }
}

/// Which register the unnamed one stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Unnamed {
    #[default]
    None,
    Numbered(usize),
    Named(usize),
    Small,
}

/// The registers of one Vim state.
#[derive(Debug, Clone, Default)]
pub(crate) struct Registers {
    unnamed: Unnamed,
    numbered: [Option<Register>; 10],
    named: [Option<Register>; 26],
    small: Option<Register>,
    /// `".`: the text typed in the last Insert session.
    pub last_insert: Option<String>,
    /// `":`: the last `:` line run.
    pub last_ex: Option<String>,
    /// `"/`: the last search pattern.
    pub last_search: Option<String>,
}

/// What a yank or delete is, for where Vim puts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Write {
    Yank,
    /// A delete or change. `one`: it goes to `"1` (shifting `"1`–`"8` up): it spans lines,
    /// or uses one of Vim's register-1 motions (`%`, `(`, `)`, `` ` ``, `/`, `?`, `n`, `N`,
    /// `{`, `}`). `small`: it is within one line, so it also goes to `"-`.
    Delete {
        one: bool,
        small: bool,
    },
}

/// The register names a command may give (`"x`).
pub(crate) fn valid_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '"' | '-' | '_' | '+' | '*' | '.' | ':' | '/')
}

impl Registers {
    /// The register `name` (`'"'` the unnamed one); `None` when it is empty or not a stored
    /// register (`"_`, `"+`).
    pub fn get(&self, name: char) -> Option<Register> {
        match name {
            '"' => match self.unnamed {
                Unnamed::None => None,
                Unnamed::Numbered(i) => self.numbered[i].clone(),
                Unnamed::Named(i) => self.named[i].clone(),
                Unnamed::Small => self.small.clone(),
            },
            '0'..='9' => self.numbered[name as usize - '0' as usize].clone(),
            'a'..='z' | 'A'..='Z' => {
                self.named[name.to_ascii_lowercase() as usize - 'a' as usize].clone()
            }
            '-' => self.small.clone(),
            '.' => self.last_insert.clone().map(Register::chars),
            ':' => self.last_ex.clone().map(Register::chars),
            '/' => self.last_search.clone().map(Register::chars),
            _ => None,
        }
    }

    /// The register the unnamed one stands for, borrowed.
    pub fn unnamed_ref(&self) -> Option<&Register> {
        match self.unnamed {
            Unnamed::None => None,
            Unnamed::Numbered(i) => self.numbered[i].as_ref(),
            Unnamed::Named(i) => self.named[i].as_ref(),
            Unnamed::Small => self.small.as_ref(),
        }
    }

    /// Stores what a yank or delete took, Vim's way: a yank goes to `"0` (or the named
    /// register); a delete goes to the named register when one was given (and then to `"1` too),
    /// else to `"1` when it is big and to `"-` when it is within a line. `"_` drops it. The
    /// unnamed register then stands for the last one written.
    pub fn store(&mut self, name: Option<char>, reg: Register, write: Write) {
        if name == Some('_') {
            return;
        }
        if let Some(name) = name.filter(char::is_ascii_alphabetic) {
            let i = name.to_ascii_lowercase() as usize - 'a' as usize;
            match (&mut self.named[i], name.is_ascii_uppercase()) {
                (Some(old), true) => old.append(reg.clone()),
                (slot, _) => *slot = Some(reg.clone()),
            }
            self.unnamed = Unnamed::Named(i);
        }
        let named = name.is_some_and(|n| n.is_ascii_alphabetic());
        match write {
            Write::Yank => {
                if !named {
                    self.numbered[0] = Some(reg);
                    self.unnamed = Unnamed::Numbered(0);
                }
            }
            Write::Delete { one, small } => {
                if one || named {
                    self.numbered[1..].rotate_right(1);
                    self.numbered[1] = Some(reg.clone());
                    if !named {
                        self.unnamed = Unnamed::Numbered(1);
                    }
                }
                if small && !named {
                    self.small = Some(reg);
                    self.unnamed = Unnamed::Small;
                }
            }
        }
    }

    /// Writes a register by name without changing what the unnamed register stands for (what a
    /// macro recorded with `q` does; `q"` records into `"0`).
    pub fn set(&mut self, name: char, reg: Register) {
        match name {
            'a'..='z' => {
                self.named[name as usize - 'a' as usize] = Some(reg);
            }
            'A'..='Z' => {
                let i = name.to_ascii_lowercase() as usize - 'a' as usize;
                match &mut self.named[i] {
                    Some(old) => old.append(reg),
                    slot => *slot = Some(reg),
                }
            }
            '0'..='9' => self.numbered[name as usize - '0' as usize] = Some(reg),
            '"' => self.numbered[0] = Some(reg),
            '-' => self.small = Some(reg),
            _ => {}
        }
    }
}
