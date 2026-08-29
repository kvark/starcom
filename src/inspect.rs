//! Read-only live tmux inspection. This is not terminal restoration.
//!
//! Captures are observations made at different times. They must not be appended
//! to an emulator, or used as a reason to mark a connection Live. Snapshot/live
//! ordering, mode restoration and interactive input are a subsequent milestone.

use std::{collections, io, time};

use anyhow::Context;
use crate::{control, core, ssh};

const MAX_PANES: usize = 128;
const MAX_STDERR: usize = 64 * 1024;
const MAX_TRANSFER: usize = 8 * 1024 * 1024;
pub const MAX_HISTORY_LINES: usize = 1000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pane {
    pub id: tmuxctl::PaneId,
    pub window: tmuxctl::WindowId,
    pub size: core::Size,
    pub left: usize,
    pub top: usize,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub alternate_screen: bool,
    pub history_size: usize,
    pub history_limit: usize,
}

#[derive(Debug)]
pub struct Capture {
    pub pane: Pane,
    /// Plain text in tmux's -C escaped representation, not VT playback bytes.
    pub escaped_rows: Vec<String>,
}

#[derive(Debug)]
pub struct Observation {
    pub tmux_version: String,
    pub session: tmuxctl::SessionId,
    pub fingerprint: String,
    pub captures: Vec<Capture>,
    /// A useful warning, NOT proof of atomicity when false. Output may change
    /// without changing topology/cursor/history metadata.
    pub metadata_changed_during_capture: bool,
}

pub struct Inspector {
    channel: ssh::Channel,
    control: control::Control,
    stderr: Vec<u8>,
}

impl Inspector {
    pub fn attach(options: &ssh::Options, session: &core::SessionName,
        socket: Option<&str>) -> anyhow::Result<Self>
    {
        let command = attach_command(session, socket)?;
        let channel = ssh::Connection::connect(options)?.exec(&command)?;
        Ok(Self { channel, control: control::Control::default(), stderr: Vec::new() })
    }

    /// Perform bounded, read-only queries. Each request has a deadline and every
    /// failure aborts the control stream. No command is ever automatically retried.
    pub fn observe(&mut self, history_lines: usize) -> anyhow::Result<Observation> {
        anyhow::ensure!(history_lines <= MAX_HISTORY_LINES, "history request exceeds {MAX_HISTORY_LINES} lines");
        let info = self.request("display-message -p '#{version}|#{session_id}|#{client_control_mode}|#{client_readonly}|#{client_flags}|#{client_tty}'\n")?;
        let (version, session) = parse_info(&info)?;
        let before = self.panes()?;
        let mut captures = Vec::new();
        let mut total = 0usize;
        for pane in &before {
            let command = format!("capture-pane -p -C -t {} -S -{} -E -\n", pane.id, history_lines);
            let rows = self.request(&command)?;
            for row in &rows {
                total = total.checked_add(row.len()).context("capture size overflow")?;
                anyhow::ensure!(total <= MAX_TRANSFER, "session capture exceeds 8 MiB");
            }
            captures.push(Capture { pane: pane.clone(), escaped_rows: rows });
        }
        let after = self.panes()?;
        Ok(Observation {
            tmux_version: version, session,
            fingerprint: self.channel.fingerprint().to_owned(), captures,
            metadata_changed_during_capture: before != after,
        })
    }

    fn panes(&mut self) -> anyhow::Result<Vec<Pane>> {
        let lines = self.request(concat!(
            "list-panes -s -F '",
            "#{pane_id} #{window_id} #{pane_width} #{pane_height} ",
            "#{pane_left} #{pane_top} #{cursor_x} #{cursor_y} ",
            "#{alternate_on} #{history_size} #{history_limit}'\n"))?;
        parse_panes(&lines)
    }

    fn request(&mut self, command: &str) -> anyhow::Result<Vec<String>> {
        let result = self.request_inner(command);
        if result.is_err() {
            let _ = self.control.finish(|_| {});
            self.channel.abort();
        }
        result.with_context(|| {
            // stderr is never mixed into the control stream or emitted raw.
            let diagnostic = String::from_utf8_lossy(&self.stderr);
            let brief: String = diagnostic.chars().take(512).collect();
            format!("tmux request failed; remote stderr: {:?}", brief)
        })
    }

    fn request_inner(&mut self, command: &str) -> anyhow::Result<Vec<String>> {
        anyhow::ensure!(command.len() <= 4096 && command.ends_with('\n')
            && command.bytes().filter(|&b| b == b'\n').count() == 1,
            "invalid internal control command");
        let deadline = time::Instant::now() + self.channel.timeout();
        let id = self.control.register_command()?;
        let mut unsent = command.as_bytes();
        while !unsent.is_empty() {
            anyhow::ensure!(time::Instant::now() < deadline, "tmux write deadline expired; delivery is uncertain");
            match io::Write::write(&mut self.channel, unsent) {
                Ok(0) => anyhow::bail!("tmux channel closed during write; delivery is uncertain"),
                Ok(count) => unsent = &unsent[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.channel.wait(deadline)?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
                Err(error) => return Err(error).context("tmux write failed; delivery is uncertain"),
            }
        }
        let mut buffer = [0; 8192];
        let mut total = 0usize;
        loop {
            anyhow::ensure!(time::Instant::now() < deadline, "tmux reply deadline expired");
            let mut progress = false;
            // Drain stderr as well as stdout: a noisy login script must not fill
            // the shared SSH channel window and deadlock command replies.
            match self.channel.read_stderr(&mut buffer) {
                Ok(count) => {
                    total += count;
                    progress |= count != 0;
                    anyhow::ensure!(self.stderr.len() + count <= MAX_STDERR, "remote stderr exceeds 64 KiB");
                    self.stderr.extend_from_slice(&buffer[..count]);
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
                Err(error) => return Err(error).context("read SSH stderr"),
            }
            let mut reply = None;
            let mut exit = None;
            let mut unexpected_text = false;
            match io::Read::read(&mut self.channel, &mut buffer) {
                Ok(count) => {
                    total += count;
                    progress |= count != 0;
                    self.control.feed(&buffer[..count], |incoming| match incoming {
                        tmuxctl::Incoming::Reply { id: received, result } => {
                            reply = Some(if received == id {
                                result.map(|output| output.lines)
                                    .map_err(|error| anyhow::anyhow!("tmux rejected request: {error:?}"))
                            } else {
                                Err(anyhow::anyhow!("unexpected command reply"))
                            });
                        }
                        tmuxctl::Incoming::Notification(tmuxctl::Notification::Exit(reason)) => {
                            exit = Some(reason);
                        }
                        tmuxctl::Incoming::Notification(tmuxctl::Notification::Unknown(text)) => {
                            unexpected_text |= !text.starts_with('%');
                        }
                        _ => {},
                    })?;
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
                Err(error) => return Err(error).context("read SSH stdout"),
            }
            anyhow::ensure!(total <= MAX_TRANSFER, "tmux response exceeds 8 MiB wire budget");
            anyhow::ensure!(!unexpected_text, "unexpected stdout before/in the tmux control stream; check noninteractive shell startup output");
            if let Some(reason) = exit {
                anyhow::bail!("tmux attachment ended ({reason:?}); the requested session may not exist");
            }
            if let Some(result) = reply { return result; }
            if self.channel.eof() {
                let _ = self.control.finish(|_| {});
                anyhow::bail!("SSH exec channel ended before a tmux reply; check server, session, and tmux availability");
            }
            if !progress { self.channel.wait(deadline)?; }
        }
    }
}

impl Drop for Inspector {
    fn drop(&mut self) {
        let _ = self.control.finish(|_| {});
        self.channel.abort();
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn attach_command(session: &core::SessionName, socket: Option<&str>) -> anyhow::Result<String> {
    let mut command = "exec tmux -N -C".to_owned();
    if let Some(socket) = socket {
        anyhow::ensure!(!socket.is_empty() && socket.len() <= 4096
            && !socket.chars().any(char::is_control), "invalid tmux socket path");
        command.push_str(" -S ");
        command.push_str(&shell_quote(socket));
    }
    // -N forbids starting a server. -E leaves session environment untouched.
    // read-only is a UI safety flag, not an authorization sandbox for commands.
    command.push_str(" attach-session -E -f read-only,ignore-size,no-output -t ");
    command.push_str(&shell_quote(&format!("={}", session.as_str())));
    Ok(command)
}

fn parse_info(lines: &[String]) -> anyhow::Result<(String, tmuxctl::SessionId)> {
    anyhow::ensure!(lines.len() == 1, "invalid tmux client metadata");
    let fields: Vec<_> = lines[0].split('|').collect();
    anyhow::ensure!(fields.len() == 6 && fields[0].len() <= 64 && !fields[0].is_empty(),
        "tmux does not provide the required client metadata");
    anyhow::ensure!(fields[2] == "1" && fields[3] == "1" && fields[5].is_empty(),
        "expected a read-only, non-PTY control client");
    let flags: collections::BTreeSet<_> = fields[4].split(',').collect();
    anyhow::ensure!(flags.contains("ignore-size") && flags.contains("no-output"),
        "tmux did not apply the inspection safety flags");
    let session = fields[1].strip_prefix('$').context("invalid tmux session id")?.parse()?;
    Ok((fields[0].to_owned(), tmuxctl::SessionId(session)))
}

fn parse_panes(lines: &[String]) -> anyhow::Result<Vec<Pane>> {
    anyhow::ensure!(!lines.is_empty() && lines.len() <= MAX_PANES, "invalid pane count (maximum {MAX_PANES})");
    let mut seen = collections::BTreeSet::new();
    let mut panes = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split_whitespace().collect();
        anyhow::ensure!(fields.len() == 11, "invalid pane metadata");
        let id = tmuxctl::PaneId(fields[0].strip_prefix('%').context("invalid pane id")?.parse()?);
        anyhow::ensure!(seen.insert(id), "duplicate pane id");
        let window = tmuxctl::WindowId(fields[1].strip_prefix('@').context("invalid window id")?.parse()?);
        let size = core::Size::new(fields[2].parse()?, fields[3].parse()?)?;
        let left = fields[4].parse()?;
        let top = fields[5].parse()?;
        let cursor_x = fields[6].parse()?;
        let cursor_y = fields[7].parse()?;
        let alternate_screen = match fields[8] {
            "0" => false, "1" => true, _ => anyhow::bail!("invalid alternate-screen state"),
        };
        // tmux can leave x == width while a wrap is pending.
        anyhow::ensure!(cursor_x <= size.columns() && cursor_y < size.rows(), "cursor is outside the pane");
        anyhow::ensure!(left <= 65535 && top <= 65535, "pane position exceeds budget");
        panes.push(Pane { id, window, size, left, top, cursor_x, cursor_y,
            alternate_screen, history_size: fields[9].parse()?, history_limit: fields[10].parse()? });
    }
    panes.sort_by_key(|pane| pane.id);
    Ok(panes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_does_not_create_resize_or_change_environment() {
        let session = core::SessionName::new("work's; $(false)").unwrap();
        assert_eq!(attach_command(&session, Some("/tmp/a'b")).unwrap(),
            "exec tmux -N -C -S '/tmp/a'\"'\"'b' attach-session -E -f read-only,ignore-size,no-output -t '=work'\"'\"'s; $(false)'");
        assert!(attach_command(&session, Some("x\nkill-server")).is_err());
    }

    #[test]
    fn client_capabilities_are_checked_not_assumed() {
        assert!(parse_info(&["3.3a|$0|1|1|control-mode,read-only,ignore-size,no-output|".to_owned()]).is_ok());
        assert!(parse_info(&["3.3a|$0|1|0|ignore-size,no-output|".to_owned()]).is_err());
        assert!(parse_info(&["3.3a|$0|1|1|ignore-size|".to_owned()]).is_err());
        assert!(parse_info(&["3.3a|$0|1|1|ignore-size,no-output|/dev/pts/0".to_owned()]).is_err());
    }

    #[test]
    fn metadata_is_validated_before_allocation() {
        let valid = "%0 @1 80 24 0 0 80 23 1 0 2000".to_owned();
        let panes = parse_panes(std::slice::from_ref(&valid)).unwrap();
        assert!(panes[0].alternate_screen);
        assert_eq!(panes[0].size, core::Size::default());
        assert!(parse_panes(&[valid.clone(), valid]).is_err());
        for line in ["%0 @0 999999 999999 0 0 0 0 0 0 0", "%0 @0 80 24 0 0 81 0 0 0 0", "%0 @0 80 24 0 0 0 0 2 0 0", "pane @0 80 24 0 0 0 0 0 0 0"] {
            assert!(parse_panes(&[line.to_owned()]).is_err());
        }
    }
}
