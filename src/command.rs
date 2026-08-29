use std::fmt;

use crate::core;

/// Small requests keep the control channel responsive. Paste batching is a
/// separate operation; callers must not silently truncate a larger input.
pub const MAX_INPUT_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Split {
    LeftRight,
    TopBottom,
}

/// One complete control-mode command, including its terminating newline.
/// There is intentionally no constructor accepting arbitrary command text.
#[derive(Clone)]
pub struct Command(String);

impl Command {
    pub fn list_panes() -> Self {
        Self(
            concat!(
                "list-panes -s -F ",
                "'#{pane_id} #{window_id} #{pane_width} #{pane_height}'\n"
            )
            .to_owned(),
        )
    }

    pub fn select_pane(pane: tmuxctl::PaneId) -> Self {
        Self(format!("select-pane -t {pane}\n"))
    }

    pub fn split_pane(pane: tmuxctl::PaneId, direction: Split) -> Self {
        let flag = match direction {
            Split::LeftRight => "-h",
            Split::TopBottom => "-v",
        };
        Self(format!("split-window {flag} -t {pane}\n"))
    }

    pub fn resize_pane(pane: tmuxctl::PaneId, size: core::Size) -> Self {
        Self(format!(
            "resize-pane -t {pane} -x {} -y {}\n",
            size.columns(),
            size.rows()
        ))
    }

    /// Encode raw terminal key bytes, not tmux key names or command syntax.
    /// This does not add bracketed-paste markers or implement mouse encoding.
    pub fn send_bytes(pane: tmuxctl::PaneId, bytes: &[u8]) -> Result<Self, InputSizeError> {
        if bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
            return Err(InputSizeError);
        }
        let mut line = format!("send-keys -H -t {pane}");
        for byte in bytes {
            line.push(' ');
            line.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            line.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
        }
        line.push('\n');
        Ok(Self(line))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// POSIX-shell command for a non-PTY SSH exec channel on a Linux host.
/// Exact tmux targeting and shell quoting are separate requirements.
pub fn attach_command(session: &core::SessionName) -> String {
    let quoted_name = session.as_str().replace('\'', "'\"'\"'");
    format!("exec tmux -C attach-session -t '={quoted_name}'")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputSizeError;

impl fmt::Display for InputSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "input must contain 1..={MAX_INPUT_BYTES} bytes")
    }
}

impl std::error::Error for InputSizeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_hex_not_command_syntax() {
        let command = Command::send_bytes(tmuxctl::PaneId(7), b"a;\n\x00\xff").unwrap();
        assert_eq!(command.as_str(), "send-keys -H -t %7 61 3b 0a 00 ff\n");
        assert_eq!(
            command.as_bytes().iter().filter(|&&b| b == b'\n').count(),
            1
        );
    }

    #[test]
    fn input_is_never_silently_truncated() {
        assert!(Command::send_bytes(tmuxctl::PaneId(0), &[]).is_err());
        assert!(Command::send_bytes(tmuxctl::PaneId(0), &[0; MAX_INPUT_BYTES + 1]).is_err());
        assert!(Command::send_bytes(tmuxctl::PaneId(0), &[0; MAX_INPUT_BYTES]).is_ok());
    }

    #[test]
    fn attach_uses_exact_target_and_quotes_shell_metacharacters() {
        let session = core::SessionName::new("work's; $(id)").unwrap();
        assert_eq!(
            attach_command(&session),
            "exec tmux -C attach-session -t '=work'\"'\"'s; $(id)'"
        );
    }

    #[test]
    fn pane_actions_use_typed_targets() {
        let pane = tmuxctl::PaneId(42);
        assert_eq!(Command::select_pane(pane).as_str(), "select-pane -t %42\n");
        assert_eq!(
            Command::split_pane(pane, Split::LeftRight).as_str(),
            "split-window -h -t %42\n"
        );
        assert_eq!(
            Command::resize_pane(pane, core::Size::new(100, 30).unwrap()).as_str(),
            "resize-pane -t %42 -x 100 -y 30\n"
        );
    }
}
