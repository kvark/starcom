//! Desktop state and a single cancellable, read-only SSH worker.
//!
//! The worker never holds the model mutex during SSH or snapshot requests.
//! A replaced request cannot publish into the next connection's view.

use std::{env, path, sync, thread, time};

use crate::{core, session, snapshot, ssh, terminal, ui, window};

#[derive(Clone)]
pub struct Connection {
    pub options: ssh::Options,
    pub session: core::SessionName,
    pub socket: Option<String>,
    pub history: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Idle,
    Connecting,
    Watching,
    Resynchronizing,
    Disconnected,
    Failed,
    Demo,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Not connected",
            Self::Connecting => "Connecting",
            Self::Watching => "Watching",
            Self::Resynchronizing => "Resynchronizing",
            Self::Disconnected => "Disconnected",
            Self::Failed => "Connection failed",
            Self::Demo => "Demo data",
        }
    }
}

pub(crate) struct State {
    epoch: u64,
    pending: Option<Connection>,
    stopping: bool,
    pub generation: u64,
    pub phase: Phase,
    pub view: Option<snapshot::View>,
    pub view_label: String,
    pub error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            epoch: 0,
            pending: None,
            stopping: false,
            generation: 0,
            phase: Phase::Idle,
            view: None,
            view_label: String::new(),
            error: None,
        }
    }
}

impl State {
    fn cancel(&mut self) {
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("connection epoch exhausted");
        self.pending = None;
        self.error = None;
        if let Some(ref mut view) = self.view {
            view.disconnect();
        }
    }

    fn accepts(&self, epoch: u64) -> bool {
        !self.stopping && self.epoch == epoch
    }
}

type Shared = sync::Arc<(sync::Mutex<State>, sync::Condvar)>;
type Wake = sync::Arc<dyn Fn() + Send + Sync>;

pub struct Client {
    shared: Shared,
    worker: Option<thread::JoinHandle<()>>,
    wake: Wake,
}

impl Client {
    pub fn new(wake: Wake) -> std::io::Result<Self> {
        let shared = sync::Arc::new((sync::Mutex::new(State::default()), sync::Condvar::new()));
        let worker_shared = sync::Arc::clone(&shared);
        let worker_wake = sync::Arc::clone(&wake);
        let worker = thread::Builder::new()
            .name("starcom-ssh".to_owned())
            .spawn(move || worker_loop(worker_shared, worker_wake))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            wake,
        })
    }

    pub fn connect(&self, connection: Connection) -> anyhow::Result<()> {
        connection.options.validate()?;
        anyhow::ensure!(
            connection.history <= snapshot::MAX_HISTORY_LINES,
            "history exceeds budget"
        );
        let mut state = self.lock();
        state.cancel();
        state.phase = Phase::Connecting;
        state.pending = Some(connection);
        drop(state);
        self.shared.1.notify_one();
        (self.wake)();
        Ok(())
    }

    pub fn disconnect(&self) {
        let mut state = self.lock();
        state.cancel();
        state.phase = Phase::Disconnected;
        drop(state);
        self.shared.1.notify_one();
        (self.wake)();
    }

    pub fn demo(&self) -> anyhow::Result<()> {
        let view = demo_view()?;
        let mut state = self.lock();
        state.cancel();
        state.view = Some(view);
        state.generation += 1;
        state.view_label = "Local demo / work".to_owned();
        state.phase = Phase::Demo;
        drop(state);
        self.shared.1.notify_one();
        (self.wake)();
        Ok(())
    }

    pub fn phase(&self) -> Phase {
        self.lock().phase
    }

    /// Inspect models without exposing the lock or allowing a borrowed view to
    /// outlive it. No networking runs under this lock.
    pub fn with_view<R>(&self, read: impl FnOnce(Option<&snapshot::View>) -> R) -> R {
        read(self.lock().view.as_ref())
    }

    pub(crate) fn lock(&self) -> sync::MutexGuard<'_, State> {
        self.shared
            .0
            .lock()
            .unwrap_or_else(sync::PoisonError::into_inner)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let mut state = self.lock();
        state.cancel();
        state.stopping = true;
        drop(state);
        self.shared.1.notify_one();
        // Normal channel polling stops within its bounded wait. DNS and local
        // file/agent setup may block outside network operation deadlines; do not freeze window
        // closure waiting for those. There is only one worker, not one per retry.
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

fn worker_loop(shared: Shared, wake: Wake) {
    loop {
        let (epoch, connection) = {
            let state = shared
                .0
                .lock()
                .unwrap_or_else(sync::PoisonError::into_inner);
            let mut state = shared
                .1
                .wait_while(state, |state| !state.stopping && state.pending.is_none())
                .unwrap_or_else(sync::PoisonError::into_inner);
            if state.stopping {
                return;
            }
            (
                state.epoch,
                state.pending.take().expect("request checked above"),
            )
        };
        if let Err(error) = watch(&shared, &wake, epoch, &connection) {
            let mut state = shared
                .0
                .lock()
                .unwrap_or_else(sync::PoisonError::into_inner);
            if state.accepts(epoch) {
                if let Some(ref mut view) = state.view {
                    view.disconnect();
                }
                state.phase = Phase::Failed;
                // Do not emit credentials or remote output to logs. Error
                // display is plain GUI text, bounded independently of the wire.
                state.error = Some(format!("{error:#}").chars().take(2048).collect());
                drop(state);
                wake();
            }
        }
    }
}

fn watch(shared: &Shared, wake: &Wake, epoch: u64, connection: &Connection) -> anyhow::Result<()> {
    let attached = session::Session::attach(
        &connection.options,
        &connection.session,
        connection.socket.as_deref(),
        connection.history,
    )?;
    let (mut inspector, view) = attached.into_parts();
    let session_id = view.session;
    {
        let mut state = shared
            .0
            .lock()
            .unwrap_or_else(sync::PoisonError::into_inner);
        if !state.accepts(epoch) {
            return Ok(());
        }
        state.view = Some(view);
        state.generation += 1;
        state.view_label = format!(
            "{}@{} / {}",
            connection.options.user,
            connection.options.host,
            connection.session.as_str()
        );
        state.phase = Phase::Watching;
    }
    wake();
    loop {
        let status = {
            let state = shared
                .0
                .lock()
                .unwrap_or_else(sync::PoisonError::into_inner);
            if !state.accepts(epoch) {
                return Ok(());
            }
            state.view.as_ref().expect("view published").status()
        };
        match status {
            snapshot::Status::Disconnected => {
                let mut state = shared
                    .0
                    .lock()
                    .unwrap_or_else(sync::PoisonError::into_inner);
                if state.accepts(epoch) {
                    state.phase = Phase::Disconnected;
                }
                drop(state);
                wake();
                return Ok(());
            }
            snapshot::Status::NeedsResync => {
                {
                    let mut state = shared
                        .0
                        .lock()
                        .unwrap_or_else(sync::PoisonError::into_inner);
                    if !state.accepts(epoch) {
                        return Ok(());
                    }
                    state.phase = Phase::Resynchronizing;
                }
                wake();
                let restored = session::restore(&mut inspector, session_id, connection.history)?;
                let mut state = shared
                    .0
                    .lock()
                    .unwrap_or_else(sync::PoisonError::into_inner);
                if !state.accepts(epoch) {
                    return Ok(());
                }
                state.view = Some(restored);
                state.generation += 1;
                state.phase = Phase::Watching;
                drop(state);
                wake();
            }
            snapshot::Status::Watching => {
                // Socket readiness wait, not a repaint timer. Idle reads do not
                // wake the UI. All network I/O is outside the model mutex.
                let notifications =
                    inspector.poll(time::Instant::now() + time::Duration::from_millis(100))?;
                if notifications.is_empty() {
                    continue;
                }
                let mut state = shared
                    .0
                    .lock()
                    .unwrap_or_else(sync::PoisonError::into_inner);
                if !state.accepts(epoch) {
                    return Ok(());
                }
                let view = state.view.as_mut().expect("view published");
                for notification in notifications {
                    view.apply(notification);
                }
                drop(state);
                wake();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Startup {
    ConnectionForm,
    Demo,
}

pub fn run(startup: Startup) -> anyhow::Result<()> {
    window::run(startup)
}

pub fn save_demo(path: &path::Path) -> anyhow::Result<()> {
    let mut state = State {
        view: Some(demo_view()?),
        generation: 1,
        phase: Phase::Demo,
        view_label: "Local demo / work".to_owned(),
        ..State::default()
    };
    let mut ui = ui::DesktopUi::default();
    ui.form.user = "demo".to_owned();
    window::save_snapshot(&mut state, &mut ui, path)
}

pub(crate) fn home_path() -> Option<path::PathBuf> {
    env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(path::PathBuf::from)
}

pub(crate) fn demo_view() -> anyhow::Result<snapshot::View> {
    let mut panes = Vec::new();
    for (id, window_id, left, columns, rows) in
        [(0, 0, 0, 54, 27), (1, 0, 55, 54, 27), (2, 1, 0, 90, 27)]
    {
        let state = snapshot::State::parse(&format!(
            "%{id}|@{window_id}|{columns}|{rows}|{left}|0|0|0|0|0|2000|||0|{}|1|0|0|0|1|0|0|0|0|0|1|",
            rows - 1
        ))?;
        let mut terminal = terminal::Terminal::new(state.size, 300);
        if id == 0 {
            terminal.feed(b"\x1b[36mStarcom\x1b[0m  /  terminal workspace\r\n\r\n");
            terminal
                .feed(b"\x1b[90mThis is built-in demo data, not an SSH session.\x1b[0m\r\n\r\n");
            terminal.feed(b"\x1b[32mdemo@workstation\x1b[0m:~/starcom$ cargo test\r\n\r\n");
            terminal.feed(b"  \x1b[32mCompiling\x1b[0m starcom v0.1.0\r\n  \x1b[32mFinished\x1b[0m test profile\r\n\r\n");
            for name in [
                "control framing",
                "snapshot -> live",
                "Unicode selection",
                "independent panes",
            ] {
                terminal.feed(format!("test {name:<28} ... \x1b[32mok\x1b[0m\r\n").as_bytes());
            }
            terminal.feed(b"\r\n\x1b[36mDrag to select. Right-click to copy.\x1b[0m\r\n\r\n");
            terminal.feed(b"The window is a client; tmux owns your jobs.\r\n\r\n");
            terminal.feed(b"\x1b[32mdemo@workstation\x1b[0m:~/starcom$ ");
        } else {
            for line in 1..=80 {
                terminal.feed(format!("\x1b[90m[12:{:02}:{:02}]\x1b[0m  worker  \x1b[32mready\x1b[0m  batch {line:03}\r\n", line / 60, line % 60).as_bytes());
            }
            terminal.feed(b"\r\n\x1b[33mScroll upward to inspect retained output.\x1b[0m\r\n\r\n");
        }
        panes.push(snapshot::Pane {
            state,
            terminal,
            history_may_be_truncated: false,
        });
    }
    snapshot::View::new(tmuxctl::SessionId(0), panes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_or_replaced_epochs_cannot_publish() {
        let mut state = State::default();
        assert!(state.accepts(0));
        state.cancel();
        assert!(!state.accepts(0));
        assert!(state.accepts(1));
        state.stopping = true;
        assert!(!state.accepts(1));
    }

    #[test]
    fn demo_and_disconnect_need_no_network() {
        let client = Client::new(sync::Arc::new(|| {})).unwrap();
        client.demo().unwrap();
        assert_eq!(client.phase(), Phase::Demo);
        client.with_view(|view| assert_eq!(view.unwrap().panes().len(), 3));
        client.disconnect();
        assert_eq!(client.phase(), Phase::Disconnected);
        client.with_view(|view| assert_eq!(view.unwrap().status(), snapshot::Status::Disconnected));
    }
}
