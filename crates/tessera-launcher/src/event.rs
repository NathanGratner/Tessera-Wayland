//! Input events, kept independent of any front end so the app code is shared
//! between the terminal (crossterm) and Wayland (SCTK) versions.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
    /// True when the key came from auto-repeat rather than a fresh press.
    ///
    /// Repeats are useful for scrolling a list and deleting text, and harmful
    /// for anything that changes screen: holding Enter half a second would
    /// otherwise dismiss a dialog *and* activate whatever was underneath it.
    pub repeat: bool,
}

impl Key {
    #[cfg(test)]
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            mods: Mods::default(),
            repeat: false,
        }
    }

    /// The same key, as auto-repeat would deliver it.
    #[cfg(test)]
    pub fn repeated(self) -> Self {
        Self {
            repeat: true,
            ..self
        }
    }

    /// Whether this key should still act when it arrives from auto-repeat.
    pub fn acts_on_repeat(&self) -> bool {
        !self.repeat
            || matches!(
                self.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Backspace
                    | KeyCode::Delete
            )
    }

    #[cfg(test)]
    pub fn char(ch: char) -> Self {
        Self::new(KeyCode::Char(ch))
    }

    /// The character typed, if this key produces one and no Ctrl/Alt is held.
    pub fn typed(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(ch) if !self.mods.ctrl && !self.mods.alt => Some(ch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Mouse {
        col: u16,
        row: u16,
        kind: MouseKind,
    },
    Resize(u16, u16),
    /// The front end is shutting down (window closed, terminal hung up).
    Closed,
}
