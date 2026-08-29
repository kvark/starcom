use alacritty_terminal::{event, grid, index, term, vte};

use crate::core;

const MAX_BUFFER_CELLS: usize = 131_072;
const MAX_HISTORY_LINES: usize = 10_000;

impl grid::Dimensions for core::Size {
    fn columns(&self) -> usize {
        (*self).columns()
    }

    fn screen_lines(&self) -> usize {
        (*self).rows()
    }

    fn total_lines(&self) -> usize {
        (*self).rows()
    }
}

/// A pane model with no local PTY and no renderer.
/// VoidListener intentionally suppresses device replies and side effects during
/// replay. tmux owns application-side terminal responses; never blindly forward
/// Alacritty's PtyWrite/clipboard events into send-keys.
pub struct Terminal {
    model: term::Term<event::VoidListener>,
    parser: vte::ansi::Processor,
    size: core::Size,
    history_limit: usize,
}

impl Terminal {
    pub fn new(size: core::Size, history_lines: usize) -> Self {
        let history_lines = history_lines
            .min(MAX_HISTORY_LINES)
            .min((MAX_BUFFER_CELLS / size.columns()).saturating_sub(size.rows()));
        let config = term::Config {
            scrolling_history: history_lines,
            osc52: term::Osc52::Disabled,
            ..term::Config::default()
        };
        Self {
            model: term::Term::new(config, &size, event::VoidListener),
            parser: vte::ansi::Processor::new(),
            size,
            history_limit: history_lines,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.model, bytes);
    }

    /// Conservative maximum cell allocation for the primary and alternate grids.
    pub fn estimated_cells(size: core::Size, history_lines: usize) -> usize {
        let history = history_lines
            .min(MAX_HISTORY_LINES)
            .min((MAX_BUFFER_CELLS / size.columns()).saturating_sub(size.rows()));
        (history + 2 * size.rows()) * size.columns()
    }

    pub fn history_capacity(&self) -> usize {
        self.history_limit
    }

    pub fn cell_budget(&self) -> usize {
        (self.history_limit + 2 * self.size.rows()) * self.size.columns()
    }

    /// Restore tmux's cursor convention, including x == width (pending wrap).
    /// Called only after State has validated these coordinates.
    pub(crate) fn restore_cursor(&mut self, column: usize, row: usize) {
        debug_assert!(column <= self.size.columns() && row < self.size.rows());
        let cursor = &mut self.model.grid_mut().cursor;
        cursor.point = index::Point::new(
            index::Line(row as i32),
            index::Column(column.min(self.size.columns() - 1)),
        );
        cursor.input_needs_wrap = column == self.size.columns();
    }

    pub fn size(&self) -> core::Size {
        self.size
    }

    pub fn model(&self) -> &term::Term<event::VoidListener> {
        &self.model
    }

    /// Current active screen, not a reconstructed application transcript.
    /// The real renderer should use model().renderable_content(), not this
    /// allocation-heavy diagnostic representation.
    pub fn screen_lines(&self) -> Vec<String> {
        (0..self.size.rows())
            .map(|row| {
                self.model
                    .bounds_to_string(
                        index::Point::new(index::Line(row as i32), index::Column(0)),
                        index::Point::new(
                            index::Line(row as i32),
                            index::Column(self.size.columns() - 1),
                        ),
                    )
                    .trim_end_matches(&[' ', '\r', '\n'][..])
                    .to_owned()
            })
            .collect()
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.model.mode().contains(term::TermMode::ALT_SCREEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carriage_return_and_ansi_are_emulated_not_stripped() {
        let mut terminal = Terminal::new(core::Size::new(20, 4).unwrap(), 0);
        terminal.feed(b"working\r\x1b[2K\x1b[32mdone\x1b[0m");
        assert_eq!(terminal.screen_lines()[0], "done");
    }

    #[test]
    fn alternate_screen_restores_primary_content() {
        let mut terminal = Terminal::new(core::Size::new(20, 4).unwrap(), 0);
        terminal.feed(b"primary\x1b[?1049h\x1b[Halternate");
        assert!(terminal.is_alternate_screen());
        assert_eq!(terminal.screen_lines()[0], "alternate");
        terminal.feed(b"\x1b[?1049l");
        assert!(!terminal.is_alternate_screen());
        assert_eq!(terminal.screen_lines()[0], "primary");
    }

    #[test]
    fn unicode_survives_bytewise_input() {
        let mut terminal = Terminal::new(core::Size::new(20, 4).unwrap(), 0);
        for byte in "café 界 e\u{301}".as_bytes() {
            terminal.feed(std::slice::from_ref(byte));
        }
        assert_eq!(terminal.screen_lines()[0], "café 界 e\u{301}");
    }
}
