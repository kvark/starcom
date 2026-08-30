//! Connection form and terminal workspace for one connection tab.

pub(crate) mod input;
mod layout;
mod terminal;

use std::{collections, env, path, sync, time};

use crate::{core, desktop, input as terminal_input, session, snapshot, ssh, ssh_config, store};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authentication {
    Agent,
    Key,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Connection,
    Terminal,
}

pub struct Form {
    destination: String,
    host: String,
    pub user: String,
    session: String,
    port: u16,
    authentication: Authentication,
    identity: String,
    known_hosts: String,
    socket: String,
    history: usize,
    interactive: bool,
    reconnect: bool,
    unsupported: Vec<String>,
    profile_error: Option<String>,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            destination: String::new(),
            host: String::new(),
            user: env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default(),
            session: "work".to_owned(),
            port: 22,
            authentication: Authentication::Agent,
            identity: String::new(),
            known_hosts: desktop::home_path()
                .map(|path| path.join(".ssh/known_hosts").to_string_lossy().into_owned())
                .unwrap_or_default(),
            socket: String::new(),
            history: 200,
            interactive: true,
            reconnect: true,
            unsupported: Vec::new(),
            profile_error: None,
        }
    }
}

impl Form {
    pub fn destination(&self) -> &str {
        self.destination.trim()
    }

    /// The non-secret half of this form: where to connect and how, never
    /// anything that would let a reader of the saved file connect.
    pub(crate) fn saved(&self) -> store::Tab {
        store::Tab {
            destination: self.destination.clone(),
            host: self.host.clone(),
            user: self.user.clone(),
            session: self.session.clone(),
            port: self.port,
            agent: self.authentication == Authentication::Agent,
            identity: self.identity.clone(),
            known_hosts: self.known_hosts.clone(),
            socket: self.socket.clone(),
            history: self.history,
            interactive: self.interactive,
            reconnect: self.reconnect,
        }
    }

    /// Fill a form from a saved tab. Deliberately does not connect: restoring a
    /// workspace must never authenticate on the user's behalf at startup.
    pub(crate) fn restore(saved: store::Tab) -> Self {
        Self {
            destination: saved.destination,
            host: saved.host,
            user: saved.user,
            session: saved.session,
            port: saved.port,
            authentication: if saved.agent {
                Authentication::Agent
            } else {
                Authentication::Key
            },
            identity: saved.identity,
            known_hosts: saved.known_hosts,
            socket: saved.socket,
            history: saved.history.min(snapshot::MAX_HISTORY_LINES),
            interactive: saved.interactive,
            reconnect: saved.reconnect,
            unsupported: Vec::new(),
            profile_error: None,
        }
    }

    fn apply_profile(&mut self, profile: ssh_config::Profile) {
        self.host = profile.host;
        if let Some(user) = profile.user {
            self.user = user;
        }
        self.port = profile.port.unwrap_or(22);
        if let Some(identity) = profile.identity {
            self.identity = identity.to_string_lossy().into_owned();
            self.authentication = Authentication::Key;
        } else if !profile.identities_only {
            self.authentication = Authentication::Agent;
        }
        if let Some(known_hosts) = profile.known_hosts {
            self.known_hosts = known_hosts.to_string_lossy().into_owned();
        }
        self.unsupported = profile.unsupported;
        self.profile_error = None;
    }

    fn connection(&self) -> anyhow::Result<desktop::Connection> {
        fn local_path(text: &str) -> anyhow::Result<path::PathBuf> {
            if let Some(tail) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
                return desktop::home_path()
                    .map(|home| home.join(tail))
                    .ok_or_else(|| anyhow::anyhow!("home directory is unavailable"));
            }
            Ok(path::PathBuf::from(text))
        }

        anyhow::ensure!(
            self.unsupported.is_empty(),
            "this SSH profile uses unsupported policy: {}",
            self.unsupported.join(", ")
        );
        anyhow::ensure!(self.profile_error.is_none(), "fix the SSH profile first");
        let authentication = match self.authentication {
            Authentication::Agent => ssh::Authentication::Agent,
            Authentication::Key => {
                anyhow::ensure!(
                    !self.identity.is_empty(),
                    "choose a private-key file or use an SSH agent"
                );
                ssh::Authentication::Identity(local_path(&self.identity)?)
            }
        };
        let options = ssh::Options {
            host: self.host.trim().to_owned(),
            user: self.user.trim().to_owned(),
            port: self.port,
            authentication,
            known_hosts: local_path(&self.known_hosts)?,
            timeout: time::Duration::from_secs(10),
        };
        options.validate()?;
        Ok(desktop::Connection {
            options,
            session: core::SessionName::new(self.session.clone())?,
            socket: (!self.socket.is_empty()).then(|| self.socket.clone()),
            history: self.history,
            access: if self.interactive {
                session::Access::Interactive
            } else {
                session::Access::ReadOnly
            },
            reconnect: self.reconnect,
        })
    }
}

/// One terminal step in the order the user produced it. A clipboard read cannot
/// happen inside the egui closure, so a paste request stays an ordered step
/// rather than replacing the frame's other input.
pub(crate) enum Step {
    Send(desktop::Target, terminal_input::Action),
    RequestPaste(desktop::Target),
}

pub enum Action {
    None,
    Connect(desktop::Connection),
    /// Ask the host which sessions exist. Cannot start a tmux server.
    ListSessions(desktop::Connection),
    /// Explicitly create a session, which may start a server. Only ever from a
    /// confirmed button press, never from a failed attach.
    CreateSession(desktop::Connection),
    Disconnect,
    Demo,
    /// Every terminal step this frame produced, in order. Never a subset: a
    /// frame that cannot deliver all of its steps reports that to the user.
    Frame(Vec<Step>),
    ReloadConfig,
}

pub struct DesktopUi {
    pub form: Form,
    pub config: sync::Arc<ssh_config::Config>,
    pub config_load_error: Option<String>,
    screen: Screen,
    profile_source: String,
    font_size: f32,
    generation: u64,
    windows: collections::BTreeMap<tmuxctl::WindowId, layout::Node>,
    window: Option<tmuxctl::WindowId>,
    focused: Option<tmuxctl::PaneId>,
    pane_ui: collections::BTreeMap<tmuxctl::PaneId, terminal::PaneUi>,
    pending_paste: Option<(desktop::Target, terminal_input::Paste)>,
    notice: Option<String>,
    /// Set while the user is confirming that creating a session may start a
    /// tmux server on a host that has none.
    confirm_create: bool,
}

impl Default for DesktopUi {
    fn default() -> Self {
        Self::with_config(sync::Arc::new(ssh_config::Config::default()), None)
    }
}

impl DesktopUi {
    pub fn with_config(
        config: sync::Arc<ssh_config::Config>,
        config_load_error: Option<String>,
    ) -> Self {
        Self {
            form: Form::default(),
            config,
            config_load_error,
            screen: Screen::Connection,
            profile_source: String::new(),
            font_size: 14.0,
            generation: u64::MAX,
            windows: collections::BTreeMap::new(),
            window: None,
            focused: None,
            pane_ui: collections::BTreeMap::new(),
            pending_paste: None,
            notice: None,
            confirm_create: false,
        }
    }

    /// Restore a saved tab into this UI's form, showing the connection screen.
    pub(crate) fn restore(&mut self, saved: store::Tab) {
        self.form = Form::restore(saved);
        self.profile_source = self.form.destination().to_owned();
        self.screen = Screen::Connection;
    }

    pub(crate) fn saved(&self) -> store::Tab {
        self.form.saved()
    }

    pub fn open_terminal(&mut self) {
        self.screen = Screen::Terminal;
        self.pending_paste = None;
    }

    pub fn cancel_transient(&mut self) {
        self.pending_paste = None;
        self.confirm_create = false;
    }

    pub fn refresh_profile(&mut self) {
        let destination = self.form.destination().to_owned();
        self.profile_source = destination.clone();
        self.form.unsupported.clear();
        self.form.profile_error = None;
        if destination.is_empty() {
            self.form.host.clear();
            return;
        }
        match self.config.resolve(&destination) {
            Ok(profile) => self.form.apply_profile(profile),
            Err(error) => {
                self.form.host = destination;
                self.form.profile_error = Some(error.to_string());
            }
        }
    }

    pub fn terminal_focused(&self, ctx: &egui::Context) -> bool {
        self.screen == Screen::Terminal
            && self.focused.is_some_and(|pane| {
                ctx.memory(|memory| {
                    memory.has_focus(egui::Id::new(("terminal", self.generation, pane.0)))
                })
            })
    }

    /// Resolve one clipboard read into an ordered action, or arm the multiline
    /// confirmation. Returning None never means "silently dropped": every path
    /// that declines to send leaves a notice for the user.
    pub(crate) fn clipboard_paste(
        &mut self,
        state: &desktop::State,
        target: desktop::Target,
        text: &str,
    ) -> Option<terminal_input::Action> {
        if state.target(target.pane()) != Some(target) {
            self.notice = Some("Paste target changed; nothing was sent.".to_owned());
            return None;
        }
        match terminal_input::Paste::new(text) {
            Ok(paste) if paste.is_multiline() => {
                self.pending_paste = Some((target, paste));
                None
            }
            Ok(paste) => Some(terminal_input::Action::Paste(paste)),
            Err(error) => {
                self.notice = Some(error.to_string());
                None
            }
        }
    }

    pub fn show(&mut self, root: &mut egui::Ui, state: &mut desktop::State) -> Action {
        if self.profile_source != self.form.destination() {
            self.refresh_profile();
        }
        match self.screen {
            Screen::Connection => self.show_connection(root, state),
            Screen::Terminal => self.show_terminal(root, state),
        }
    }

    fn show_connection(&mut self, root: &mut egui::Ui, state: &desktop::State) -> Action {
        let mut action = Action::None;
        let mut chosen = None;
        egui::CentralPanel::default().show_inside(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.heading("Connect to an existing tmux session");
                    ui.weak("This connection belongs to this tab.");
                });
                ui.add_space(14.0);
                ui.scope(|ui| {
                    ui.set_max_width(680.0);
                    ui.horizontal(|ui| {
                        ui.label("SSH host or alias");
                        if ui.small_button("Reload config").clicked() {
                            action = Action::ReloadConfig;
                        }
                    });
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.form.destination)
                                .desired_width(f32::INFINITY)
                                .hint_text("host from ~/.ssh/config"),
                        )
                        .changed()
                    {
                        self.refresh_profile();
                    }

                    let query = self.form.destination().to_ascii_lowercase();
                    let aliases: Vec<_> = self
                        .config
                        .aliases()
                        .iter()
                        .filter(|name| query.is_empty() || name.to_ascii_lowercase().contains(&query))
                        .take(16)
                        .cloned()
                        .collect();
                    if !aliases.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak("Suggestions:");
                            for alias in aliases {
                                if ui.small_button(&alias).clicked() {
                                    self.form.destination = alias;
                                    self.refresh_profile();
                                }
                            }
                        });
                    } else {
                        ui.weak("No literal Host aliases match. Wildcard entries still supply defaults.");
                    }

                    if let Some(ref error) = self.config_load_error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    if let Some(ref error) = self.form.profile_error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    if !self.form.unsupported.is_empty() {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!(
                                "Unsupported SSH policy: {}. Starcom will not silently bypass it.",
                                self.form.unsupported.join(", ")
                            ),
                        );
                    }
                    if !self.form.host.is_empty() {
                        ui.weak(format!(
                            "Resolved endpoint: {}@{}:{}",
                            self.form.user, self.form.host, self.form.port
                        ));
                    }

                    ui.add_space(8.0);
                    field(ui, "User", &mut self.form.user);
                    field(ui, "tmux session", &mut self.form.session);
                    ui.checkbox(&mut self.form.interactive, "Allow terminal input");
                    ui.checkbox(
                        &mut self.form.reconnect,
                        "Reconnect automatically after connection loss",
                    )
                    .on_hover_text(
                        "Only transport loss is retried. Authentication, host-key, \
                         missing-session and detach failures always stop and wait for you.",
                    );
                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut self.form.authentication,
                            Authentication::Agent,
                            "SSH agent",
                        );
                        ui.radio_value(
                            &mut self.form.authentication,
                            Authentication::Key,
                            "Key file",
                        );
                    });
                    if self.form.authentication == Authentication::Key {
                        field(ui, "Private key", &mut self.form.identity);
                    }
                    ui.collapsing("Resolved and advanced settings", |ui| {
                        field(ui, "Host name / address", &mut self.form.host);
                        field(ui, "Known hosts", &mut self.form.known_hosts);
                        field(
                            ui,
                            "Remote tmux socket (optional)",
                            &mut self.form.socket,
                        );
                        ui.horizontal(|ui| {
                            ui.label("Port");
                            ui.add(egui::DragValue::new(&mut self.form.port).range(1..=65535));
                        });
                        ui.horizontal(|ui| {
                            ui.label("History lines");
                            ui.add(
                                egui::DragValue::new(&mut self.form.history)
                                    .range(0..=snapshot::MAX_HISTORY_LINES),
                            );
                        });
                    });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let enabled = self.form.profile_error.is_none()
                            && self.form.unsupported.is_empty()
                            && !self.form.host.trim().is_empty();
                        if ui.add_enabled(enabled, egui::Button::new("Connect")).clicked() {
                            match self.form.connection() {
                                Ok(connection) => {
                                    self.notice = None;
                                    action = Action::Connect(connection);
                                }
                                Err(error) => self.notice = Some(error.to_string()),
                            }
                        }
                        // Discovery is a separate connection, so it must not
                        // compete with a live attachment on this tab.
                        let idle = !matches!(
                            state.phase,
                            desktop::Phase::Connecting
                                | desktop::Phase::Watching
                                | desktop::Phase::Resynchronizing
                                | desktop::Phase::Reconnecting
                        );
                        let busy = matches!(state.discovery, Some(desktop::Discovery::Running));
                        if ui
                            .add_enabled(
                                enabled && idle && !busy,
                                egui::Button::new("List sessions"),
                            )
                            .on_hover_text(
                                "Ask the host which tmux sessions exist. Uses tmux -N, so it \
                                 cannot start a server.",
                            )
                            .clicked()
                        {
                            match self.form.connection() {
                                Ok(connection) => {
                                    self.notice = None;
                                    action = Action::ListSessions(connection);
                                }
                                Err(error) => self.notice = Some(error.to_string()),
                            }
                        }
                        if ui
                            .add_enabled(
                                enabled && idle && !busy,
                                egui::Button::new("Create session"),
                            )
                            .on_hover_text(
                                "Create the named session on the host. This starts a tmux \
                                 server if none is running.",
                            )
                            .clicked()
                        {
                            self.confirm_create = true;
                        }
                        if ui.button("Open local demo").clicked() {
                            action = Action::Demo;
                        }
                        if matches!(
                            state.phase,
                            desktop::Phase::Connecting
                                | desktop::Phase::Watching
                                | desktop::Phase::Resynchronizing
                        ) && ui.button("Back to terminal").clicked()
                        {
                            self.open_terminal();
                        }
                    });
                    match state.discovery {
                        None => {}
                        Some(desktop::Discovery::Running) => {
                            ui.add_space(6.0);
                            ui.weak("Asking the host…");
                        }
                        Some(desktop::Discovery::Failed(ref detail)) => {
                            ui.add_space(6.0);
                            ui.colored_label(ui.visuals().error_fg_color, detail);
                        }
                        Some(desktop::Discovery::Created(ref name)) => {
                            ui.add_space(6.0);
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                format!("Created session {name}. Connect to attach to it."),
                            );
                        }
                        Some(desktop::Discovery::Sessions(ref found)) => {
                            ui.add_space(6.0);
                            if found.is_empty() {
                                ui.weak("The host is running tmux with no sessions.");
                            } else {
                                ui.weak("Sessions on the host:");
                                for summary in found {
                                    ui.horizontal(|ui| {
                                        // Choosing one only fills the field; it
                                        // does not attach on the user's behalf.
                                        if ui.button(&summary.name).clicked() {
                                            chosen = Some(summary.name.clone());
                                        }
                                        ui.weak(summary.describe());
                                    });
                                }
                            }
                        }
                    }
                    if let Some(ref notice) = self.notice {
                        ui.colored_label(ui.visuals().warn_fg_color, notice);
                    }
                    ui.add_space(8.0);
                    ui.weak("The remote host needs only stock SSH and tmux. Host keys are never accepted automatically.");
                });
            });
        });
        if let Some(name) = chosen {
            self.form.session = name;
        }
        if self.confirm_create {
            let mut confirmed = false;
            egui::Window::new("Create a tmux session?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(root.ctx(), |ui| {
                    ui.label(format!(
                        "Create session {:?} on {}?",
                        self.form.session.trim(),
                        self.form.host.trim()
                    ));
                    ui.add_space(4.0);
                    ui.weak(
                        "If the host is not already running tmux, this starts a server. \
                         Starcom never does that on its own.",
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.confirm_create = false;
                        }
                        if ui.button("Create session").clicked() {
                            confirmed = true;
                        }
                    });
                });
            if confirmed {
                self.confirm_create = false;
                match self.form.connection() {
                    Ok(connection) => {
                        self.notice = None;
                        action = Action::CreateSession(connection);
                    }
                    Err(error) => self.notice = Some(error.to_string()),
                }
            }
        }
        action
    }

    fn rebuild_layout(&mut self, state: &desktop::State) {
        if self.generation == state.generation {
            return;
        }
        self.windows.clear();
        self.pane_ui.clear();
        self.focused = None;
        if let Some(ref view) = state.view {
            let mut grouped = collections::BTreeMap::<_, Vec<_>>::new();
            for pane in view.panes().values() {
                grouped.entry(pane.state.window).or_default().push(pane);
            }
            for (id, panes) in grouped {
                if let Some(node) = layout::Node::from_panes(&panes) {
                    self.windows.insert(id, node);
                } else {
                    self.notice = Some(format!("Cannot lay out overlapping panes in window {id}"));
                }
            }
        }
        if self.window.is_none_or(|id| !self.windows.contains_key(&id)) {
            self.window = self.windows.keys().next().copied();
        }
        self.generation = state.generation;
    }

    fn show_terminal(&mut self, root: &mut egui::Ui, state: &mut desktop::State) -> Action {
        self.rebuild_layout(state);
        // Navigation and terminal steps are separate results, so a button press
        // can never quietly consume the keystrokes collected in the same frame.
        let mut action = Action::None;
        let mut steps: Vec<Step> = Vec::new();
        let connection_epoch = state.epoch();

        egui::Panel::top("toolbar").show_inside(root, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(if state.access == session::Access::Interactive {
                        "INTERACTIVE"
                    } else {
                        "READ-ONLY"
                    })
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                ui.separator();
                ui.label(state.phase.label());
                if ui.button("Connection settings").clicked() {
                    self.screen = Screen::Connection;
                    self.cancel_transient();
                }
                // Cancelling a scheduled retry is the same operation as
                // disconnecting: it ends this connection and keeps the last view.
                let reconnecting = state.phase == desktop::Phase::Reconnecting;
                if ui
                    .add_enabled(
                        reconnecting
                            || matches!(
                                state.phase,
                                desktop::Phase::Connecting
                                    | desktop::Phase::Watching
                                    | desktop::Phase::Resynchronizing
                            ),
                        egui::Button::new(if reconnecting {
                            "Stop reconnecting"
                        } else {
                            "Disconnect"
                        }),
                    )
                    .clicked()
                {
                    action = Action::Disconnect;
                }
                if state.access == session::Access::Interactive {
                    let mut allow = state.allow_resize;
                    if ui
                        .add_enabled(
                            state.input_ready(),
                            egui::Checkbox::new(&mut allow, "Resize remote panes"),
                        )
                        .on_hover_text("Changes the shared tmux layout for every attached client")
                        .changed()
                    {
                        state.allow_resize = allow && state.input_ready();
                    }
                    if !state.input_ready() {
                        state.allow_resize = false;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("+")
                        .on_hover_text("Larger terminal text")
                        .clicked()
                    {
                        self.font_size = (self.font_size + 1.0).min(28.0);
                    }
                    ui.label(format!("{} pt", self.font_size as u32));
                    if ui
                        .button("−")
                        .on_hover_text("Smaller terminal text")
                        .clicked()
                    {
                        self.font_size = (self.font_size - 1.0).max(10.0);
                    }
                    if ui.button("Copy selection").clicked() {
                        self.copy_selection(ui.ctx(), state);
                    }
                    if ui
                        .add_enabled(state.input_ready(), egui::Button::new("Paste"))
                        .clicked()
                        && let Some(target) = self.focused.and_then(|pane| state.target(pane))
                    {
                        steps.push(Step::RequestPaste(target));
                    }
                });
            });
        });

        egui::Panel::bottom("status").show_inside(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.small(if state.phase == desktop::Phase::Demo {
                    "Local demo — no SSH connection"
                } else if state.input_ready() {
                    "Click a pane to type · Wheel to scroll · Drag to select · Ctrl-Shift-C/V or Cmd-C/V"
                } else {
                    "Wheel to scroll · Drag to select · Right-click to copy"
                });
                if let Some(retry) = state.retry {
                    // The countdown is the only thing on screen that changes on
                    // its own, so ask for exactly the frames it needs.
                    ui.ctx().request_repaint_after(
                        retry.remaining().min(time::Duration::from_millis(250)),
                    );
                    ui.separator();
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "Reconnecting: attempt {} in {:.0}s. Nothing you type now is queued.",
                            retry.attempt,
                            retry.remaining().as_secs_f32().ceil()
                        ),
                    );
                }
                if let Some(ref continuity) = state.continuity {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, continuity);
                }
                if let Some(ref notice) = self.notice {
                    ui.separator();
                    ui.small(notice);
                }
                if let Some(ref error) = state.error {
                    ui.separator();
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        });

        egui::CentralPanel::default().show_inside(root, |ui| {
            if state.view.is_none() {
                ui.vertical_centered(|ui| {
                    ui.add_space(90.0);
                    ui.heading(state.phase.label());
                    ui.add_space(8.0);
                    ui.label("The connection form is kept in this tab.");
                });
                return;
            }
            ui.horizontal_wrapped(|ui| {
                for &id in self.windows.keys() {
                    if ui
                        .selectable_label(self.window == Some(id), format!("Window {id}"))
                        .clicked()
                    {
                        self.window = Some(id);
                    }
                }
                ui.separator();
                ui.label(&state.view_label);
            });
            if !matches!(state.phase, desktop::Phase::Watching | desktop::Phase::Demo) {
                ui.label(
                    egui::RichText::new("Showing the last received view; it may be stale.")
                        .color(ui.visuals().warn_fg_color),
                );
            }
            ui.separator();
            let rect = ui.available_rect_before_wrap();
            if let (Some(id), Some(view)) = (self.window, state.view.as_mut())
                && let Some(node) = self.windows.get_mut(&id)
            {
                let generation = self.generation;
                let font_size = self.font_size;
                let pane_ui = &mut self.pane_ui;
                let focused = &mut self.focused;
                let notice = &mut self.notice;
                let mut resizes = Vec::new();
                node.draw(
                    ui,
                    rect,
                    egui::Id::new(("split", id.0, generation, connection_epoch)),
                    state.allow_resize,
                    &mut resizes,
                    &mut |ui, rect, pane_id| {
                        if let Some(pane) = view.panes_mut().get_mut(&pane_id) {
                            pane_ui
                                .entry(pane_id)
                                .or_default()
                                .show(ui, rect, pane, generation, font_size, focused, notice);
                        }
                    },
                );
                for (pane, resize) in resizes {
                    if let Some(target) = state.target(pane) {
                        steps.push(Step::Send(target, terminal_input::Action::Resize(resize)));
                    }
                }
            }
        });

        if matches!(action, Action::None) && self.terminal_focused(root.ctx()) {
            let (events, modifiers) = root.input(|input| (input.events.clone(), input.modifiers));
            let target = self.focused.and_then(|pane| state.target(pane));
            if let Some(target) = target {
                for event in &events {
                    match input::translate(event, modifiers) {
                        Ok(Some(input::Event::Input(actions))) => {
                            steps.extend(
                                actions.into_iter().map(|action| Step::Send(target, action)),
                            );
                        }
                        Ok(Some(input::Event::Copy)) => self.copy_selection(root.ctx(), state),
                        Ok(Some(input::Event::Paste(paste))) => {
                            if paste.is_multiline() {
                                self.pending_paste = Some((target, paste));
                            } else {
                                steps
                                    .push(Step::Send(target, terminal_input::Action::Paste(paste)));
                            }
                        }
                        // Keeps its place in the frame; the clipboard read that
                        // resolves it happens outside this closure.
                        Ok(Some(input::Event::RequestPaste)) => {
                            steps.push(Step::RequestPaste(target))
                        }
                        Ok(None) => {}
                        Err(error) => self.notice = Some(error.to_string()),
                    }
                }
            }
        }

        if let Some((target, paste)) = self.pending_paste.clone() {
            let mut send = false;
            let mut cancel = false;
            egui::Window::new("Confirm multiline paste")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(root.ctx(), |ui| {
                    ui.label(format!(
                        "Send {} bytes across {} lines?",
                        paste.as_str().len(),
                        paste.as_str().lines().count()
                    ));
                    let mut preview: String = paste.as_str().chars().take(320).collect();
                    ui.add(
                        egui::TextEdit::multiline(&mut preview)
                            .desired_rows(6)
                            .interactive(false),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if ui.button("Send paste").clicked() {
                            send = true;
                        }
                    });
                });
            if cancel {
                self.pending_paste = None;
            } else if send {
                self.pending_paste = None;
                steps.push(Step::Send(target, terminal_input::Action::Paste(paste)));
            }
        }

        if matches!(action, Action::None) && !steps.is_empty() {
            Action::Frame(steps)
        } else {
            if !steps.is_empty() {
                // Only reachable if a navigation button and a terminal step land
                // in one frame. Say so rather than discarding input in silence.
                self.notice = Some(
                    "Terminal input was not sent because this frame changed the connection instead. Nothing was retried."
                        .to_owned(),
                );
            }
            action
        }
    }

    fn copy_selection(&mut self, ctx: &egui::Context, state: &desktop::State) {
        let text = self.focused.and_then(|id| {
            state
                .view
                .as_ref()?
                .panes()
                .get(&id)?
                .terminal
                .selected_text()
        });
        match text {
            Some(text) => terminal::copy(ctx, text, &mut self.notice),
            None => self.notice = Some("Select text in a pane first.".to_owned()),
        }
    }

    #[cfg(test)]
    pub fn smoke_selection(&self, ctx: &egui::Context) -> (egui::Pos2, egui::Pos2) {
        let rect = self
            .pane_ui
            .values()
            .next()
            .map(|pane| pane.rect)
            .unwrap_or(egui::Rect::from_min_size(
                egui::pos2(20.0, 80.0),
                egui::vec2(500.0, 400.0),
            ));
        let font = egui::FontId::monospace(self.font_size);
        let width = ctx.fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
        let start = rect.min + egui::vec2(8.0 + width * 0.2, 48.0);
        let end = start + egui::vec2(width * 6.8, 0.0);
        (start, end)
    }
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .char_limit(4096),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_alias_populates_the_connection_form() {
        let config = sync::Arc::new(ssh_config::Config::from_text(
            "Host dev\nHostName 10.0.0.2\nUser alice\nPort 2222\nIdentityFile ~/.ssh/dev",
        ));
        let mut ui = DesktopUi::with_config(config, None);
        ui.form.destination = "dev".to_owned();
        ui.refresh_profile();
        assert_eq!(ui.form.host, "10.0.0.2");
        assert_eq!(ui.form.user, "alice");
        assert_eq!(ui.form.port, 2222);
        assert_eq!(ui.form.authentication, Authentication::Key);
    }

    #[test]
    fn new_ui_starts_on_connection_form() {
        let ui = DesktopUi::default();
        assert_eq!(ui.screen, Screen::Connection);
    }

    /// A frame that produces a paste request AND keystrokes must deliver both,
    /// in order. Returning one action per frame used to discard the keystrokes.
    #[test]
    fn a_paste_request_does_not_displace_the_same_frame_keystrokes() {
        let ctx = egui::Context::default();
        crate::window::configure(&ctx);
        let mut state = desktop::State::interactive_demo().unwrap();
        let pane = tmuxctl::PaneId(0);
        let target = state.target(pane).expect("interactive demo target");
        let mut ui = DesktopUi::default();
        ui.open_terminal();
        let screen = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 720.0),
            )),
            ..Default::default()
        };
        // Lay the pane out and let the focus request settle. rebuild_layout
        // clears the focused pane on its first pass, so ask again each frame.
        for _ in 0..4 {
            let _ = ctx.run_ui(screen(), |root| {
                ui.show(root, &mut state);
            });
            ui.focused = Some(pane);
            ctx.memory_mut(|memory| {
                memory.request_focus(egui::Id::new(("terminal", ui.generation, pane.0)))
            });
        }
        assert!(
            ui.terminal_focused(&ctx),
            "the pane must hold focus or this test proves nothing"
        );
        let input = egui::RawInput {
            events: vec![
                egui::Event::Key {
                    key: egui::Key::Insert,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::SHIFT,
                },
                egui::Event::Text("ls".to_owned()),
            ],
            modifiers: egui::Modifiers::SHIFT,
            ..screen()
        };
        let mut action = Action::None;
        let _ = ctx.run_ui(input, |root| {
            action = ui.show(root, &mut state);
        });
        let Action::Frame(steps) = action else {
            panic!("expected an ordered terminal frame")
        };
        assert!(
            matches!(steps.as_slice(), [Step::RequestPaste(a), Step::Send(b, _)]
                if *a == target && *b == target),
            "the paste request and the keystrokes must both survive, in order"
        );
        assert!(ui.notice.is_none(), "nothing should have been dropped");
    }
}
