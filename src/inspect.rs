//! Read-only tmux inspection and bounded control-channel requests.
//!
//! `observe` produces independent observations, not an atomic terminal snapshot.
//! The session module coordinates a snapshot-to-live boundary over this channel.
//! Neither path exposes interactive input or marks a connection policy Live.

use std::{collections, io, time};

use crate::{control, core, ssh};
use anyhow::Context;

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

pub(crate) struct Batch {
    pub replies: Vec<Vec<String>>,
    /// The number of completed replies at each notification's wire position.
    pub notifications: Vec<(usize, tmuxctl::Notification)>,
}

pub struct Inspector {
    channel: ssh::Channel,
    control: control::Control,
    stderr: Vec<u8>,
}

impl Inspector {
    pub fn attach(
        options: &ssh::Options,
        session: &core::SessionName,
        socket: Option<&str>,
    ) -> anyhow::Result<Self> {
        let command = attach_command(session, socket)?;
        let channel = ssh::Connection::connect(options)?.exec(&command)?;
        Ok(Self {
            channel,
            control: control::Control::new(control::Limits {
                pending_commands: 256,
                ..control::Limits::default()
            })?,
            stderr: Vec::new(),
        })
    }

    /// Perform bounded, read-only queries. Each request has a deadline and every
    /// failure aborts the control stream. No command is ever automatically retried.
    pub fn observe(&mut self, history_lines: usize) -> anyhow::Result<Observation> {
        anyhow::ensure!(
            history_lines <= MAX_HISTORY_LINES,
            "history request exceeds {MAX_HISTORY_LINES} lines"
        );
        let info = self.request("display-message -p '#{version}|#{session_id}|#{client_control_mode}|#{client_readonly}|#{client_flags}|#{client_tty}'\n")?;
        let (version, session) = parse_info(&info)?;
        let before = self.panes()?;
        let mut captures = Vec::new();
        let mut total = 0usize;
        for pane in &before {
            let command = format!(
                "capture-pane -p -C -t {} -S -{} -E -\n",
                pane.id, history_lines
            );
            let rows = self.request(&command)?;
            for row in &rows {
                total = total
                    .checked_add(row.len())
                    .context("capture size overflow")?;
                anyhow::ensure!(total <= MAX_TRANSFER, "session capture exceeds 8 MiB");
            }
            captures.push(Capture {
                pane: pane.clone(),
                escaped_rows: rows,
            });
        }
        let after = self.panes()?;
        Ok(Observation {
            tmux_version: version,
            session,
            fingerprint: self.channel.fingerprint().to_owned(),
            captures,
            metadata_changed_during_capture: before != after,
        })
    }

    pub(crate) fn panes(&mut self) -> anyhow::Result<Vec<Pane>> {
        let lines = self.request(concat!(
            "list-panes -s -F '",
            "#{pane_id} #{window_id} #{pane_width} #{pane_height} ",
            "#{pane_left} #{pane_top} #{cursor_x} #{cursor_y} ",
            "#{alternate_on} #{history_size} #{history_limit}'\n"
        ))?;
        parse_panes(&lines)
    }

    pub(crate) fn request(&mut self, command: &str) -> anyhow::Result<Vec<String>> {
        let mut batch = self.request_batch(&[command.to_owned()])?;
        Ok(batch.replies.remove(0))
    }

    /// One newline-terminated, synchronous tmux command list. Register every
    /// reply before writing; never retry a partial write or failed command list.
    pub(crate) fn request_batch(&mut self, commands: &[String]) -> anyhow::Result<Batch> {
        let result = self.request_batch_inner(commands);
        if result.is_err() {
            self.abort();
        }
        result.with_context(|| {
            let brief: String = String::from_utf8_lossy(&self.stderr)
                .chars()
                .take(512)
                .collect();
            format!("tmux request failed; remote stderr: {brief:?}")
        })
    }

    fn request_batch_inner(&mut self, commands: &[String]) -> anyhow::Result<Batch> {
        anyhow::ensure!(
            !commands.is_empty() && commands.len() <= 256,
            "invalid command count"
        );
        let mut wire = String::new();
        for command in commands {
            let command = command.strip_suffix('\n').unwrap_or(command);
            anyhow::ensure!(
                !command.is_empty()
                    && command.len() <= 4096
                    && !command.chars().any(|ch| matches!(ch, '\n' | '\r' | '\0')),
                "invalid internal control command"
            );
            if !wire.is_empty() {
                wire.push_str(" ; ");
            }
            wire.push_str(command);
        }
        anyhow::ensure!(wire.len() < 64 * 1024, "command batch exceeds 64 KiB");
        wire.push('\n');
        let deadline = time::Instant::now() + self.channel.timeout();
        let mut ids = Vec::with_capacity(commands.len());
        for _ in commands {
            ids.push(self.control.register_command()?);
        }
        let mut unsent = wire.as_bytes();
        while !unsent.is_empty() {
            anyhow::ensure!(
                time::Instant::now() < deadline,
                "tmux write deadline expired; delivery is uncertain"
            );
            match io::Write::write(&mut self.channel, unsent) {
                Ok(0) => anyhow::bail!("tmux channel closed during write; delivery is uncertain"),
                Ok(count) => unsent = &unsent[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.channel.wait(deadline)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    return Err(error).context("tmux write failed; delivery is uncertain");
                }
            }
        }
        let mut batch = Batch {
            replies: Vec::new(),
            notifications: Vec::new(),
        };
        let mut total = 0usize;
        loop {
            let (events, bytes) = self
                .read_events(deadline)?
                .context("tmux reply deadline expired")?;
            total = total.checked_add(bytes).context("wire budget overflow")?;
            anyhow::ensure!(
                total <= MAX_TRANSFER,
                "tmux batch exceeds 8 MiB wire budget"
            );
            for event in events {
                match event {
                    tmuxctl::Incoming::Reply { id, result } => {
                        anyhow::ensure!(
                            ids.get(batch.replies.len()) == Some(&id),
                            "unexpected command reply"
                        );
                        batch.replies.push(
                            result
                                .map_err(|error| {
                                    anyhow::anyhow!("tmux rejected request: {error:?}")
                                })?
                                .lines,
                        );
                    }
                    tmuxctl::Incoming::Notification(tmuxctl::Notification::Exit(reason)) => {
                        anyhow::bail!("tmux attachment ended ({reason:?})");
                    }
                    tmuxctl::Incoming::Notification(notification) => {
                        batch
                            .notifications
                            .push((batch.replies.len(), notification));
                    }
                }
            }
            if batch.replies.len() == commands.len() {
                return Ok(batch);
            }
        }
    }

    /// Read at most one stdout/stderr chunk. None is an idle deadline, not EOF.
    /// The return value preserves wire order, including output after the last
    /// reply in the same SSH packet. Network chunk boundaries are not barriers.
    fn read_events(
        &mut self,
        deadline: time::Instant,
    ) -> anyhow::Result<Option<(Vec<tmuxctl::Incoming>, usize)>> {
        let mut buffer = [0; 8192];
        loop {
            if time::Instant::now() >= deadline {
                return Ok(None);
            }
            let mut bytes = 0usize;
            match self.channel.read_stderr(&mut buffer) {
                Ok(count) => {
                    bytes += count;
                    anyhow::ensure!(
                        self.stderr.len() + count <= MAX_STDERR,
                        "remote stderr exceeds 64 KiB"
                    );
                    self.stderr.extend_from_slice(&buffer[..count]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error).context("read SSH stderr"),
            }
            let mut events = Vec::new();
            match io::Read::read(&mut self.channel, &mut buffer) {
                Ok(count) => {
                    bytes += count;
                    self.control
                        .feed(&buffer[..count], |event| events.push(event))?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error).context("read SSH stdout"),
            }
            for event in &events {
                if let tmuxctl::Incoming::Notification(tmuxctl::Notification::Unknown(ref text)) =
                    *event
                {
                    anyhow::ensure!(
                        text.starts_with('%'),
                        "unexpected stdout in tmux control stream"
                    );
                }
            }
            if bytes != 0 {
                return Ok(Some((events, bytes)));
            }
            anyhow::ensure!(!self.channel.eof(), "SSH control channel ended");
            if let Err(error) = self.channel.wait(deadline) {
                if error.kind == ssh::Kind::Timeout {
                    return Ok(None);
                }
                return Err(error.into());
            }
        }
    }

    pub(crate) fn poll(
        &mut self,
        deadline: time::Instant,
    ) -> anyhow::Result<Vec<tmuxctl::Notification>> {
        let result = (|| {
            let Some((events, _)) = self.read_events(deadline)? else {
                return Ok(Vec::new());
            };
            let mut notifications = Vec::new();
            for event in events {
                match event {
                    tmuxctl::Incoming::Notification(notification) => {
                        notifications.push(notification)
                    }
                    tmuxctl::Incoming::Reply { .. } => anyhow::bail!("unsolicited command reply"),
                }
            }
            Ok(notifications)
        })();
        if result.is_err() {
            self.abort();
        }
        result
    }

    pub(crate) fn abort(&mut self) {
        let _ = self.control.finish(|_| {});
        self.channel.abort();
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
        anyhow::ensure!(
            !socket.is_empty() && socket.len() <= 4096 && !socket.chars().any(char::is_control),
            "invalid tmux socket path"
        );
        command.push_str(" -S ");
        command.push_str(&shell_quote(socket));
    }
    // -N forbids starting a server. -E leaves session environment untouched.
    // read-only is a UI safety flag, not an authorization sandbox for commands.
    command.push_str(" attach-session -E -f read-only,ignore-size,no-output -t ");
    command.push_str(&shell_quote(&format!("={}", session.as_str())));
    Ok(command)
}

pub(crate) fn parse_info(lines: &[String]) -> anyhow::Result<(String, tmuxctl::SessionId)> {
    anyhow::ensure!(lines.len() == 1, "invalid tmux client metadata");
    let fields: Vec<_> = lines[0].split('|').collect();
    anyhow::ensure!(
        fields.len() == 6 && fields[0].len() <= 64 && !fields[0].is_empty(),
        "tmux does not provide the required client metadata"
    );
    anyhow::ensure!(
        fields[2] == "1" && fields[3] == "1" && fields[5].is_empty(),
        "expected a read-only, non-PTY control client"
    );
    let flags: collections::BTreeSet<_> = fields[4].split(',').collect();
    anyhow::ensure!(
        flags.contains("ignore-size") && flags.contains("no-output"),
        "tmux did not apply the inspection safety flags"
    );
    let session = fields[1]
        .strip_prefix('$')
        .context("invalid tmux session id")?
        .parse()?;
    Ok((fields[0].to_owned(), tmuxctl::SessionId(session)))
}

fn parse_panes(lines: &[String]) -> anyhow::Result<Vec<Pane>> {
    anyhow::ensure!(
        !lines.is_empty() && lines.len() <= MAX_PANES,
        "invalid pane count (maximum {MAX_PANES})"
    );
    let mut seen = collections::BTreeSet::new();
    let mut panes = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split_whitespace().collect();
        anyhow::ensure!(fields.len() == 11, "invalid pane metadata");
        let id = tmuxctl::PaneId(
            fields[0]
                .strip_prefix('%')
                .context("invalid pane id")?
                .parse()?,
        );
        anyhow::ensure!(seen.insert(id), "duplicate pane id");
        let window = tmuxctl::WindowId(
            fields[1]
                .strip_prefix('@')
                .context("invalid window id")?
                .parse()?,
        );
        let size = core::Size::new(fields[2].parse()?, fields[3].parse()?)?;
        let left = fields[4].parse()?;
        let top = fields[5].parse()?;
        let cursor_x = fields[6].parse()?;
        let cursor_y = fields[7].parse()?;
        let alternate_screen = match fields[8] {
            "0" => false,
            "1" => true,
            _ => anyhow::bail!("invalid alternate-screen state"),
        };
        // tmux can leave x == width while a wrap is pending.
        anyhow::ensure!(
            cursor_x <= size.columns() && cursor_y < size.rows(),
            "cursor is outside the pane"
        );
        anyhow::ensure!(
            left <= 65535 && top <= 65535,
            "pane position exceeds budget"
        );
        panes.push(Pane {
            id,
            window,
            size,
            left,
            top,
            cursor_x,
            cursor_y,
            alternate_screen,
            history_size: fields[9].parse()?,
            history_limit: fields[10].parse()?,
        });
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
        assert_eq!(
            attach_command(&session, Some("/tmp/a'b")).unwrap(),
            "exec tmux -N -C -S '/tmp/a'\"'\"'b' attach-session -E -f read-only,ignore-size,no-output -t '=work'\"'\"'s; $(false)'"
        );
        assert!(attach_command(&session, Some("x\nkill-server")).is_err());
    }

    #[test]
    fn client_capabilities_are_checked_not_assumed() {
        assert!(
            parse_info(&["3.3a|$0|1|1|control-mode,read-only,ignore-size,no-output|".to_owned()])
                .is_ok()
        );
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
        for line in [
            "%0 @0 999999 999999 0 0 0 0 0 0 0",
            "%0 @0 80 24 0 0 81 0 0 0 0",
            "%0 @0 80 24 0 0 0 0 2 0 0",
            "pane @0 80 24 0 0 0 0 0 0 0",
        ] {
            assert!(parse_panes(&[line.to_owned()]).is_err());
        }
    }
}
