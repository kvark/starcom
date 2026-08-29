//! Rebuild pane models from tmux captures and observable terminal state.
//!
//! This is not a serialization of tmux's complete input parser. The active SGR
//! pen, saved DEC cursor/charset state and some modes are not exported by stock
//! tmux. See docs/SYNCHRONIZATION.md before treating a restored model as exact.

use std::collections;

use crate::{core, terminal};
use anyhow::Context;

pub const MAX_PANES: usize = 32;
pub const MAX_HISTORY_LINES: usize = 1000;
const MAX_SESSION_CELLS: usize = 1_048_576;
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

// No names or free-form text: every separator has an unambiguous meaning.
pub const STATE_FORMAT: &str = concat!(
    "#{pane_id}|#{window_id}|#{pane_width}|#{pane_height}|",
    "#{pane_left}|#{pane_top}|#{cursor_x}|#{cursor_y}|",
    "#{alternate_on}|#{history_size}|#{history_limit}|",
    "#{alternate_saved_x}|#{alternate_saved_y}|",
    "#{scroll_region_upper}|#{scroll_region_lower}|",
    "#{cursor_flag}|#{insert_flag}|#{keypad_cursor_flag}|#{keypad_flag}|",
    "#{wrap_flag}|#{mouse_standard_flag}|#{mouse_button_flag}|#{mouse_any_flag}|",
    "#{mouse_utf8_flag}|#{mouse_sgr_flag}|#{bracket_paste_flag}|#{pane_tabs}"
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    pub pane: tmuxctl::PaneId,
    pub window: tmuxctl::WindowId,
    pub size: core::Size,
    pub left: usize,
    pub top: usize,
    pub cursor: (usize, usize),
    pub alternate: bool,
    pub history_size: usize,
    pub history_limit: usize,
    saved_cursor: (usize, usize),
    scroll_region: (usize, usize),
    // Cursor, insert, application cursor, application keypad, wrap, mouse
    // standard/button/any/UTF8/SGR, in that order.
    modes: [bool; 10],
    // Older tmux versions do not export this input-side mode.
    bracketed_paste: Option<bool>,
    tabs: Vec<usize>,
}

impl State {
    pub fn parse(line: &str) -> anyhow::Result<Self> {
        let fields: Vec<_> = line.split('|').collect();
        anyhow::ensure!(fields.len() == 27, "incomplete tmux pane state");
        let number = |index: usize| -> anyhow::Result<usize> {
            fields[index]
                .parse()
                .with_context(|| format!("invalid pane state field {index}"))
        };
        let flag = |index: usize| -> anyhow::Result<bool> {
            match fields[index] {
                "0" => Ok(false),
                "1" => Ok(true),
                _ => anyhow::bail!("missing or invalid pane mode {index}"),
            }
        };
        let pane = tmuxctl::PaneId(
            fields[0]
                .strip_prefix('%')
                .context("invalid pane id")?
                .parse()?,
        );
        let window = tmuxctl::WindowId(
            fields[1]
                .strip_prefix('@')
                .context("invalid window id")?
                .parse()?,
        );
        let size = core::Size::new(number(2)?, number(3)?)?;
        let cursor = (number(6)?, number(7)?);
        let alternate = flag(8)?;
        let saved_cursor = if alternate {
            (number(11)?, number(12)?)
        } else {
            // tmux leaves alternate_saved_* empty when no saved grid exists.
            (0, 0)
        };
        for position in [cursor, saved_cursor] {
            anyhow::ensure!(
                position.0 <= size.columns() && position.1 < size.rows(),
                "cursor outside pane"
            );
        }
        let scroll_region = (number(13)?, number(14)?);
        anyhow::ensure!(
            scroll_region.0 <= scroll_region.1 && scroll_region.1 < size.rows(),
            "invalid scroll region"
        );
        let mut modes = [false; 10];
        for (offset, mode) in modes.iter_mut().enumerate() {
            *mode = flag(15 + offset)?;
        }
        let bracketed_paste = if fields[25].is_empty() {
            None
        } else {
            Some(flag(25)?)
        };
        let mut tabs = Vec::new();
        if !fields[26].is_empty() {
            for value in fields[26].split(',') {
                let column: usize = value.parse().context("invalid tab stop")?;
                anyhow::ensure!(column < size.columns(), "tab stop outside pane");
                anyhow::ensure!(
                    tabs.last().is_none_or(|last| *last < column),
                    "unordered tab stops"
                );
                tabs.push(column);
            }
        }
        let left = number(4)?;
        let top = number(5)?;
        anyhow::ensure!(
            left <= 65535 && top <= 65535,
            "pane position exceeds budget"
        );
        Ok(Self {
            pane,
            window,
            size,
            left,
            top,
            cursor,
            alternate,
            history_size: number(9)?,
            history_limit: number(10)?,
            saved_cursor,
            scroll_region,
            modes,
            bracketed_paste,
            tabs,
        })
    }

    /// None means the tmux server did not export this input-side mode.
    pub fn bracketed_paste(&self) -> Option<bool> {
        self.bracketed_paste
    }

    fn restore_modes(&self, terminal: &mut terminal::Terminal) {
        terminal.feed(b"\x1b[0m\x0f\x1b[3g");
        for &column in &self.tabs {
            terminal.feed(format!("\x1b[1;{}H\x1bH", column + 1).as_bytes());
        }
        terminal.feed(
            format!(
                "\x1b[{};{}r",
                self.scroll_region.0 + 1,
                self.scroll_region.1 + 1
            )
            .as_bytes(),
        );
        for (index, prefix) in [
            (0, "?25"),
            (1, "4"),
            (2, "?1"),
            (4, "?7"),
            (5, "?1000"),
            (6, "?1002"),
            (7, "?1003"),
            (8, "?1005"),
            (9, "?1006"),
        ] {
            let suffix = if self.modes[index] { 'h' } else { 'l' };
            terminal.feed(format!("\x1b[{prefix}{suffix}").as_bytes());
        }
        if let Some(enabled) = self.bracketed_paste {
            let suffix = if enabled { 'h' } else { 'l' };
            terminal.feed(format!("\x1b[?2004{suffix}").as_bytes());
        }
        terminal.feed(if self.modes[3] { b"\x1b=" } else { b"\x1b>" });
        terminal.restore_cursor(self.cursor.0, self.cursor.1);
    }
}

/// Check the complete session budget before allocating any replacement grids.
pub fn validate_budget(states: &[State], history: usize) -> anyhow::Result<()> {
    anyhow::ensure!(history <= MAX_HISTORY_LINES, "history exceeds budget");
    anyhow::ensure!(
        !states.is_empty() && states.len() <= MAX_PANES,
        "invalid pane count"
    );
    let mut cells = 0usize;
    let mut ids = collections::BTreeSet::new();
    for state in states {
        anyhow::ensure!(ids.insert(state.pane), "duplicate pane");
        cells = cells
            .checked_add(terminal::Terminal::estimated_cells(state.size, history))
            .context("cell budget overflow")?;
        anyhow::ensure!(
            cells <= MAX_SESSION_CELLS,
            "session exceeds terminal cell budget"
        );
    }
    Ok(())
}

/// A reconstructed, read-only pane. Metadata describes the capture boundary;
/// obtain the current cursor and active screen from `terminal` after live data.
pub struct Pane {
    pub state: State,
    pub terminal: terminal::Terminal,
    pub history_may_be_truncated: bool,
}

impl Pane {
    pub fn restore(
        state: State,
        active: &[String],
        saved: &[String],
        pending: &[String],
        history_lines: usize,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(history_lines <= MAX_HISTORY_LINES, "history exceeds budget");
        let mut terminal = terminal::Terminal::new(state.size, history_lines);
        if state.alternate {
            feed_grid(&mut terminal, saved)?;
            terminal.restore_cursor(state.saved_cursor.0, state.saved_cursor.1);
            terminal.feed(b"\x1b[?1049h");
        } else {
            anyhow::ensure!(
                saved.iter().all(String::is_empty),
                "unexpected saved primary grid"
            );
        }
        feed_grid(&mut terminal, active)?;
        state.restore_modes(&mut terminal);
        // -P encodes newlines; it must be a single logical line. Empty capture
        // responses differ across tmux versions and both forms are accepted.
        anyhow::ensure!(pending.len() <= 1, "multiline pending capture");
        if let Some(bytes) = pending.first() {
            terminal.feed(&decode_escaped(bytes.as_bytes())?);
        }
        let history_may_be_truncated =
            state.alternate || state.history_size > terminal.history_capacity();
        Ok(Self {
            state,
            terminal,
            history_may_be_truncated,
        })
    }
}

fn feed_grid(terminal: &mut terminal::Terminal, lines: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(!lines.is_empty(), "missing screen capture");
    let mut total = 0usize;
    // tmux's capture uses SO/SI for its DEC special-graphics cells.
    terminal.feed(b"\x1b[0m\x0f\x1b)0\x1b[H");
    for (index, line) in lines.iter().enumerate() {
        total = total
            .checked_add(line.len())
            .context("capture size overflow")?;
        anyhow::ensure!(total <= MAX_CAPTURE_BYTES, "screen capture exceeds 1 MiB");
        if index != 0 {
            // -J joins soft wraps. Only hard line breaks remain in the reply.
            terminal.feed(b"\r\n");
        }
        terminal.feed(&decode_escaped(line.as_bytes())?);
    }
    Ok(())
}

/// `capture-pane -C` is not the same escaping as `%output`: screen text uses
/// doubled backslashes, while -P and synthesized controls use octal escapes.
/// Never decode a second time (a literal `\\033` must stay literal).
pub fn decode_escaped(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(bytes.len() <= MAX_CAPTURE_BYTES, "capture exceeds 1 MiB");
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
        } else if bytes.get(index + 1) == Some(&b'\\') {
            decoded.push(b'\\');
            index += 2;
        } else {
            let digits = bytes
                .get(index + 1..index + 4)
                .context("truncated capture escape")?;
            anyhow::ensure!(
                digits[0] <= b'3' && digits.iter().all(|byte| (b'0'..=b'7').contains(byte)),
                "invalid capture escape"
            );
            decoded.push((digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + (digits[2] - b'0'));
            index += 4;
        }
    }
    Ok(decoded)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Watching,
    NeedsResync,
    Disconnected,
}

/// Published models. Invalidated views remain readable; no input API is exposed.
pub struct View {
    pub session: tmuxctl::SessionId,
    panes: collections::BTreeMap<tmuxctl::PaneId, Pane>,
    status: Status,
}

impl View {
    pub fn new(session: tmuxctl::SessionId, panes: Vec<Pane>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !panes.is_empty() && panes.len() <= MAX_PANES,
            "pane count exceeds budget"
        );
        let mut cells = 0usize;
        let mut map = collections::BTreeMap::new();
        for pane in panes {
            // Charge the complete per-pane allocation ceiling, including both
            // visible grids, rather than only the number of nonblank cells.
            cells = cells
                .checked_add(pane.terminal.cell_budget())
                .context("cell budget overflow")?;
            anyhow::ensure!(
                cells <= MAX_SESSION_CELLS,
                "session exceeds terminal cell budget"
            );
            anyhow::ensure!(
                map.insert(pane.state.pane, pane).is_none(),
                "duplicate pane"
            );
        }
        Ok(Self {
            session,
            panes: map,
            status: Status::Watching,
        })
    }

    pub fn panes(&self) -> &collections::BTreeMap<tmuxctl::PaneId, Pane> {
        &self.panes
    }
    pub fn status(&self) -> Status {
        self.status
    }
    pub fn disconnect(&mut self) {
        self.status = Status::Disconnected;
    }

    pub fn apply(&mut self, notification: tmuxctl::Notification) {
        // A detach must remain observable even if a preceding layout change
        // already invalidated the models in this same network read.
        if matches!(notification, tmuxctl::Notification::Exit(_)) {
            self.disconnect();
            return;
        }
        if self.status != Status::Watching {
            return;
        }
        match notification {
            tmuxctl::Notification::Output { pane, bytes }
            | tmuxctl::Notification::ExtendedOutput { pane, bytes, .. } => {
                match self.panes.get_mut(&pane) {
                    Some(terminal) => terminal.terminal.feed(&bytes),
                    None => self.status = Status::NeedsResync,
                }
            }
            tmuxctl::Notification::Exit(_) => self.disconnect(),
            // Unknown mutations, changes to pane geometry, and flow-control
            // gaps require a fresh snapshot. Never guess dimensions or replay
            // queued data into models whose topology may now be wrong.
            tmuxctl::Notification::LayoutChange { .. }
            | tmuxctl::Notification::WindowAdd(_)
            | tmuxctl::Notification::WindowClose(_)
            | tmuxctl::Notification::Pause(_)
            | tmuxctl::Notification::Continue(_)
            | tmuxctl::Notification::SessionChanged(..)
            | tmuxctl::Notification::Unknown(_) => self.status = Status::NeedsResync,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(columns: usize, rows: usize) -> State {
        State::parse(&format!(
            "%1|@2|{columns}|{rows}|0|0|0|0|0|0|2000|||0|{}|1|0|0|0|1|0|0|0|0|0|1|",
            rows - 1
        ))
        .unwrap()
    }
    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn older_tmux_missing_bracketed_paste_is_explicitly_unknown() {
        let original = "%1|@2|12|3|0|0|0|0|0|0|2000|||0|2|1|0|0|0|1|0|0|0|0|0||";
        assert_eq!(State::parse(original).unwrap().bracketed_paste(), None);
        assert_eq!(state(12, 3).bracketed_paste(), Some(true));
    }

    #[test]
    fn captures_decode_once_and_preserve_literal_backslashes() {
        assert_eq!(
            decode_escaped(br"a\\033b\033[31m\134\012").unwrap(),
            b"a\\033b\x1b[31m\\\n"
        );
        assert_eq!(decode_escaped(br"\177\377").unwrap(), [127, 255]);
        assert_eq!(
            decode_escaped("café 界".as_bytes()).unwrap(),
            "café 界".as_bytes()
        );
        for bytes in [br"\".as_slice(), br"\0", br"\400", br"\999", br"\x1b"] {
            assert!(decode_escaped(bytes).is_err());
        }
    }

    #[test]
    fn reconstructs_primary_screen_and_continues_pending_escape() {
        let mut state = state(12, 3);
        state.cursor = (3, 0);
        let mut pane = Pane::restore(
            state,
            &lines(&["old", "", ""]),
            &[],
            &lines(&[r"\033[2"]),
            10,
        )
        .unwrap();
        pane.terminal.feed(b"K\rnew");
        assert_eq!(pane.terminal.screen_lines()[0], "new");
    }

    #[test]
    fn alternate_exit_restores_primary_and_saved_cursor() {
        let mut state = state(12, 3);
        state.alternate = true;
        state.saved_cursor = (4, 0);
        let mut pane = Pane::restore(
            state,
            &lines(&["alternate", "", ""]),
            &lines(&["home", "", ""]),
            &[],
            10,
        )
        .unwrap();
        assert!(pane.terminal.is_alternate_screen());
        assert_eq!(pane.terminal.screen_lines()[0], "alternate");
        pane.terminal.feed(b"\x1b[?1049l!");
        assert!(!pane.terminal.is_alternate_screen());
        assert_eq!(pane.terminal.screen_lines()[0], "home!");
    }

    #[test]
    fn wrap_pending_is_not_lost_at_snapshot_boundary() {
        let mut state = state(4, 2);
        state.cursor = (4, 0);
        let mut pane = Pane::restore(state, &lines(&["abcd", ""]), &[], &[], 0).unwrap();
        pane.terminal.feed(b"E");
        assert_eq!(pane.terminal.screen_lines(), ["abcd", "E"]);
    }

    #[test]
    fn joined_soft_wraps_do_not_add_a_hard_line() {
        let mut state = state(4, 3);
        state.cursor = (2, 1);
        let pane = Pane::restore(state, &lines(&["abcdef", ""]), &[], &[], 0).unwrap();
        assert_eq!(pane.terminal.screen_lines(), ["abcd", "ef", ""]);
    }

    #[test]
    fn layout_change_stops_output_until_resync() {
        let pane = Pane::restore(state(12, 3), &lines(&["before", "", ""]), &[], &[], 0).unwrap();
        let mut view = View::new(tmuxctl::SessionId(0), vec![pane]).unwrap();
        view.apply(tmuxctl::Notification::WindowAdd(tmuxctl::WindowId(9)));
        view.apply(tmuxctl::Notification::Output {
            pane: tmuxctl::PaneId(1),
            bytes: b"bad".to_vec(),
        });
        assert_eq!(view.status(), Status::NeedsResync);
        assert_eq!(
            view.panes()[&tmuxctl::PaneId(1)].terminal.screen_lines()[0],
            "before"
        );
        view.apply(tmuxctl::Notification::Exit(None));
        assert_eq!(view.status(), Status::Disconnected);
    }

    #[test]
    fn custom_tabs_and_scroll_region_survive_restore() {
        let mut state = state(12, 4);
        state.tabs = vec![3];
        state.scroll_region = (1, 2);
        state.cursor = (0, 2);
        let mut pane = Pane::restore(
            state,
            &lines(&["top", "first", "second", "bottom"]),
            &[],
            &[],
            0,
        )
        .unwrap();
        pane.terminal.feed(b"\r\n\tZ");
        assert_eq!(
            pane.terminal.screen_lines(),
            ["top", "second", "\tZ", "bottom"]
        );
        // Text extraction preserves tabs; the cursor checks their actual width.
        assert_eq!(pane.terminal.model().grid().cursor.point.column.0, 4);
    }

    #[test]
    fn allocation_budget_is_checked_before_model_construction() {
        let mut states = Vec::new();
        for id in 0..16 {
            let mut state = state(120, 60);
            state.pane = tmuxctl::PaneId(id);
            states.push(state);
        }
        assert!(validate_budget(&states, 1000).is_err());
        assert!(validate_budget(&states[..1], 1000).is_ok());
    }

    #[test]
    fn state_and_capture_limits_fail_before_unbounded_work() {
        assert!(State::parse("%1|@2").is_err());
        let valid = state(12, 3);
        assert!(Pane::restore(valid.clone(), &[], &[], &[], 0).is_err());
        assert!(Pane::restore(valid, &lines(&["x"]), &[], &[], MAX_HISTORY_LINES + 1).is_err());
        assert!(decode_escaped(&vec![b'x'; MAX_CAPTURE_BYTES + 1]).is_err());
    }
}
