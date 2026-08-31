//! Embedded SSH with Sunset and RustCrypto; no libssh2, OpenSSL, or SSH subprocess.
//!
//! Run connection setup on a worker. One nonblocking socket owns one exec channel;
//! bounded queues preserve stdout/stderr separation and apply backpressure. The
//! system resolver and local file/agent connection setup can still block.

use std::{collections, fmt, io, net, path, sync, time};

mod agent;
mod auth;
mod trust;

const MAX_QUEUED_STDOUT: usize = 1024 * 1024;
const MAX_QUEUED_STDERR: usize = 64 * 1024;
const DRIVE_BUDGET: usize = 512;
/// A jump chain longer than this is refused rather than traversed. Every hop is
/// a full SSH connection with its own key exchange and its own deadline, so the
/// cost is real and an unbounded chain is a way to make a connect hang.
pub(crate) const MAX_JUMPS: usize = 4;

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
    /// Jump hosts to traverse, in order, before reaching `host`. Empty is a
    /// direct connection. Each hop verifies its own host key against its own
    /// known-hosts file and authenticates with its own identity: a jump host is
    /// never allowed to vouch for the next one.
    pub jumps: Vec<Options>,
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
        if self.jumps.len() > MAX_JUMPS {
            return Err(Error::new(
                Kind::Configuration,
                "too many jump hosts to traverse",
            ));
        }
        for jump in &self.jumps {
            jump.validate()?;
            // The chain is one flat list, so it cannot be made to nest itself
            // into a cycle. Config resolution refuses a nested ProxyJump for
            // the same reason rather than flattening it silently.
            if !jump.jumps.is_empty() {
                return Err(Error::new(
                    Kind::Configuration,
                    "a jump host may not have jump hosts of its own",
                ));
            }
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

    /// Report a transport fault raised outside this module, so reconnection
    /// policy classifies it from a type rather than from remote text.
    pub(crate) fn transport(detail: impl fmt::Display) -> Self {
        Self::new(Kind::Transport, detail)
    }

    /// Build any failure kind without a socket, so reconnection policy can be
    /// tested against all of them offline.
    #[cfg(test)]
    pub(crate) fn for_test(kind: Kind, detail: impl fmt::Display) -> Self {
        Self::new(kind, detail)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SSH {:?}: {}", self.kind, self.detail.escape_debug())
    }
}

impl std::error::Error for Error {}

/// Wake a socket readiness wait when the UI enqueues input or cancels a session.
/// This carries no credentials and cannot read or write the SSH channel.
#[cfg(feature = "gui")]
#[derive(Clone)]
pub(crate) struct Wake {
    poller: sync::Arc<polling::Poller>,
    pending: sync::Arc<sync::atomic::AtomicBool>,
}

#[cfg(feature = "gui")]
impl Wake {
    pub(crate) fn notify(&self) {
        self.pending.store(true, sync::atomic::Ordering::Release);
        let _ = self.poller.notify();
    }
}

/// Whether a local SSH agent looks reachable, without connecting to it.
///
/// The connection form uses this to say so before a connection is attempted,
/// rather than letting the user find out from a failed authentication. It is a
/// hint, never a decision: Starcom still authenticates exactly as configured.
pub fn agent_available() -> bool {
    agent::available()
}

/// What a connection's SSH stream runs over.
///
/// A jump host's forwarding channel is a byte stream with the same nonblocking
/// contract as a socket, which is the whole of what `ProxyJump` needs. The hop
/// below owns its own connection and, through it, the one real socket and the
/// poller every hop in the chain waits on.
enum Transport {
    Tcp(net::TcpStream),
    Forward(Box<Channel>),
}

impl Transport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => io::Read::read(socket, buffer),
            Self::Forward(channel) => io::Read::read(channel.as_mut(), buffer),
        }
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => io::Write::write(socket, bytes),
            Self::Forward(channel) => io::Write::write(channel.as_mut(), bytes),
        }
    }

    /// Stop carrying data, in whichever way this transport can. Aborting a
    /// forward walks down the chain to the socket underneath it.
    fn shutdown(&mut self) {
        match self {
            Self::Tcp(socket) => {
                let _ = socket.shutdown(net::Shutdown::Both);
            }
            Self::Forward(channel) => channel.abort(),
        }
    }
}

pub struct Connection {
    runner: sunset::Runner<'static, sunset::Client>,
    transport: Transport,
    poller: sync::Arc<polling::Poller>,
    wake_pending: sync::Arc<sync::atomic::AtomicBool>,
    events: polling::Events,
    options: Options,
    trust: trust::Store,
    credentials: Option<auth::Credentials>,
    verified: bool,
    authenticated: bool,
    fingerprint: String,
    handle: Option<sunset::ChanHandle>,
    command: Option<String>,
    started: bool,
    incoming: Vec<u8>,
    incoming_offset: usize,
    stdout: collections::VecDeque<u8>,
    stderr: collections::VecDeque<u8>,
    closed: bool,
}

impl Connection {
    /// Connect to `options.host`, traversing `options.jumps` first when there
    /// are any. Every hop is an ordinary connection whose transport happens not
    /// to be a socket, so nothing about trust or authentication is special-cased
    /// for a bastion: each one verifies its own host key and authenticates for
    /// itself, and the deadline is per hop.
    pub fn connect(options: &Options) -> Result<Self, Error> {
        options.validate()?;
        let Some((first, rest)) = options.jumps.split_first() else {
            return Self::connect_direct(options);
        };
        let mut hop = Self::connect_direct(first)?;
        for next in rest {
            hop = Self::connect_through(hop.open_forward(&next.host, next.port)?, next)?;
        }
        Self::connect_through(hop.open_forward(&options.host, options.port)?, options)
    }

    fn connect_direct(options: &Options) -> Result<Self, Error> {
        options.validate()?;
        let trust = trust::Store::load(&options.known_hosts)?;
        // The OS resolver isn't cancellable through ToSocketAddrs. Connection
        // and authentication deadlines start after it returns; never run on UI.
        let addresses = net::ToSocketAddrs::to_socket_addrs(&(options.host.as_str(), options.port))
            .map_err(transport)?;
        let deadline = time::Instant::now() + options.timeout;
        let mut socket = None;
        let mut last_error = "hostname resolved to no usable address".to_owned();
        for address in addresses.take(16) {
            match net::TcpStream::connect_timeout(&address, remaining(deadline)?) {
                Ok(stream) => {
                    socket = Some(stream);
                    break;
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        let socket = socket.ok_or_else(|| transport(last_error))?;
        configure_stream(&socket)?;
        let poller = sync::Arc::new(polling::Poller::new().map_err(transport)?);
        // SAFETY: Connection owns this socket and deregisters it before drop.
        unsafe { poller.add(&socket, polling::Event::readable(0)) }.map_err(transport)?;
        Self::establish(
            Transport::Tcp(socket),
            poller,
            sync::Arc::new(sync::atomic::AtomicBool::new(false)),
            options,
            trust,
            deadline,
        )
    }

    /// Run a connection over an already-open forwarding channel from the hop
    /// below. The poller and the wake flag are the ones that hop registered, so
    /// waiting anywhere in the chain waits on the single real socket, and one
    /// notification from the UI wakes all of it.
    fn connect_through(carrier: Channel, options: &Options) -> Result<Self, Error> {
        options.validate()?;
        let trust = trust::Store::load(&options.known_hosts)?;
        let deadline = time::Instant::now() + options.timeout;
        let poller = sync::Arc::clone(&carrier.connection.poller);
        let wake_pending = sync::Arc::clone(&carrier.connection.wake_pending);
        Self::establish(
            Transport::Forward(Box::new(carrier)),
            poller,
            wake_pending,
            options,
            trust,
            deadline,
        )
    }

    fn establish(
        stream: Transport,
        poller: sync::Arc<polling::Poller>,
        wake_pending: sync::Arc<sync::atomic::AtomicBool>,
        options: &Options,
        trust: trust::Store,
        deadline: time::Instant,
    ) -> Result<Self, Error> {
        let mut connection = Self {
            runner: sunset::Runner::new_client_owned(),
            transport: stream,
            poller,
            wake_pending,
            events: polling::Events::new(),
            options: options.clone(),
            trust,
            credentials: None,
            verified: false,
            authenticated: false,
            fingerprint: String::new(),
            handle: None,
            command: None,
            started: false,
            incoming: Vec::new(),
            incoming_offset: 0,
            stdout: collections::VecDeque::new(),
            stderr: collections::VecDeque::new(),
            closed: false,
        };
        while !connection.authenticated {
            remaining(deadline)?;
            let progressed = connection.drive(deadline)?;
            if connection.closed {
                return Err(transport("connection ended before authentication"));
            }
            if !progressed {
                connection.wait_ready(deadline)?;
            }
        }
        // Signing secrets and the agent socket are not needed once authenticated.
        connection.credentials = None;
        Ok(connection)
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Execute a caller-quoted command without a PTY, environment requests,
    /// forwarded agent, shell wrapper, or repeated SSH invocation.
    pub fn exec(mut self, command: &str) -> Result<Channel, Error> {
        if command.is_empty() || command.len() > 8192 || command.contains('\0') {
            return Err(Error::new(
                Kind::Configuration,
                "invalid remote exec command",
            ));
        }
        let deadline = time::Instant::now() + self.options.timeout;
        self.command = Some(command.to_owned());
        self.handle = Some(self.runner.open_client_session().map_err(protocol)?);
        while !self.started {
            remaining(deadline)?;
            let progressed = self.drive(deadline)?;
            if self.closed {
                return Err(transport("connection ended opening exec channel"));
            }
            if !progressed {
                self.wait_ready(deadline)?;
            }
        }
        Ok(Channel { connection: self })
    }

    /// Open a `direct-tcpip` channel: ask the server to connect to
    /// `address:port` on our behalf, and carry that TCP stream on the channel.
    ///
    /// This is the channel behind `ssh -J`. It needs no PTY, no shell, and no
    /// remote helper — the server does the connecting, exactly as OpenSSH's own
    /// jump host does. The host key of this hop is verified before the channel
    /// is opened, like every other connection.
    pub fn open_forward(mut self, address: &str, port: u16) -> Result<Channel, Error> {
        if address.is_empty() || address.len() > 255 || port == 0 {
            return Err(Error::new(
                Kind::Configuration,
                "invalid forwarding destination",
            ));
        }
        let deadline = time::Instant::now() + self.options.timeout;
        let handle = self
            .runner
            // The origin is informational and commonly logged by the server.
            // Report a loopback origin rather than anything about this host.
            .open_client_tcpip(address, port, "127.0.0.1", 0)
            .map_err(protocol)?;
        self.handle = Some(handle);
        // A channel that is still opening is not valid for writing, so this is
        // the confirmation signal; there is no session event for a tcp channel.
        while !self.forward_ready() {
            remaining(deadline)?;
            let progressed = self.drive(deadline)?;
            if self.closed {
                return Err(transport("connection ended opening the forward"));
            }
            // A refused open leaves the channel finished rather than removing
            // it, which is how "never" is told apart from "not yet" instead of
            // waiting out the deadline.
            if self.forward_finished() {
                return Err(transport(
                    "the server refused to open the forward; it may not allow \
                     TCP forwarding, or the destination refused the connection",
                ));
            }
            if !progressed {
                self.wait_ready(deadline)?;
            }
        }
        // Nothing was executed, so no exec reply is outstanding.
        self.started = true;
        Ok(Channel { connection: self })
    }

    fn forward_ready(&self) -> bool {
        self.handle.as_ref().is_some_and(|handle| {
            self.runner
                .is_write_channel_valid(handle, sunset::ChanData::Normal)
        })
    }

    fn forward_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| self.runner.is_channel_finished(handle))
    }

    /// Advance bounded network/protocol work without blocking the socket.
    /// Caller data is never retried across a new connection.
    fn drive(&mut self, deadline: time::Instant) -> Result<bool, Error> {
        if self.closed {
            return Ok(false);
        }
        let mut any_progress = false;
        for _ in 0..DRIVE_BUDGET {
            // Drain an existing data packet before asking the protocol to move
            // on. A full queue stops network reads rather than dropping output.
            let mut progressed = self.drain_channel()?;
            if self.runner.read_channel_ready().is_some() {
                return Ok(any_progress || progressed);
            }
            match self.runner.progress().map_err(protocol)? {
                sunset::Event::None => {}
                sunset::Event::Progressed => progressed = true,
                sunset::Event::Cli(event) => {
                    progressed = true;
                    match event {
                        sunset::CliEvent::Hostkey(check) => {
                            let mut wire = Vec::new();
                            sunset::sshwire::ssh_push_vec(
                                &mut wire,
                                &check.hostkey().map_err(protocol)?,
                            )
                            .map_err(protocol)?;
                            // Also applied to subsequent key exchanges. Never
                            // accept a changed key just because the channel exists.
                            self.fingerprint =
                                self.trust
                                    .verify(&self.options.host, self.options.port, &wire)?;
                            check.accept().map_err(protocol)?;
                            self.verified = true;
                        }
                        sunset::CliEvent::Username(request) => {
                            if !self.verified {
                                return Err(transport(
                                    "authentication requested before verified host key",
                                ));
                            }
                            request.username(&self.options.user).map_err(protocol)?;
                        }
                        sunset::CliEvent::Pubkey(request) => {
                            if !self.verified {
                                return Err(transport(
                                    "identity requested before verified host key",
                                ));
                            }
                            if self.credentials.is_none() {
                                self.credentials = Some(auth::Credentials::load(
                                    &self.options.authentication,
                                    deadline,
                                )?);
                            }
                            let credentials = self.credentials.as_mut().expect("loaded above");
                            match credentials.next_key() {
                                Ok(key) => request.pubkey(key).map_err(protocol)?,
                                Err(error) if credentials.offered() == 0 => {
                                    request.skip().map_err(protocol)?;
                                    return Err(error);
                                }
                                Err(_) => {
                                    // Already offered at least one key. Another
                                    // Pubkey event means "any more?"; skipping
                                    // lets the handshake finish instead of
                                    // treating a successful first key as failure.
                                    request.skip().map_err(protocol)?;
                                }
                            }
                        }
                        sunset::CliEvent::AgentSign(request) => {
                            self.credentials
                                .as_mut()
                                .ok_or_else(|| transport("unexpected signing request"))?
                                .sign(request, deadline)?;
                        }
                        sunset::CliEvent::Password(request) => {
                            request.skip().map_err(protocol)?;
                            return Err(Error::new(
                                Kind::Authentication,
                                "server requires another authentication method; no password fallback",
                            ));
                        }
                        sunset::CliEvent::Authenticated => {
                            if !self.verified {
                                return Err(transport("unverified authentication"));
                            }
                            self.authenticated = true;
                        }
                        sunset::CliEvent::SessionOpened(mut opened) => {
                            if self.handle.as_ref().map(sunset::ChanHandle::num)
                                != Some(opened.channel())
                            {
                                return Err(transport("unexpected SSH channel"));
                            }
                            let command = self
                                .command
                                .take()
                                .ok_or_else(|| transport("unexpected exec notification"))?;
                            opened.exec(command).map_err(protocol)?;
                            // Sunset sends exec without requesting a reply.
                            // Command readiness is established by tmux responses,
                            // not by this flag (and denied execs end the channel).
                            self.started = true;
                        }
                        sunset::CliEvent::SessionExit(_) | sunset::CliEvent::Banner(_) => {}
                        sunset::CliEvent::Defunct => self.closed = true,
                        sunset::CliEvent::PollAgain => {}
                    }
                }
                sunset::Event::Serv(_) => return Err(transport("unexpected server-side event")),
            }
            if self.closed {
                return Ok(true);
            }
            progressed |= self.flush_output()?;
            progressed |= self.drain_channel()?;
            if self.runner.read_channel_ready().is_some() {
                return Ok(any_progress || progressed);
            }
            if self.runner.is_input_ready() {
                if self.incoming_offset == self.incoming.len() {
                    let mut buffer = [0; 16 * 1024];
                    match self.transport.read(&mut buffer) {
                        Ok(0) => {
                            self.runner.close_input();
                            progressed = true;
                        }
                        Ok(count) => {
                            self.incoming.clear();
                            self.incoming.extend_from_slice(&buffer[..count]);
                            self.incoming_offset = 0;
                            progressed = true;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            progressed = true
                        }
                        Err(error) => return Err(transport(error)),
                    }
                }
                if self.incoming_offset < self.incoming.len() {
                    let count = self
                        .runner
                        .input(&self.incoming[self.incoming_offset..])
                        .map_err(protocol)?;
                    self.incoming_offset += count;
                    progressed |= count != 0;
                }
            }
            any_progress |= progressed;
            if !progressed {
                break;
            }
        }
        Ok(any_progress)
    }

    fn drain_channel(&mut self) -> Result<bool, Error> {
        let Some((number, data, pending)) = self.runner.read_channel_ready() else {
            return Ok(false);
        };
        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| transport("data before exec channel"))?;
        if handle.num() != number {
            return Err(transport("data for unknown channel"));
        }
        let (queue, limit) = match data {
            sunset::ChanData::Normal => (&mut self.stdout, MAX_QUEUED_STDOUT),
            sunset::ChanData::Stderr => (&mut self.stderr, MAX_QUEUED_STDERR),
        };
        let mut buffer = [0; 8192];
        let capacity = buffer
            .len()
            .min(limit.saturating_sub(queue.len()))
            .min(pending);
        if capacity == 0 {
            return Ok(false);
        }
        let count = self
            .runner
            .read_channel(handle, data, &mut buffer[..capacity])
            .map_err(protocol)?;
        queue.extend(&buffer[..count]);
        Ok(count != 0)
    }

    fn flush_output(&mut self) -> Result<bool, Error> {
        let bytes = self.runner.output_buf();
        if bytes.is_empty() {
            return Ok(false);
        }
        match self.transport.write(bytes) {
            Ok(0) => Err(transport(
                "SSH transport closed during write; delivery is uncertain",
            )),
            Ok(count) => {
                self.runner.consume_output(count);
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(error) => Err(transport(error)),
        }
    }

    fn wait_ready(&mut self, deadline: time::Instant) -> Result<(), Error> {
        let interest = if self.runner.is_output_pending() {
            polling::Event::all(0)
        } else {
            polling::Event::readable(0)
        };
        let socket = match &mut self.transport {
            Transport::Tcp(socket) => socket,
            // There is nothing here to poll: the hop below owns the only real
            // socket, so waiting means letting that hop advance and block.
            Transport::Forward(channel) => return channel.wait(deadline),
        };
        self.poller.modify(&*socket, interest).map_err(transport)?;
        loop {
            self.events.clear();
            match self
                .poller
                .wait(&mut self.events, Some(remaining(deadline)?))
            {
                Ok(count) if count != 0 => return Ok(()),
                Ok(_) if self.wake_pending.load(sync::atomic::Ordering::Acquire) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(transport(error)),
            }
        }
    }

    fn channel_eof(&self) -> bool {
        self.closed
            || self.handle.as_ref().is_some_and(|handle| {
                self.runner.is_channel_eof(handle)
                    || self.runner.is_channel_closed(handle)
                    // A refused or released channel is neither "eof" nor
                    // "closed" to Sunset, but it will never carry data again.
                    // Without this a reader waits out its whole deadline.
                    || self.runner.is_channel_finished(handle)
            })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Transport::Tcp(socket) = &self.transport {
            let _ = self.poller.delete(socket);
        }
        self.transport.shutdown();
    }
}

pub struct Channel {
    connection: Connection,
}

impl Channel {
    #[cfg(feature = "gui")]
    pub(crate) fn waker(&self) -> Wake {
        Wake {
            poller: sync::Arc::clone(&self.connection.poller),
            pending: sync::Arc::clone(&self.connection.wake_pending),
        }
    }

    pub(crate) fn take_wakeup(&self) -> bool {
        self.connection
            .wake_pending
            .swap(false, sync::atomic::Ordering::AcqRel)
    }

    pub fn timeout(&self) -> time::Duration {
        self.connection.options.timeout
    }
    pub fn fingerprint(&self) -> &str {
        self.connection.fingerprint()
    }
    pub fn eof(&self) -> bool {
        self.connection.channel_eof()
            && self.connection.stdout.is_empty()
            && self.connection.stderr.is_empty()
    }
    pub fn wait(&mut self, deadline: time::Instant) -> Result<(), Error> {
        remaining(deadline)?;
        if !self.connection.drive(deadline)? && !self.eof() {
            self.connection.wait_ready(deadline)?;
        }
        Ok(())
    }
    pub fn read_stderr(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.read_stream(bytes, sunset::ChanData::Stderr)
    }
    pub fn abort(&mut self) {
        self.connection.closed = true;
        self.connection.transport.shutdown();
    }
    fn read_stream(&mut self, bytes: &mut [u8], data: sunset::ChanData) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.connection
            .drive(time::Instant::now() + self.timeout())
            .map_err(io::Error::other)?;
        let eof = self.connection.channel_eof();
        let queue = match data {
            sunset::ChanData::Normal => &mut self.connection.stdout,
            sunset::ChanData::Stderr => &mut self.connection.stderr,
        };
        let count = bytes.len().min(queue.len());
        for byte in &mut bytes[..count] {
            *byte = queue.pop_front().expect("length checked");
        }
        if count != 0 || eof {
            Ok(count)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

impl io::Read for Channel {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.read_stream(bytes, sunset::ChanData::Normal)
    }
}

impl io::Write for Channel {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.connection
            .drive(time::Instant::now() + self.timeout())
            .map_err(io::Error::other)?;
        if self.connection.channel_eof() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let handle = self
            .connection
            .handle
            .as_ref()
            .expect("exec opened channel");
        let count = self
            .connection
            .runner
            .write_channel(handle, sunset::ChanData::Normal, bytes)
            .map_err(|error| io::Error::other(protocol(error)))?;
        self.connection.flush_output().map_err(io::Error::other)?;
        if count == 0 {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(count)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.connection
            .drive(time::Instant::now() + self.timeout())
            .map_err(io::Error::other)?;
        if self.connection.runner.is_output_pending() {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.abort();
    }
}

fn configure_stream(socket: &net::TcpStream) -> Result<(), Error> {
    socket.set_nodelay(true).map_err(transport)?;
    socket.set_nonblocking(true).map_err(transport)?;
    // Idle control sessions otherwise sit silent until the next tmux command.
    // NAT and stateful filters then black-hole the TCP connection, and the next
    // request dies as "tmux reply deadline expired" instead of a transport loss.
    let _ = set_keepalive_idle(socket, 30);
    Ok(())
}

/// Seconds of silence before the first TCP keepalive probe. Not a tmux ping:
/// RTT in the status bar is the last small control command we already sent.
fn set_keepalive_idle(socket: &net::TcpStream, seconds: u32) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        let fd = socket.as_raw_fd();
        let seconds = seconds as std::ffi::c_int;
        let on: std::ffi::c_int = 1;
        const IPPROTO_TCP: i32 = 6;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const SOL_SOCKET: i32 = 1;
        #[cfg(target_os = "macos")]
        const SOL_SOCKET: i32 = 0xffff;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const SO_KEEPALIVE: i32 = 9;
        #[cfg(target_os = "macos")]
        const SO_KEEPALIVE: i32 = 0x0008;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const TCP_KEEPIDLE: i32 = 4;
        #[cfg(target_os = "macos")]
        const TCP_KEEPIDLE: i32 = 0x10;
        unsafe extern "C" {
            fn setsockopt(
                sockfd: i32,
                level: i32,
                optname: i32,
                optval: *const std::ffi::c_void,
                optlen: u32,
            ) -> i32;
        }
        let set = |level, name, value: &std::ffi::c_int| unsafe {
            setsockopt(
                fd,
                level,
                name,
                (value as *const std::ffi::c_int).cast(),
                std::mem::size_of_val(value) as u32,
            )
        };
        if set(SOL_SOCKET, SO_KEEPALIVE, &on) != 0 || set(IPPROTO_TCP, TCP_KEEPIDLE, &seconds) != 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (socket, seconds);
        Ok(())
    }
}

fn remaining(deadline: time::Instant) -> Result<time::Duration, Error> {
    deadline
        .checked_duration_since(time::Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| Error::new(Kind::Timeout, "operation deadline expired"))
}

fn protocol(error: sunset::Error) -> Error {
    let kind = if matches!(error, sunset::Error::NoAuthMethods) {
        Kind::Authentication
    } else {
        Kind::Transport
    };
    Error::new(kind, error)
}

fn transport(detail: impl fmt::Display) -> Error {
    Error::new(Kind::Transport, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            host: "example.invalid".to_owned(),
            port: 22,
            user: "someone".to_owned(),
            known_hosts: path::PathBuf::from("known_hosts"),
            authentication: Authentication::Agent,
            timeout: time::Duration::from_secs(5),
            jumps: Vec::new(),
        }
    }

    /// A chain is checked before anything is dialled, because the failure it
    /// prevents — an unbounded or self-referential route — is one that would
    /// otherwise look like a hang rather than an error.
    #[test]
    fn a_jump_chain_is_bounded_and_flat() {
        let mut direct = options();
        assert!(direct.validate().is_ok());

        direct.jumps = vec![options(); MAX_JUMPS];
        assert!(direct.validate().is_ok());

        direct.jumps = vec![options(); MAX_JUMPS + 1];
        assert_eq!(direct.validate().unwrap_err().kind, Kind::Configuration);

        // A hop with hops of its own could describe a cycle. Config resolution
        // refuses to build one; this refuses to traverse one.
        let mut nested = options();
        nested.jumps = vec![options()];
        direct.jumps = vec![nested];
        assert_eq!(direct.validate().unwrap_err().kind, Kind::Configuration);

        // A hop is validated as strictly as a destination.
        let mut invalid = options();
        invalid.port = 0;
        direct.jumps = vec![invalid];
        assert_eq!(direct.validate().unwrap_err().kind, Kind::Configuration);
    }

    #[test]
    fn diagnostics_cannot_emit_terminal_controls() {
        let error = Error::new(Kind::Transport, "remote\x1b]52;c;payload\x07\n");
        assert!(!error.to_string().contains('\x1b'));
        assert!(!error.to_string().contains('\n'));
    }
}
