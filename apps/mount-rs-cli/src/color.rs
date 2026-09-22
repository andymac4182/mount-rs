//! Small, dependency-free terminal colour helpers.
//!
//! `NO_COLOR` is the same opt-out used by the upstream CLI. We intentionally
//! do not guess from a tty: callers piping help or watcher output can opt out
//! explicitly, while an interactive terminal gets the useful diagnostics.

use std::fmt::{self, Display, Formatter};

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const RED: u8 = 31;
pub const GREEN: u8 = 32;
pub const YELLOW: u8 = 33;
pub const CYAN: u8 = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    enabled: bool,
}

impl Color {
    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    pub fn from_env() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            Self::disabled()
        } else {
            Self::enabled()
        }
    }

    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    pub fn paint(self, code: u8, text: impl Display) -> Styled {
        Styled {
            enabled: self.enabled,
            code,
            text: text.to_string(),
        }
    }

    pub fn bold(self, text: impl Display) -> Styled {
        self.paint(BOLD, text)
    }

    pub fn dim(self, text: impl Display) -> Styled {
        self.paint(DIM, text)
    }

    pub fn red(self, text: impl Display) -> Styled {
        self.paint(RED, text)
    }

    pub fn green(self, text: impl Display) -> Styled {
        self.paint(GREEN, text)
    }

    pub fn yellow(self, text: impl Display) -> Styled {
        self.paint(YELLOW, text)
    }

    pub fn cyan(self, text: impl Display) -> Styled {
        self.paint(CYAN, text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Styled {
    enabled: bool,
    code: u8,
    text: String,
}

impl Display for Styled {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        if self.enabled {
            write!(formatter, "\u{1b}[{}m{}\u{1b}[0m", self.code, self.text)
        } else {
            formatter.write_str(&self.text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_style_is_plain_text() {
        assert_eq!(Color::disabled().bold("mount-rs").to_string(), "mount-rs");
    }

    #[test]
    fn enabled_style_has_a_single_ansi_span() {
        assert_eq!(
            Color::enabled().green("ok").to_string(),
            "\u{1b}[32mok\u{1b}[0m"
        );
    }
}
