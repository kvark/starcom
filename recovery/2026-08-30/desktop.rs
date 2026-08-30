//! Desktop state and a single cancellable SSH worker with bounded user input.
//!
//! The worker never holds the model mutex during SSH or snapshot requests.
//! A replaced request cannot publish into the next connection's view.

use std::{collections, env, path, sync, thread, time};

use crate::{core, input, session, snapshot, ssh, terminal, ui, window};

#[derive(Clone)]
pub struct Connection {
    pub options: ssh::Options,
    pub session: core::SessionName,
    pub socket: Option<String>,
    pub history: usize,
    pub access: session::Access,
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
            Self::Watching => "Connected",
            Self::Resynchronizing => "Resynchronizing",
            Self::Disconnected => "Disconnected",
            Self::Failed => "Connection failed",
            Self::Demo => "Demo data",
        }
    }
}

const MAX_PENDING_ACTIONS: usize = 64;
const MAX_PENDING_BYTES: usize = 128 * 1024;

/// A UI action is bound to the exact connection and reconstructed view in which
/// it originated. Pane IDs alone are unsafe across a new tmux server/connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    epoch: u64,
    generation: u64,
    pane: tmuxctl::PaneId,
}

impl Target {
    pub fn pane(self) -> tmuxctl::PaneId {
        self.pane
    }
}

struct Pending {
    target: Target,
    action: input::Action,
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
    pub access: session::Access,
    pub allow_resize: bool,
    actions: collections::VecDeque<Pending>,
    action_bytes: usize,
    io_wake: Option<ssh::Wake>,
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
            access: session::Access::ReadOnly,
            allow_resize: false,
            actions: collections::VecDeque::new(),
            action_bytes: 0,
            io_wake: None,
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
        self.discard_actions();
        self.access = session::Access::ReadOnly;
        self.allow_resize = false;
        if let Some(wake) = self.io_wake.take() {
            wake.notify();
        }
        if let Some(ref mut view) = self.view {
            view.disconnect();
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(crate) fn input_ready(&self) -> bool {
        self.phase == Phase::Watching
            && self.access == session::Access::Interactive
            && self
                .view
                .as_ref()
                .is_some_and(|view| view.status() == snapshot::Status::Watching)
    }

    pub(crate) fn target(&self, pane: tmuxctl::PaneId) -> Option<Target> {
        (self.input_ready() && self.view.as_ref()?.panes().contains_key(&pane)).then_some(Target {
            epoch: self.epoch,
            generation: self.generation,
            pane,
        })
    }

    fn discard_actions(&mut self) {
        self.actions.clear();
        self.action_bytes = 0;
    }

    pub(crate) fn enqueue(&mut self, target: Target, action: input::Action) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.target(target.pane) == Some(target),
            "input was not sent: the connection or layout is no longer current"
        );
        anyhow::ensure!(
            !matches!(action, input::Action::Resize(_)) || self.allow_resize,
            "remote resizing is disabled; it changes the shared tmux layout"
        );
        session::validate_action(
            self.view.as_ref().expect("target validated"),
            target.pane,
            &action,
        )?;
        let size = action.size();
        anyhow::ensure!(
            self.actions.len() < MAX_PENDING_ACTIONS
                && self.action_bytes + size <= MAX_PENDING_BYTES,
            "input queue is full; this action was not sent"
        );
        // Coalesce ordinary text, without moving it past a key/paste/resize.
        if let input::Action::Bytes(ref bytes) = action
            && let Some(Pending {
                target: previous,
                action: input::Action::Bytes(queued),
            }) = self.actions.back_mut()
            && *previous == target
            && queued.len() + bytes.len() <= crate::command::MAX_INPUT_BYTES
        {
            queued.extend_from_slice(bytes);
        } else {
            self.actions.push_back(Pending { target, action });
        }
        self.action_bytes += size;
        if let Some(ref wake) = self.io_wake {
            wake.notify();
        }
        Ok(())
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

    /// Obtain a token only after restoration. Keep it with any delayed GUI
    /// action (such as paste confirmation); never recreate a token on retry.
    pub fn target(&self, pane: tmuxctl::PaneId) -> Option<Target> {
        self.lock().target(pane)
    }

    pub fn submit(&self, target: Target, action: input::Action) -> anyhow::Result<()> {
        self.lock().enqueue(target, action)
    }

    /// Admit a GUI frame atomically, so a full queue cannot accept half of a
    /// committed UTF-8 string or reorder a paste and its following keys.
    pub(crate) fn submit_batch(&self, actions: Vec<(Target, input::Action)>) -> anyhow::Result<()> {
        let mut state = self.lock();
        let size: usize = actions.iter().map(|(_, action)| action.size()).sum();
        anyhow::ensure!(
            state.actions.len() + actions.len() <= MAX_PENDING_ACTIONS
                && state.action_bytes + size <= MAX_PENDING_BYTES,
            "input queue is full; this frame's actions were not sent"
        );
        for (target, action) in &actions {
            anyhow::ensure!(
                state.target(target.pane) == Some(*target),
                "input target changed; nothing was sent"
            );
            anyhow::ensure!(
                !matches!(action, input::Action::Resize(_)) || state.allow_resize,
                "remote resizing is disabled"
            );
            session::validate_action(
                state.view.as_ref().expect("target checked"),
                target.pane,
                action,
            )?;
        }
        for (target, action) in actions {
            state.enqueue(target, action)?;
        }
        Ok(())
    }

    /// Explicit per-connection consent; automatic window sizing remains off.
    pub fn allow_remote_resize(&self, allow: bool) {
        let mut state = self.lock();
        state.allow_resize = allow && state.input_ready();
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
                state.discard_actions();
                state.io_wake = None;
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
    let attached = session::Session::attach_with_access(
        &connection.options,
        &connection.session,
        connection.socket.as_deref(),
        connection.history,
        connection.access,
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
        state.access = connection.access;
        state.io_wake = Some(inspector.waker());
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
                    state.discard_actions();
                    state.io_wake = None;
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
                    if !state.actions.is_empty() {
                        state.error = Some(
                            "Layout changed; queued input was discarded, not replayed.".to_owned(),
                        );
                    }
                    state.discard_actions();
                    state.phase = Phase::Resynchronizing;
                }
                wake();
                let mut restored =
                    session::restore(&mut inspector, session_id, connection.history)?;
                if connection.access == session::Access::Interactive {
                    for event in inspector.enable_input(&restored)? {
                        restored.apply(event);
                    }
                }
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
                let pending = {
                    let mut state = shared
                        .0
                        .lock()
                        .unwrap_or_else(sync::PoisonError::into_inner);
                    if !state.accepts(epoch) {
                        return Ok(());
                    }
                    if let Some(pending) = state.actions.pop_front() {
                        state.action_bytes -= pending.action.size();
                        if state.target(pending.target.pane) != Some(pending.target)
                            || (matches!(pending.action, input::Action::Resize(_))
                                && !state.allow_resize)
                        {
                            continue;
                        }
                        let target = session::action_target(
                            state.view.as_ref().expect("view published"),
                            pending.target.pane,
                        )?;
                        let resizing = matches!(pending.action, input::Action::Resize(_));
                        let ordinary = matches!(
                            pending.action,
                            input::Action::Bytes(_) | input::Action::Key(..)
                        );
                        let mut actions = vec![pending.action];
                        // Pipeline adjacent keystrokes in one tmux transaction,
                        // without moving them across a paste/resize or pane switch.
                        while ordinary
                            && actions.len() < 32
                            && state.actions.front().is_some_and(|next| {
                                next.target == pending.target
                                    && matches!(
                                        next.action,
                                        input::Action::Bytes(_) | input::Action::Key(..)
                                    )
                            })
                        {
                            let next = state.actions.pop_front().expect("front checked");
                            state.action_bytes -= next.action.size();
                            actions.push(next.action);
                        }
                        if resizing {
                            state.view.as_mut().expect("view published").invalidate();
                            state.phase = Phase::Resynchronizing;
                            if !state.actions.is_empty() {
                                state.error = Some(
                                    "Resize started; queued input was discarded, not replayed."
                                        .to_owned(),
                                );
                            }
                            state.discard_actions();
                        }
                        Some((target, actions))
                    } else {
                        None
                    }
                };
                if let Some((target, actions)) = pending {
                    // The pop above is the dispatch boundary. Cancellation may
                    // follow while I/O is in flight; these actions are NEVER requeued.
                    let outcome = inspector.interact(target, &actions)?;
                    let mut state = shared
                        .0
                        .lock()
                        .unwrap_or_else(sync::PoisonError::into_inner);
                    if !state.accepts(epoch) {
                        return Ok(());
                    }
                    for event in outcome.notifications {
                        state.view.as_mut().expect("view published").apply(event);
                    }
                    if !outcome.applied {
                        state.error = Some("tmux blocked this action: the pane changed, is in a mode, or synchronize-panes/zoom is enabled. Nothing was retried.".to_owned());
                        state.view.as_mut().expect("view published").invalidate();
                    }
                    drop(state);
                    wake();
                    continue;
                }
                // Socket readiness wait, not a repaint timer. Idle reads do not
                // wake the UI. All network I/O is outside the model mutex.
                let notifications =
                    inspector.poll(time::Instant::now() + time::Duration::from_secs(30))?;
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

    fn editable() -> State {
        State {
            phase: Phase::Watching,
            view: Some(demo_view().unwrap()),
            access: session::Access::Interactive,
            ..State::default()
        }
    }

    #[test]
    fn queue_is_bounded_and_cancellation_invalidates_all_actions() {
        let mut state = editable();
        let target = state.target(tmuxctl::PaneId(0)).unwrap();
        for _ in 0..MAX_PENDING_ACTIONS {
            state
                .enqueue(
                    target,
                    input::Action::Key(input::Key::Enter, input::Modifiers::default()),
                )
                .unwrap();
        }
        assert!(
            state
                .enqueue(target, input::Action::Bytes(b"overflow".to_vec()))
                .is_err()
        );
        assert_eq!(state.actions.len(), MAX_PENDING_ACTIONS);
        state.cancel();
        assert!(state.actions.is_empty());
        assert_eq!(state.action_bytes, 0);
        assert!(
            state
                .enqueue(target, input::Action::Bytes(b"stale".to_vec()))
                .is_err()
        );
    }

    #[test]
    fn queue_coalesces_bytes_without_crossing_a_key_or_pane_boundary() {
        let mut state = editable();
        let a = state.target(tmuxctl::PaneId(0)).unwrap();
        let b = state.target(tmuxctl::PaneId(1)).unwrap();
        for bytes in [b"one".to_vec(), b"two".to_vec()] {
            state.enqueue(a, input::Action::Bytes(bytes)).unwrap();
        }
        state
            .enqueue(
                a,
                input::Action::Key(input::Key::Enter, input::Modifiers::default()),
            )
            .unwrap();
        state
            .enqueue(b, input::Action::Bytes(b"three".to_vec()))
            .unwrap();
        assert_eq!(state.actions.len(), 3);
        assert!(
            matches!(&state.actions[0].action, input::Action::Bytes(bytes) if bytes == b"onetwo")
        );
        assert_eq!(state.actions[2].target, b);
        assert_eq!(state.action_bytes, 6 + 32 + 5);
    }

    #[test]
    fn a_gui_batch_is_rejected_as_a_whole_when_it_would_overflow() {
        let client = Client::new(sync::Arc::new(|| {})).unwrap();
        *client.lock() = editable();
        let target = client.target(tmuxctl::PaneId(0)).unwrap();
        let actions = (0..=MAX_PENDING_ACTIONS)
            .map(|_| {
                (
                    target,
                    input::Action::Key(input::Key::Enter, input::Modifiers::default()),
                )
            })
            .collect();
        assert!(client.submit_batch(actions).is_err());
        assert!(client.lock().actions.is_empty());
        assert_eq!(client.lock().action_bytes, 0);
    }
}
