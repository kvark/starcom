//! Short-lived SFTP uploads on their own SSH connection.
//!
//! File drops use this instead of the tmux control channel: the control worker
//! stays attached, and a refused or slow transfer cannot stall the session.
//! The protocol is Sunset's sans-io SFTP client driven from the same polling
//! loop as exec.

use std::{fs, io, path, time};

use anyhow::Context;

use crate::ssh;

const MAX_FILES: usize = 8;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_NAME: usize = 255;

/// Bytes written so far for the file currently being uploaded.
pub struct Progress<'a> {
    pub name: &'a str,
    pub done: u64,
    pub total: u64,
}

/// Upload local files into the remote temp directory (`realpath("/tmp")`)
/// under unique `starcom-…` names, and return those absolute paths in the
/// same order. `on_progress` is called as each write lands.
pub fn put_files(
    options: &ssh::Options,
    files: &[path::PathBuf],
    mut on_progress: impl FnMut(Progress<'_>),
) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(!files.is_empty(), "no files to upload");
    anyhow::ensure!(
        files.len() <= MAX_FILES,
        "drop at most {MAX_FILES} files at a time"
    );
    let stamp = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros())
        .unwrap_or(0);
    let mut prepared = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let name = remote_file_name(file).with_context(|| format!("{}", file.display()))?;
        let remote_name = unique_remote_name(&name, stamp, index)?;
        let meta = fs::metadata(file).with_context(|| format!("stat {}", file.display()))?;
        anyhow::ensure!(!meta.is_dir(), "{} is a directory; drop files only", name);
        anyhow::ensure!(
            meta.len() <= MAX_FILE_BYTES,
            "{name} is larger than {} MiB",
            MAX_FILE_BYTES / (1024 * 1024)
        );
        prepared.push((file.clone(), remote_name, name, meta.len()));
    }

    let mut options = options.clone();
    options.timeout = options.timeout.max(time::Duration::from_secs(30));
    let deadline = time::Instant::now() + time::Duration::from_secs(300);
    let channel = ssh::Connection::connect(&options)?.subsystem("sftp")?;
    let mut session = Session::new(channel, deadline);

    session
        .sftp
        .init()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    session.send(&[])?;
    match session.event()? {
        Event::Version => {}
        other => anyhow::bail!("SFTP handshake: unexpected {other}"),
    }

    session
        .sftp
        .realpath("/tmp")
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    session.send(&[])?;
    let dir = session.realpath()?;
    anyhow::ensure!(
        !dir.is_empty() && dir.len() <= 1024 && !dir.contains('\0'),
        "SFTP temp path is unusable"
    );

    let mut remote = Vec::new();
    for (file, dest_name, name, total) in prepared {
        let dest = join_remote(&dir, &dest_name);
        anyhow::ensure!(
            dest.len() <= 1024,
            "{name}: remote path exceeds SFTP path limit"
        );
        on_progress(Progress {
            name: &name,
            done: 0,
            total,
        });
        session.put(&file, &dest, |done| {
            on_progress(Progress {
                name: &name,
                done,
                total,
            });
        })?;
        remote.push(dest);
    }
    Ok(remote)
}

/// File name used on the host. Rejects path separators so a drop cannot
/// choose a remote directory.
pub(crate) fn remote_file_name(path: &path::Path) -> anyhow::Result<String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("file name is missing or not UTF-8")?;
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= MAX_NAME
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\\')
            && !name.chars().any(char::is_control),
        "file name {name:?} cannot be uploaded"
    );
    Ok(name.to_owned())
}

fn unique_remote_name(name: &str, stamp: u128, index: usize) -> anyhow::Result<String> {
    let prefix = format!("starcom-{stamp}-{index}-");
    anyhow::ensure!(
        prefix.len() < MAX_NAME,
        "file name {name:?} cannot be uploaded"
    );
    let mut rest = name.to_owned();
    while prefix.len() + rest.len() > MAX_NAME {
        rest.pop();
    }
    anyhow::ensure!(!rest.is_empty(), "file name {name:?} cannot be uploaded");
    Ok(format!("{prefix}{rest}"))
}

fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn blocked(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

enum Event {
    Version,
    Handle(Vec<u8>),
    Status(sunset_sftp::protocol::StatusCode),
    NameStart(u32),
    Name(Vec<u8>),
    NameEnd,
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version => f.write_str("version"),
            Self::Handle(_) => f.write_str("handle"),
            Self::Status(code) => write!(f, "status {code}"),
            Self::NameStart(_) => f.write_str("name-start"),
            Self::Name(_) => f.write_str("name"),
            Self::NameEnd => f.write_str("name-end"),
        }
    }
}

struct Session {
    channel: ssh::Channel,
    sftp: sunset_sftp::client::SftpRunner<
        { sunset_sftp::client::DEFAULT_CLIENT_BUF },
        { sunset_sftp::client::DEFAULT_CLIENT_BUF },
    >,
    leftover: Vec<u8>,
    leftover_at: usize,
    deadline: time::Instant,
}

impl Session {
    fn new(channel: ssh::Channel, deadline: time::Instant) -> Self {
        Self {
            channel,
            sftp: sunset_sftp::client::SftpRunner::new(),
            leftover: Vec::new(),
            leftover_at: 0,
            deadline,
        }
    }

    fn remaining(&self) -> anyhow::Result<time::Duration> {
        self.deadline
            .checked_duration_since(time::Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| anyhow::anyhow!("SFTP transfer exceeded its deadline"))
    }

    fn send(&mut self, mut data: &[u8]) -> anyhow::Result<()> {
        loop {
            self.remaining()?;
            let mut progressed = false;
            while !self.sftp.output_buf().is_empty() {
                match io::Write::write(&mut self.channel, self.sftp.output_buf()) {
                    Ok(0) => anyhow::bail!("SFTP channel closed during write"),
                    Ok(count) => {
                        self.sftp.consume_output(count);
                        progressed = true;
                    }
                    Err(error) if blocked(&error) => break,
                    Err(error) => return Err(error).context("write SFTP request"),
                }
            }
            if let Some(len) = self.sftp.send_data() {
                anyhow::ensure!(
                    data.len() >= len,
                    "SFTP write payload is shorter than the request"
                );
                match io::Write::write(&mut self.channel, &data[..len]) {
                    Ok(0) => anyhow::bail!("SFTP channel closed during write"),
                    Ok(count) => {
                        self.sftp.data_sent(count);
                        data = &data[count..];
                        progressed = true;
                    }
                    Err(error) if blocked(&error) => {}
                    Err(error) => return Err(error).context("write SFTP payload"),
                }
            }
            if self.sftp.output_done() {
                match io::Write::flush(&mut self.channel) {
                    Ok(()) => return Ok(()),
                    Err(error) if blocked(&error) => {}
                    Err(error) => return Err(error).context("flush SFTP"),
                }
            }
            if !progressed {
                self.channel
                    .wait(self.deadline)
                    .context("wait to send SFTP")?;
            }
        }
    }

    fn event(&mut self) -> anyhow::Result<Event> {
        let mut incoming = [0; 8192];
        while !self.sftp.has_event() {
            self.remaining()?;
            if self.leftover_at < self.leftover.len() {
                let used = self
                    .sftp
                    .input(&self.leftover[self.leftover_at..])
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                self.leftover_at += used;
                if self.leftover_at == self.leftover.len() {
                    self.leftover.clear();
                    self.leftover_at = 0;
                }
                if used == 0 {
                    anyhow::bail!("SFTP parser stalled on leftover input");
                }
                continue;
            }
            match io::Read::read(&mut self.channel, &mut incoming) {
                Ok(0) => anyhow::bail!("SFTP channel ended"),
                Ok(count) => {
                    let mut used = 0;
                    while used < count && !self.sftp.has_event() {
                        let step = self
                            .sftp
                            .input(&incoming[used..count])
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                        if step == 0 {
                            break;
                        }
                        used += step;
                    }
                    if used < count {
                        self.leftover.extend_from_slice(&incoming[used..count]);
                    }
                }
                Err(error) if blocked(&error) => {
                    self.channel
                        .wait(self.deadline)
                        .context("wait to read SFTP")?;
                }
                Err(error) => return Err(error).context("read SFTP"),
            }
        }
        Ok(
            match self
                .sftp
                .event()
                .ok_or_else(|| anyhow::anyhow!("SFTP event disappeared"))?
            {
                sunset_sftp::client::SftpEvent::Version { .. } => Event::Version,
                sunset_sftp::client::SftpEvent::Handle { handle, .. } => {
                    Event::Handle(handle.to_vec())
                }
                sunset_sftp::client::SftpEvent::Status { code, .. } => Event::Status(code),
                sunset_sftp::client::SftpEvent::NameStart { count, .. } => Event::NameStart(count),
                sunset_sftp::client::SftpEvent::Name { filename, .. } => {
                    Event::Name(filename.to_vec())
                }
                sunset_sftp::client::SftpEvent::NameEnd { .. } => Event::NameEnd,
                other => anyhow::bail!("unexpected SFTP event {other:?}"),
            },
        )
    }

    fn realpath(&mut self) -> anyhow::Result<String> {
        match self.event()? {
            Event::NameStart(count) if count >= 1 => {}
            Event::Status(code) => anyhow::bail!("SFTP realpath failed: {code}"),
            other => anyhow::bail!("SFTP realpath: unexpected {other}"),
        }
        let name = match self.event()? {
            Event::Name(bytes) => String::from_utf8(bytes).context("SFTP realpath is not UTF-8")?,
            other => anyhow::bail!("SFTP realpath: unexpected {other}"),
        };
        match self.event()? {
            Event::NameEnd => Ok(name),
            other => anyhow::bail!("SFTP realpath: unexpected {other}"),
        }
    }

    fn put(
        &mut self,
        local: &path::Path,
        remote: &str,
        mut on_progress: impl FnMut(u64),
    ) -> anyhow::Result<()> {
        use sunset_sftp::client::pflags;
        use sunset_sftp::protocol::Attrs;

        self.sftp
            .open(
                remote,
                pflags::WRITE | pflags::CREAT | pflags::TRUNC,
                &Attrs::default(),
            )
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        self.send(&[])?;
        let handle = match self.event()? {
            Event::Handle(handle) => handle,
            Event::Status(code) => anyhow::bail!("SFTP open {remote}: {code}"),
            other => anyhow::bail!("SFTP open {remote}: unexpected {other}"),
        };

        let mut file =
            fs::File::open(local).with_context(|| format!("open {}", local.display()))?;
        let mut buffer = vec![0; sunset_sftp::client::MAX_WRITE_LEN as usize];
        let mut offset = 0u64;
        loop {
            let count = io::Read::read(&mut file, &mut buffer)
                .with_context(|| format!("read {}", local.display()))?;
            if count == 0 {
                break;
            }
            self.sftp
                .write(&handle, offset, count)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            self.send(&buffer[..count])?;
            match self.event()? {
                Event::Status(sunset_sftp::protocol::StatusCode::SSH_FX_OK) => {}
                Event::Status(code) => anyhow::bail!("SFTP write {remote}: {code}"),
                other => anyhow::bail!("SFTP write {remote}: unexpected {other}"),
            }
            offset += count as u64;
            on_progress(offset);
        }

        self.sftp
            .close(&handle)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        self.send(&[])?;
        match self.event()? {
            Event::Status(sunset_sftp::protocol::StatusCode::SSH_FX_OK) => Ok(()),
            Event::Status(code) => anyhow::bail!("SFTP close {remote}: {code}"),
            other => anyhow::bail!("SFTP close {remote}: unexpected {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_reject_path_separators_and_controls() {
        assert_eq!(
            remote_file_name(path::Path::new("/tmp/notes.txt")).unwrap(),
            "notes.txt"
        );
        assert_eq!(
            remote_file_name(path::Path::new("notes.txt")).unwrap(),
            "notes.txt"
        );
        assert!(remote_file_name(path::Path::new(".")).is_err());
        assert!(remote_file_name(path::Path::new("..")).is_err());
        assert!(remote_file_name(path::Path::new("")).is_err());
    }

    #[test]
    fn remote_join_does_not_double_a_trailing_slash() {
        assert_eq!(join_remote("/home/alice", "f"), "/home/alice/f");
        assert_eq!(join_remote("/home/alice/", "f"), "/home/alice/f");
        assert_eq!(join_remote("/tmp", "f"), "/tmp/f");
    }

    #[test]
    fn unique_names_keep_the_original_and_stay_short() {
        let name = unique_remote_name("notes.txt", 1_700_000_000_000_000, 0).unwrap();
        assert_eq!(name, "starcom-1700000000000000-0-notes.txt");
        let long = unique_remote_name(&"a".repeat(MAX_NAME), 0, 0).unwrap();
        assert!(long.len() <= MAX_NAME);
        assert!(long.starts_with("starcom-0-0-"));
        assert!(long.ends_with('a'));
    }
}
