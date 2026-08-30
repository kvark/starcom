use std::fmt;

use crate::input;

/// Small requests keep the control channel responsive. Paste batching is a
/// separate operation; callers must not silently truncate a larger input.
pub const MAX_INPUT_BYTES: usize = 1024;

/// One complete control-mode command, including its terminating newline.
/// There is intentionally no constructor accepting arbitrary command text.
#[derive(Clone)]
pub struct Command(String);

impl Command {
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

    pub fn send_key(
        pane: tmuxctl::PaneId,
        key: input::Key,
        modifiers: input::Modifiers,
    ) -> Result<Self, input::Error> {
        Ok(Self(format!(
            "send-keys -t {pane} {}\n",
            key.name(modifiers)?
        )))
    }

    /// One axis only. tmux retains ownership of the window's total dimensions.
    pub fn resize_axis(pane: tmuxctl::PaneId, resize: input::Resize) -> Result<Self, input::Error> {
        input::Action::Resize(resize).validate()?;
        let axis = match resize.axis {
            input::Axis::Columns => "-x",
            input::Axis::Rows => "-y",
        };
        Ok(Self(format!(
            "resize-pane -t {pane} {axis} {}\n",
            resize.cells
        )))
    }

    /// A single synchronous command list creates a private named buffer, appends
    /// bounded octal-quoted chunks, and pastes/deletes it. Never -w: the remote
    /// clipboard and the user's automatically named paste buffers are untouched.
    pub(crate) fn paste(pane: tmuxctl::PaneId, paste: &input::Paste, buffer: &str) -> Vec<Self> {
        assert!(!buffer.is_empty() && buffer.len() <= 128);
        assert!(
            buffer
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        );
        let mut commands = Vec::new();
        for (index, chunk) in paste.as_str().as_bytes().chunks(768).enumerate() {
            let append = if index == 0 { "" } else { "-a " };
            let mut line = format!("set-buffer {append}-b {buffer} \"");
            for &byte in chunk {
                line.push('\\');
                line.push(char::from(b'0' + (byte >> 6)));
                line.push(char::from(b'0' + (byte >> 3 & 7)));
                line.push(char::from(b'0' + (byte & 7)));
            }
            line.push_str("\"\n");
            commands.push(Self(line));
        }
        commands.push(Self(format!("paste-buffer -d -p -b {buffer} -t {pane}\n")));
        commands
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Quote one value for the remote POSIX shell that runs the exec command.
/// Rejects control characters outright rather than trying to encode them.
pub(crate) fn shell_quote(value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control),
        "value cannot be placed in a remote command"
    );
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
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
    fn shell_quoting_neutralizes_metacharacters_and_rejects_controls() {
        assert_eq!(
            shell_quote("work's; $(id)").unwrap(),
            "'work'\"'\"'s; $(id)'"
        );
        assert_eq!(shell_quote("/tmp/a b").unwrap(), "'/tmp/a b'");
        for bad in ["", "a\nb", "a\0b", "a\u{1b}b"] {
            assert!(shell_quote(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn input_is_never_silently_truncated() {
        assert!(Command::send_bytes(tmuxctl::PaneId(0), &[]).is_err());
        assert!(Command::send_bytes(tmuxctl::PaneId(0), &[0; MAX_INPUT_BYTES + 1]).is_err());
        assert!(Command::send_bytes(tmuxctl::PaneId(0), &[0; MAX_INPUT_BYTES]).is_ok());
    }
}
