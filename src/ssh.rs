//! Embedded SSH using FileMan's libssh2 stack. No local ssh process or PTY.
//!
//! Connection/authentication are blocking, with libssh2 timeouts. Run this API
//! on a worker, not the future GUI thread. Channel I/O is nonblocking and uses
//! socket readiness, not sleep/poll loops. A connection owns one channel for now.

use std::{fmt, fs, io, net, path, time};

const MAX_KNOWN_HOSTS_BYTES: u64 = 4 * 1024 * 1024;
const AGAIN: ssh2::ErrorCode = ssh2::ErrorCode::Session(-37);

#[derive(Clone, Debug)]
pub enum Authentication {
    Identity(path::PathBuf),
    Agent,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub known_hosts: path::PathBuf,
    pub authentication: Authentication,
    /// Per-operation timeout. OS hostname resolution is not covered by this.
    pub timeout: time::Duration,
}

impl Options {
    pub fn validate(&self) -> Result<(), Error> {
        if self.host.is_empty()
            || self.host.len() > 255
            || !self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-:_".contains(&b))
        {
            return Err(Error::new(
                Kind::Configuration,
                "host must be a DNS name or an unbracketed IP address, not an SSH alias/config fragment",
            ));
        }
        if self.port == 0
            || self.user.is_empty()
            || self.user.len() > 256
            || self
                .user
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(Error::new(Kind::Configuration, "invalid SSH user or port"));
        }
        if self.timeout < time::Duration::from_millis(1)
            || self.timeout > time::Duration::from_secs(300)
        {
            return Err(Error::new(
                Kind::Configuration,
                "SSH timeout must be between 1 ms and 300 s",
            ));
        }
        if self.known_hosts.as_os_str().is_empty() {
            return Err(Error::new(
                Kind::Configuration,
                "an explicit known-hosts file is required",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Configuration,
    Transport,
    UnknownHostKey,
    ChangedHostKey,
    Authentication,
    Timeout,
}

#[derive(Debug)]
pub struct Error {
    pub kind: Kind,
    detail: String,
}

impl Error {
    fn new(kind: Kind, detail: impl fmt::Display) -> Self {
        Self {
            kind,
            detail: detail.to_string(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SSH {:?}: {}", self.kind, self.detail.escape_debug())
    }
}

impl std::error::Error for Error {}

pub struct Connection {
    session: ssh2::Session,
    socket: net::TcpStream,
    poller: polling::Poller,
    events: polling::Events,
    timeout: time::Duration,
    fingerprint: String,
}

impl Connection {
    pub fn connect(options: &Options) -> Result<Self, Error> {
        options.validate()?;
        let file =
            fs::File::open(&options.known_hosts).map_err(|e| Error::new(Kind::Configuration, e))?;
        let mut limited = io::Read::take(file, MAX_KNOWN_HOSTS_BYTES + 1);
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut limited, &mut bytes)
            .map_err(|e| Error::new(Kind::Configuration, e))?;
        if bytes.len() as u64 > MAX_KNOWN_HOSTS_BYTES {
            return Err(Error::new(
                Kind::Configuration,
                "known-hosts file exceeds 4 MiB",
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|e| Error::new(Kind::Configuration, e))?;
        // Reject unsupported trust semantics before networking or authentication.
        validate_known_hosts(text)?;

        // ToSocketAddrs uses the OS resolver; do not claim a DNS deadline here.
        let addresses = net::ToSocketAddrs::to_socket_addrs(&(options.host.as_str(), options.port))
            .map_err(|e| Error::new(Kind::Transport, e))?;
        let deadline = time::Instant::now() + options.timeout;
        let mut socket = None;
        let mut last_error = "hostname resolved to no usable address".to_owned();
        for address in addresses.take(16) {
            let remaining = remaining(deadline)?;
            match net::TcpStream::connect_timeout(&address, remaining) {
                Ok(stream) => {
                    socket = Some(stream);
                    break;
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        let socket = socket.ok_or_else(|| Error::new(Kind::Transport, last_error))?;
        socket
            .set_nodelay(true)
            .map_err(|e| Error::new(Kind::Transport, e))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| Error::new(Kind::Transport, e))?;
        let mut session = ssh2::Session::new().map_err(|e| Error::new(Kind::Transport, e))?;
        session.set_tcp_stream(
            socket
                .try_clone()
                .map_err(|e| Error::new(Kind::Transport, e))?,
        );
        session.set_timeout(options.timeout.as_millis() as u32);
        session
            .handshake()
            .map_err(|e| Error::new(Kind::Transport, e))?;
        let (key, _) = session
            .host_key()
            .ok_or_else(|| Error::new(Kind::Transport, "server supplied no host key"))?;
        let hash = session
            .host_key_hash(ssh2::HashType::Sha256)
            .ok_or_else(|| Error::new(Kind::Transport, "host-key fingerprint unavailable"))?;
        let fingerprint = format!(
            "SHA256:{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD_NO_PAD, hash)
        );
        verify_known_hosts(&session, text, &options.host, options.port, key).map_err(
            |mut error| {
                error.detail.push_str(&format!("; presented {fingerprint}"));
                error
            },
        )?;

        // Never authenticate before host-key verification. No TOFU, passwords in
        // argv, agent forwarding, or fallback to an unintended identity.
        let auth = match options.authentication {
            Authentication::Identity(ref private_key) => {
                session.userauth_pubkey_file(&options.user, None, private_key, None)
            }
            Authentication::Agent => session.userauth_agent(&options.user),
        };
        auth.map_err(|e| Error::new(Kind::Authentication, e))?;
        if !session.authenticated() {
            return Err(Error::new(
                Kind::Authentication,
                "authentication was not completed",
            ));
        }
        session.set_blocking(false);
        let poller = polling::Poller::new().map_err(|e| Error::new(Kind::Transport, e))?;
        // SAFETY: Connection owns the registered socket and deletes it from the
        // poller in Drop before the socket can be closed or reused.
        unsafe { poller.add(&socket, polling::Event::readable(0)) }
            .map_err(|e| Error::new(Kind::Transport, e))?;
        Ok(Self {
            session,
            socket,
            poller,
            events: polling::Events::new(),
            timeout: options.timeout,
            fingerprint,
        })
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Execute a caller-constructed remote command; no interactive shell or PTY
    /// is requested. User data must be shell-quoted by the command builder.
    pub fn exec(mut self, command: &str) -> Result<Channel, Error> {
        let deadline = time::Instant::now() + self.timeout;
        let mut inner = self.retry(deadline, |session| session.channel_session())?;
        self.retry(deadline, |_| inner.exec(command))?;
        Ok(Channel {
            inner,
            connection: self,
        })
    }

    fn retry<T>(
        &mut self,
        deadline: time::Instant,
        mut operation: impl FnMut(&mut ssh2::Session) -> Result<T, ssh2::Error>,
    ) -> Result<T, Error> {
        loop {
            remaining(deadline)?;
            match operation(&mut self.session) {
                Ok(value) => return Ok(value),
                Err(error) if error.code() == AGAIN => self.wait(deadline)?,
                Err(error) => return Err(Error::new(Kind::Transport, error)),
            }
        }
    }

    fn wait(&mut self, deadline: time::Instant) -> Result<(), Error> {
        let interest = match self.session.block_directions() {
            ssh2::BlockDirections::Outbound => polling::Event::writable(0),
            ssh2::BlockDirections::Both => polling::Event::all(0),
            _ => polling::Event::readable(0),
        };
        self.poller
            .modify(&self.socket, interest)
            .map_err(|e| Error::new(Kind::Transport, e))?;
        loop {
            self.events.clear();
            match self
                .poller
                .wait(&mut self.events, Some(remaining(deadline)?))
            {
                Ok(count) if count != 0 => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(Error::new(Kind::Transport, error)),
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.poller.delete(&self.socket);
        let _ = self.socket.shutdown(net::Shutdown::Both);
    }
}

pub struct Channel {
    inner: ssh2::Channel,
    connection: Connection,
}

impl Channel {
    pub fn timeout(&self) -> time::Duration {
        self.connection.timeout
    }
    pub fn fingerprint(&self) -> &str {
        self.connection.fingerprint()
    }
    pub fn eof(&self) -> bool {
        self.inner.eof()
    }
    pub fn wait(&mut self, deadline: time::Instant) -> Result<(), Error> {
        self.connection.wait(deadline)
    }
    pub fn read_stderr(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.inner.stderr(), bytes)
    }
    pub fn abort(&mut self) {
        let _ = self.connection.socket.shutdown(net::Shutdown::Both);
    }
}

impl io::Read for Channel {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.inner, bytes)
    }
}

impl io::Write for Channel {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::Write::write(&mut self.inner, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut self.inner)
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // Closing our SSH transport detaches this client, not the tmux server.
        // Also prevents libssh2 teardown from waiting on a stalled peer.
        self.abort();
    }
}

fn remaining(deadline: time::Instant) -> Result<time::Duration, Error> {
    deadline
        .checked_duration_since(time::Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| Error::new(Kind::Timeout, "operation deadline expired"))
}

fn validate_known_hosts(text: &str) -> Result<(), Error> {
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let hosts = line.split_whitespace().next().unwrap_or("");
        if hosts.starts_with('@') || hosts.contains(['*', '?', '!']) {
            return Err(Error::new(
                Kind::Configuration,
                format!(
                    "known-hosts line {} uses unsupported markers/patterns; refusing to ignore trust policy",
                    index + 1
                ),
            ));
        }
    }
    Ok(())
}

fn verify_known_hosts(
    session: &ssh2::Session,
    text: &str,
    host: &str,
    port: u16,
    key: &[u8],
) -> Result<(), Error> {
    validate_known_hosts(text)?;
    let mut hosts = session
        .known_hosts()
        .map_err(|e| Error::new(Kind::Configuration, e))?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        hosts
            .read_str(line, ssh2::KnownHostFileKind::OpenSSH)
            .map_err(|e| Error::new(Kind::Configuration, e))?;
    }
    // check_port may also consult an unqualified hostname. We intentionally
    // require the exact OpenSSH host:port identity for a non-default port.
    let lookup = if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    };
    match hosts.check(&lookup, key) {
        ssh2::CheckResult::Match => Ok(()),
        ssh2::CheckResult::NotFound => Err(Error::new(
            Kind::UnknownHostKey,
            "host key is not trusted; authenticate it out of band before adding it to known_hosts",
        )),
        ssh2::CheckResult::Mismatch => Err(Error::new(
            Kind::ChangedHostKey,
            "host key differs from known_hosts; refusing authentication",
        )),
        ssh2::CheckResult::Failure => Err(Error::new(
            Kind::Configuration,
            "known-hosts verification failed",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fill: u8) -> Vec<u8> {
        let mut key = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        key.extend_from_slice(&[fill; 32]);
        key
    }

    #[test]
    fn host_keys_are_exact_and_port_scoped() {
        let session = ssh2::Session::new().unwrap();
        let key = key(7);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &key);
        let text = format!("[example.test]:2222 ssh-ed25519 {encoded}\n");
        assert!(verify_known_hosts(&session, &text, "example.test", 2222, &key).is_ok());
        assert_eq!(
            verify_known_hosts(&session, &text, "example.test", 22, &key)
                .unwrap_err()
                .kind,
            Kind::UnknownHostKey
        );
        assert_eq!(
            verify_known_hosts(&session, &text, "example.test", 2223, &key)
                .unwrap_err()
                .kind,
            Kind::UnknownHostKey
        );
        assert_eq!(
            verify_known_hosts(&session, &text, "other.test", 2222, &key)
                .unwrap_err()
                .kind,
            Kind::UnknownHostKey
        );
        let mut changed = key.clone();
        changed[20] ^= 1;
        assert_eq!(
            verify_known_hosts(&session, &text, "example.test", 2222, &changed)
                .unwrap_err()
                .kind,
            Kind::ChangedHostKey
        );
        let bare = format!("example.test ssh-ed25519 {encoded}\n");
        assert_eq!(
            verify_known_hosts(&session, &bare, "example.test", 2222, &key)
                .unwrap_err()
                .kind,
            Kind::UnknownHostKey
        );
    }

    #[test]
    fn unsupported_trust_policy_is_not_ignored() {
        for line in [
            "@revoked host ssh-ed25519 AAAA",
            "@cert-authority host key",
            "*.test key",
            "!bad.test,good.test key",
        ] {
            assert_eq!(
                validate_known_hosts(line).unwrap_err().kind,
                Kind::Configuration
            );
        }
        assert!(validate_known_hosts("# comment\n\n[::1]:2222 ssh-ed25519 AAAA\n").is_ok());
    }

    #[test]
    fn diagnostics_cannot_emit_terminal_controls() {
        let error = Error::new(Kind::Transport, "remote\x1b]52;c;payload\x07\n");
        assert!(!error.to_string().contains('\x1b'));
        assert!(!error.to_string().contains('\n'));
    }
}
