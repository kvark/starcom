use std::fmt;

/// Initial safety budget, not a terminal-protocol limit.
pub const MAX_VISIBLE_CELLS: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Size {
    columns: usize,
    rows: usize,
}

impl Size {
    pub fn new(columns: usize, rows: usize) -> Result<Self, InvalidValue> {
        if columns == 0 || rows == 0 || columns > 4096 || rows > 4096 {
            return Err(InvalidValue::Size);
        }
        if columns * rows > MAX_VISIBLE_CELLS {
            return Err(InvalidValue::Size);
        }
        Ok(Self { columns, rows })
    }

    pub fn columns(self) -> usize {
        self.columns
    }

    pub fn rows(self) -> usize {
        self.rows
    }
}

impl Default for Size {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 24,
        }
    }
}

/// A name, never a shell fragment or a fuzzy tmux target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionName(String);

impl SessionName {
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidValue> {
        let name = name.into();
        if name.is_empty() || name.len() > 1024 || name.chars().any(char::is_control) {
            return Err(InvalidValue::SessionName);
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidValue {
    Size,
    SessionName,
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match *self {
            Self::Size => "terminal dimensions are zero or exceed the cell budget",
            Self::SessionName => "session name is empty, too long, or contains control characters",
        })
    }
}

impl std::error::Error for InvalidValue {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_are_bounded_before_allocation() {
        assert!(Size::new(0, 24).is_err());
        assert!(Size::new(80, 0).is_err());
        assert!(Size::new(usize::MAX, 2).is_err());
        assert!(Size::new(4096, 4096).is_err());
        assert_eq!(Size::new(80, 24).unwrap(), Size::default());
    }

    #[test]
    fn session_names_reject_control_characters() {
        for name in ["", "work\nkill-server", "work\r", "work\0", "work\u{1b}"] {
            assert!(SessionName::new(name).is_err());
        }
        assert!(SessionName::new("a".repeat(1025)).is_err());
        assert!(SessionName::new("work with spaces; $(not-a-command)").is_ok());
    }
}
