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

/// One resolved jump host, in the order it is traversed. Everything here comes
/// from the SSH config, not from the form: a bastion is routing policy, and the
/// user does not get asked to retype it.
#[derive(Clone, Debug)]
struct Jump {
    host: String,
    user: String,
    port: u16,
    identity: Option<String>,
    known_hosts: Option<String>,
}

/// The account `ssh` would use when nothing names one.
fn local_user() -> String {
    env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default()
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
    /// Jump hosts from the SSH config, traversed before `host`.
    jumps: Vec<Jump>,
    unsupported: Vec<String>,
    profile_error: Option<String>,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            destination: String::new(),
            host: String::new(),
            user: local_user(),
            session: String::new(),
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
            jumps: Vec::new(),
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
            // Filled in from the config by the caller: a saved tab records
            // where the user chose to connect, never how it is routed there.
            jumps: Vec::new(),
            unsupported: Vec::new(),
            profile_error: None,
        }
    }

    fn apply_profile(
        &mut self,
        profile: ssh_config::Profile,
        default_identity: Option<path::PathBuf>,
        agent_available: bool,
    ) {
        self.host = profile.host;
        if let Some(user) = profile.user {
            self.user = user;
        }
        self.port = profile.port.unwrap_or(22);
        if let Some(identity) = profile.identity {
            self.identity = identity.to_string_lossy().into_owned();
            self.authentication = Authentication::Key;
        } else {
            // OpenSSH still tries ~/.ssh/id_ed25519 and friends with no
            // IdentityFile and no agent. Put that path on the form instead of
            // insisting on an agent the user does not have.
            if let Some(identity) = default_identity {
                self.identity = identity.to_string_lossy().into_owned();
                if profile.identities_only || !agent_available {
                    self.authentication = Authentication::Key;
                } else {
                    self.authentication = Authentication::Agent;
                }
            } else if !profile.identities_only {
                self.authentication = Authentication::Agent;
            }
        }
        if let Some(known_hosts) = profile.known_hosts {
            self.known_hosts = known_hosts.to_string_lossy().into_owned();
        }
        self.apply_routing(profile.jumps, profile.unsupported);
        self.profile_error = None;
    }

    /// The part of a profile that says how the connection is routed and what
    /// policy blocks it, separate from the endpoint and identity the user may
    /// have edited. Restoring a tab takes this and nothing else.
    fn apply_routing(&mut self, jumps: Vec<ssh_config::Profile>, unsupported: Vec<String>) {
        self.jumps = jumps
            .into_iter()
            .map(|hop| Jump {
                host: hop.host,
                // A hop with no User of its own gets the local account, the way
                // `ssh -J` does. It does not inherit the destination's user.
                user: hop.user.unwrap_or_else(local_user),
                port: hop.port.unwrap_or(22),
                identity: hop.identity.map(|path| path.to_string_lossy().into_owned()),
                known_hosts: hop
                    .known_hosts
                    .map(|path| path.to_string_lossy().into_owned()),
            })
            .collect();
        self.unsupported = unsupported;
    }

    fn host_ready(&self) -> bool {
        self.profile_error.is_none() && self.unsupported.is_empty() && !self.host.trim().is_empty()
    }

    /// Listing does not attach, so it does not need a session name.
    fn listing(&self) -> anyhow::Result<desktop::Connection> {
        self.connection_named("_")
    }

    /// The route, for the form to show. A bastion changes who sees the
    /// connection, so it is never invisible on the screen that starts one.
    fn route(&self) -> String {
        self.jumps
            .iter()
            .map(|hop| format!("{}@{}:{}", hop.user, hop.host, hop.port))
            .collect::<Vec<_>>()
            .join(" → ")
    }

    fn connection(&self) -> anyhow::Result<desktop::Connection> {
        anyhow::ensure!(!self.session.trim().is_empty(), "choose a tmux session");
        self.connection_named(self.session.trim())
    }

    fn connection_named(&self, session: &str) -> anyhow::Result<desktop::Connection> {
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
        let timeout = time::Duration::from_secs(30);
        let known_hosts = local_path(&self.known_hosts)?;
        let jumps = self
            .jumps
            .iter()
            .map(|hop| {
                Ok(ssh::Options {
                    host: hop.host.trim().to_owned(),
                    user: hop.user.trim().to_owned(),
                    port: hop.port,
                    // A hop with its own IdentityFile uses it. Otherwise it
                    // authenticates the way the user chose here, which is the
                    // only identity Starcom has been told about.
                    authentication: match hop.identity {
                        Some(ref path) => ssh::Authentication::Identity(local_path(path)?),
                        None => authentication.clone(),
                    },
                    known_hosts: match hop.known_hosts {
                        Some(ref path) => local_path(path)?,
                        None => known_hosts.clone(),
                    },
                    timeout,
                    jumps: Vec::new(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let options = ssh::Options {
            host: self.host.trim().to_owned(),
            user: self.user.trim().to_owned(),
            port: self.port,
            authentication,
            known_hosts,
            timeout,
            jumps,
        };
        options.validate()?;
        Ok(desktop::Connection {
            options,
            session: core::SessionName::new(session)?,
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
    /// Whether a local SSH agent looked reachable, and when that was last
    /// asked. A hint for the form only; the connection still authenticates as
    /// configured. Rechecked while the form is open so that starting an agent
    /// clears the warning without restarting Starcom.
    agent_available: bool,
    agent_checked: time::Instant,
    /// Set while the user is confirming that creating a session may start a
    /// tmux server on a host that has none.
    confirm_create: bool,
    /// Destination we last asked the host to list sessions for. Empty means
    /// never listed. Restored tabs copy the destination here so opening a
    /// workspace does not authenticate.
    listed_destination: String,
    /// List as soon as a host is chosen or a custom destination is committed.
    auto_list: bool,
    /// Last cell size sent to tmux, so we do not spam refresh-client -C.
    client_cells: Option<core::Size>,
    pending_client_cells: Option<(core::Size, time::Instant)>,
    refresh_tick: u64,
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
            agent_available: ssh::agent_available(),
            agent_checked: time::Instant::now(),
            confirm_create: false,
            listed_destination: String::new(),
            auto_list: false,
            client_cells: None,
            pending_client_cells: None,
            refresh_tick: 0,
        }
    }

    /// Whether an SSH agent looks reachable, re-asked at most once a second.
    ///
    /// The form is drawn every frame and the probe touches the filesystem, so
    /// it is throttled; a second of staleness on a warning label is invisible,
    /// while never re-asking would leave the warning up after the user starts
    /// an agent.
    fn agent_probe(&mut self) -> bool {
        const INTERVAL: time::Duration = time::Duration::from_secs(1);
        let now = time::Instant::now();
        if now.duration_since(self.agent_checked) >= INTERVAL {
            self.agent_available = ssh::agent_available();
            self.agent_checked = now;
        }
        self.agent_available
    }

    /// Restore a saved tab into this UI's form, showing the connection screen.
    pub(crate) fn restore(&mut self, saved: store::Tab) {
        self.form = Form::restore(saved);
        self.profile_source = self.form.destination().to_owned();
        // Restoring a tab must not contact the host. Pretend we already listed
        // so the form does not fire SSH on first paint.
        self.listed_destination = self.form.destination().to_owned();
        self.auto_list = false;
        // The saved tab holds what the user chose to connect to. How the
        // connection is routed and what policy blocks it are the config's to
        // say, and are re-read rather than restored: a saved tab must not be a
        // way to reach a host on terms the config no longer allows, or to skip
        // a bastion that has since been added.
        if !self.profile_source.is_empty()
            && let Ok(profile) = self.config.resolve(&self.profile_source)
        {
            self.form.apply_routing(profile.jumps, profile.unsupported);
        }
        self.screen = Screen::Connection;
    }

    pub(crate) fn saved(&self) -> store::Tab {
        self.form.saved()
    }

    pub fn open_terminal(&mut self) {
        self.screen = Screen::Terminal;
        self.pending_paste = None;
    }

    pub(crate) fn reset_client_size(&mut self) {
        self.client_cells = None;
        self.pending_client_cells = None;
    }

    pub fn cancel_transient(&mut self) {
        self.pending_paste = None;
        self.confirm_create = false;
    }

    pub fn refresh_profile(&mut self) {
        let destination = self.form.destination().to_owned();
        self.profile_source = destination.clone();
        self.form.unsupported.clear();
        self.form.jumps.clear();
        self.form.profile_error = None;
        if destination.is_empty() {
            self.form.host.clear();
            return;
        }
        match self.config.resolve(&destination) {
            Ok(profile) => self.form.apply_profile(
                profile,
                self.config.default_identity(),
                ssh::agent_available(),
            ),
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
        // The workspace replaces the form once a view exists, not when Connect
        // is pressed. A failed first attach has nothing to show there.
        if state.view.is_some()
            && self.screen == Screen::Connection
            && self.generation != state.generation
        {
            self.open_terminal();
        } else if self.screen == Screen::Terminal
            && state.view.is_none()
            && matches!(
                state.phase,
                desktop::Phase::Failed | desktop::Phase::Disconnected
            )
        {
            self.screen = Screen::Connection;
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
                    ui.add_space(20.0);
                    ui.heading("Connect");
                    ui.weak("One Starcom tab is one tmux session on one host.");
                });
                ui.add_space(14.0);
                ui.scope(|ui| {
                    ui.set_max_width(680.0);
                    let idle = !matches!(
                        state.phase,
                        desktop::Phase::Connecting
                            | desktop::Phase::Watching
                            | desktop::Phase::Resynchronizing
                            | desktop::Phase::Reconnecting
                    );
                    let busy = matches!(state.discovery, Some(desktop::Discovery::Running));
                    let host_ready = self.form.host_ready();
                    let listing_here = self.listed_destination == self.form.destination();

                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Host").size(18.0).strong());
                        if ui.small_button("Reload config").clicked() {
                            action = Action::ReloadConfig;
                        }
                    });
                    ui.add_space(6.0);
                    let aliases: Vec<String> =
                        self.config.aliases().iter().take(32).cloned().collect();
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                        ui.spacing_mut().button_padding = egui::vec2(16.0, 10.0);
                        if aliases.is_empty() {
                            ui.weak("No literal Host entries in ~/.ssh/config.");
                        }
                        for alias in &aliases {
                            let selected = self.form.destination() == alias;
                            let text = egui::RichText::new(alias).size(22.0).strong();
                            if ui
                                .add(
                                    egui::Button::new(text)
                                        .selected(selected)
                                        .min_size(egui::vec2(0.0, 44.0))
                                        .corner_radius(8.0),
                                )
                                .clicked()
                            {
                                if selected {
                                    self.listed_destination.clear();
                                }
                                self.form.destination = alias.clone();
                                self.refresh_profile();
                                self.auto_list = true;
                            }
                        }
                        let response = ui.add_sized(
                            egui::vec2(240.0, 44.0),
                            egui::TextEdit::singleline(&mut self.form.destination)
                                .hint_text("hostname, address, or alias"),
                        );
                        if response.changed() {
                            self.refresh_profile();
                            self.auto_list = false;
                        }
                        if response.lost_focus()
                            && (ui.input(|input| input.key_pressed(egui::Key::Enter))
                                || !self.form.destination().is_empty())
                        {
                            self.auto_list = true;
                        }
                    });

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
                            "{}@{}:{}",
                            self.form.user, self.form.host, self.form.port
                        ));
                    }
                    if !self.form.jumps.is_empty() {
                        ui.weak(format!("Via jump host: {}", self.form.route()))
                            .on_hover_text(
                                "From ProxyJump in your SSH config. Each hop verifies its \
                                 own host key and authenticates separately; a jump host \
                                 never vouches for the next one.",
                            );
                    }

                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("Session").strong());
                        if ui
                            .add_enabled(
                                host_ready && idle && !busy,
                                egui::Button::new("Refresh"),
                            )
                            .on_hover_text(
                                "Ask the host which tmux sessions exist. Uses tmux -N, so it \
                                 cannot start a server.",
                            )
                            .clicked()
                        {
                            self.listed_destination.clear();
                            self.auto_list = true;
                        }
                        match state.discovery {
                            None if self.form.destination().is_empty() => {
                                ui.weak("Pick a host to see its sessions.");
                            }
                            None => {
                                ui.weak("Sessions appear after the host is contacted.");
                            }
                            Some(desktop::Discovery::Running) => {
                                ui.weak("Asking the host…");
                            }
                            Some(desktop::Discovery::Failed(ref detail)) if listing_here => {
                                ui.colored_label(ui.visuals().error_fg_color, detail);
                            }
                            Some(desktop::Discovery::Created(ref name)) if listing_here => {
                                chosen = Some(name.clone());
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    format!("Created {name}. Connect to attach."),
                                );
                            }
                            Some(desktop::Discovery::Sessions(ref found)) if listing_here => {
                                if found.is_empty() {
                                    ui.weak("The host is running tmux with no sessions.");
                                } else {
                                    if !found
                                        .iter()
                                        .any(|summary| summary.name == self.form.session)
                                    {
                                        chosen = Some(found[0].name.clone());
                                    }
                                    for summary in found {
                                        let selected = self.form.session == summary.name;
                                        let mut text = summary.name.clone();
                                        if summary.attached > 0 {
                                            text.push_str(" · attached");
                                        }
                                        let response = ui.selectable_label(selected, text);
                                        if response.clicked() {
                                            chosen = Some(summary.name.clone());
                                        }
                                        if response.double_clicked() {
                                            chosen = Some(summary.name.clone());
                                            self.form.session = summary.name.clone();
                                            match self.form.connection() {
                                                Ok(connection) => {
                                                    self.notice = None;
                                                    action = Action::Connect(connection);
                                                }
                                                Err(error) => {
                                                    self.notice = Some(error.to_string())
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Some(_) => {
                                ui.weak("Sessions appear after the host is contacted.");
                            }
                        }
                    });

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let can_connect = host_ready && !self.form.session.trim().is_empty();
                        if ui
                            .add_enabled(
                                can_connect,
                                egui::Button::new("Connect")
                                    .min_size(egui::vec2(112.0, 28.0)),
                            )
                            .clicked()
                        {
                            match self.form.connection() {
                                Ok(connection) => {
                                    self.notice = None;
                                    action = Action::Connect(connection);
                                }
                                Err(error) => self.notice = Some(error.to_string()),
                            }
                        }
                        if ui
                            .add_enabled(host_ready && idle && !busy, egui::Button::new("Create session"))
                            .on_hover_text(
                                "Create a named session on the host. This starts a tmux \
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
                    if state.phase == desktop::Phase::Connecting {
                        ui.add_space(6.0);
                        ui.weak("Connecting…");
                    }
                    if let Some(ref error) = state.error {
                        ui.add_space(6.0);
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }

                    ui.add_space(10.0);
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
                    if self.form.authentication == Authentication::Agent && !self.agent_probe() {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "No SSH agent found. Start one and add a key, or choose a key file.",
                        );
                    }
                    if self.form.authentication == Authentication::Key {
                        field(ui, "Private key", &mut self.form.identity);
                    }
                    ui.collapsing("Advanced", |ui| {
                        field(ui, "User", &mut self.form.user);
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
        if matches!(action, Action::None) && self.auto_list {
            self.auto_list = false;
            let dest = self.form.destination().to_owned();
            let idle = !matches!(
                state.phase,
                desktop::Phase::Connecting
                    | desktop::Phase::Watching
                    | desktop::Phase::Resynchronizing
                    | desktop::Phase::Reconnecting
            );
            let busy = matches!(state.discovery, Some(desktop::Discovery::Running));
            if !dest.is_empty()
                && dest != self.listed_destination
                && self.form.host_ready()
                && idle
                && !busy
            {
                self.listed_destination = dest;
                match self.form.listing() {
                    Ok(connection) => {
                        self.notice = None;
                        action = Action::ListSessions(connection);
                    }
                    Err(error) => self.notice = Some(error.to_string()),
                }
            }
        }
        if self.confirm_create {
            let mut confirmed = false;
            egui::Window::new("Create a tmux session?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(root.ctx(), |ui| {
                    ui.label(format!("Create a session on {}?", self.form.host.trim()));
                    ui.add_space(4.0);
                    field(ui, "Session name", &mut self.form.session);
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
                        if ui
                            .add_enabled(
                                !self.form.session.trim().is_empty(),
                                egui::Button::new("Create session"),
                            )
                            .clicked()
                        {
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
            // One Starcom tab shows one tmux window. Window selection is deferred;
            // keep the current window if it still exists, otherwise the first.
            let id = self
                .window
                .filter(|id| grouped.contains_key(id))
                .or_else(|| grouped.keys().next().copied());
            if let Some(id) = id
                && let Some(panes) = grouped.get(&id)
            {
                if let Some(node) = layout::Node::from_panes(panes) {
                    self.windows.insert(id, node);
                } else {
                    self.notice = Some(format!("Cannot lay out overlapping panes in window {id}"));
                }
                self.window = Some(id);
            } else {
                self.window = None;
            }
        } else {
            self.window = None;
        }
        self.generation = state.generation;
    }

    fn show_terminal(&mut self, root: &mut egui::Ui, state: &mut desktop::State) -> Action {
        self.refresh_tick = self.refresh_tick.wrapping_add(1);
        self.rebuild_layout(state);
        // Navigation and terminal steps are separate results, so a button press
        // can never quietly consume the keystrokes collected in the same frame.
        let mut action = Action::None;
        let mut steps: Vec<Step> = Vec::new();
        let connection_epoch = state.epoch();

        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .fill(root.visuals().panel_fill),
            )
            .show_inside(root, |ui| {
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
                    // Cancelling a scheduled retry is the same operation as
                    // disconnecting: it ends this connection and keeps the last view.
                    if ui
                        .button("Exit")
                        .on_hover_text(
                            "Drop this attachment and return to the connection form. \
                         Remote jobs keep running.",
                        )
                        .clicked()
                    {
                        action = Action::Disconnect;
                        self.screen = Screen::Connection;
                        self.cancel_transient();
                    }
                    if state.access == session::Access::Interactive {
                        let mut allow = state.allow_resize;
                        if ui
                            .checkbox(&mut allow, "Resize remote panes")
                            .on_hover_text(
                                "Divider drags send resize-pane to tmux, like an ordinary \
                             terminal client. Other attached clients see the change.",
                            )
                            .changed()
                        {
                            state.allow_resize = allow;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("+")
                            .on_hover_text("Larger terminal text")
                            .clicked()
                        {
                            self.font_size = (self.font_size + 1.0).min(28.0);
                            self.reset_client_size();
                        }
                        ui.label(format!("{} pt", self.font_size as u32));
                        if ui
                            .button("−")
                            .on_hover_text("Smaller terminal text")
                            .clicked()
                        {
                            self.font_size = (self.font_size - 1.0).max(10.0);
                            self.reset_client_size();
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
                let spin = ["|", "/", "-", "\\"][(self.refresh_tick as usize) % 4];
                egui::Frame::NONE
                    .fill(ui.visuals().code_bg_color)
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(spin).monospace());
                    });
                if let Some(rtt) = state.last_rtt {
                    ui.separator();
                    ui.small(if rtt.as_millis() == 0 {
                        "<1 ms".to_owned()
                    } else {
                        format!("{} ms", rtt.as_millis())
                    });
                }
                ui.separator();
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

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(root, |ui| {
                if state.view.is_none() {
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(640.0);
                        ui.add_space(90.0);
                        ui.heading(state.phase.label());
                        if let Some(ref error) = state.error {
                            ui.add_space(8.0);
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                        ui.add_space(8.0);
                        ui.label("The connection form is kept in this tab.");
                    });
                    return;
                }
                if !matches!(state.phase, desktop::Phase::Watching | desktop::Phase::Demo) {
                    ui.label(
                        egui::RichText::new("Showing the last received view; it may be stale.")
                            .color(ui.visuals().warn_fg_color),
                    );
                }
                let rect = ui.available_rect_before_wrap();
                let (cell_width, row_height) = terminal::cell_metrics(ui, self.font_size);
                let controls = state.access == session::Access::Interactive && state.input_ready();
                if state.allow_resize && state.input_ready() {
                    let inner = rect.shrink(4.0);
                    let columns = ((inner.width() / cell_width).floor() as usize).max(1);
                    let rows = ((inner.height() / row_height).floor() as usize).max(1);
                    if let Ok(size) = core::Size::new(columns, rows)
                        && self.client_cells != Some(size)
                    {
                        let stable = match self.pending_client_cells {
                            Some((pending, at)) if pending == size => {
                                at.elapsed() >= time::Duration::from_millis(200)
                            }
                            _ => {
                                self.pending_client_cells = Some((size, time::Instant::now()));
                                false
                            }
                        };
                        if !stable {
                            ui.ctx()
                                .request_repaint_after(time::Duration::from_millis(200));
                        } else {
                            let window = self.window;
                            let pane = self.focused.or_else(|| {
                                state.view.as_ref().and_then(|view| {
                                    view.panes().values().find_map(|pane| {
                                        (Some(pane.state.window) == window)
                                            .then_some(pane.state.pane)
                                    })
                                })
                            });
                            if let Some(pane) = pane
                                && let Some(target) = state.target(pane)
                            {
                                self.client_cells = Some(size);
                                self.pending_client_cells = None;
                                steps.push(Step::Send(
                                    target,
                                    terminal_input::Action::ClientSize(size),
                                ));
                            }
                        }
                    } else {
                        self.pending_client_cells = None;
                    }
                }
                if let (Some(id), Some(view)) = (self.window, state.view.as_mut())
                    && let Some(node) = self.windows.get_mut(&id)
                {
                    let generation = self.generation;
                    let font_size = self.font_size;
                    let pane_ui = &mut self.pane_ui;
                    let focused = &mut self.focused;
                    let notice = &mut self.notice;
                    let mut resizes = Vec::new();
                    let mut pane_events = Vec::new();
                    let can_kill = view
                        .panes()
                        .values()
                        .filter(|pane| pane.state.window == id)
                        .count()
                        > 1;
                    node.draw(
                        ui,
                        rect,
                        egui::Id::new(("split", id.0, generation, connection_epoch)),
                        state.allow_resize,
                        cell_width,
                        row_height,
                        &mut resizes,
                        &mut |ui, rect, pane_id| {
                            if let Some(pane) = view.panes_mut().get_mut(&pane_id) {
                                let events = pane_ui.entry(pane_id).or_default().show(
                                    ui,
                                    rect,
                                    pane,
                                    generation,
                                    font_size,
                                    focused,
                                    notice,
                                    controls && *focused == Some(pane_id),
                                    can_kill,
                                );
                                pane_events
                                    .extend(events.into_iter().map(|action| (pane_id, action)));
                            }
                        },
                    );
                    for (pane, resize) in resizes {
                        if let Some(target) = state.target(pane) {
                            steps.push(Step::Send(target, terminal_input::Action::Resize(resize)));
                        }
                    }
                    for (pane, action) in pane_events {
                        if let Some(target) = state.target(pane) {
                            steps.push(Step::Send(target, action));
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
    fn a_host_without_identityfile_uses_a_default_key_when_no_agent() {
        let mut form = Form::default();
        form.apply_profile(
            ssh_config::Profile {
                host: "zork.example".into(),
                ..ssh_config::Profile::default()
            },
            Some(path::PathBuf::from("/home/test/.ssh/id_ed25519")),
            false,
        );
        assert_eq!(form.host, "zork.example");
        assert_eq!(form.authentication, Authentication::Key);
        assert_eq!(form.identity, "/home/test/.ssh/id_ed25519");
    }

    #[test]
    fn a_reachable_agent_is_kept_when_the_profile_has_no_identityfile() {
        let mut form = Form::default();
        form.apply_profile(
            ssh_config::Profile {
                host: "zork.example".into(),
                ..ssh_config::Profile::default()
            },
            Some(path::PathBuf::from("/home/test/.ssh/id_ed25519")),
            true,
        );
        assert_eq!(form.authentication, Authentication::Agent);
        assert_eq!(form.identity, "/home/test/.ssh/id_ed25519");
    }

    #[test]
    fn identities_only_uses_the_default_key_even_when_an_agent_is_reachable() {
        let mut form = Form::default();
        form.apply_profile(
            ssh_config::Profile {
                host: "zork.example".into(),
                identities_only: true,
                ..ssh_config::Profile::default()
            },
            Some(path::PathBuf::from("/home/test/.ssh/id_ed25519")),
            true,
        );
        assert_eq!(form.authentication, Authentication::Key);
        assert_eq!(form.identity, "/home/test/.ssh/id_ed25519");
    }

    #[test]
    fn new_ui_starts_on_connection_form() {
        let ui = DesktopUi::default();
        assert_eq!(ui.screen, Screen::Connection);
    }

    fn paint(ui: &mut DesktopUi, state: &mut desktop::State) -> Action {
        let ctx = egui::Context::default();
        crate::window::configure(&ctx);
        let mut action = Action::None;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 720.0),
                )),
                ..Default::default()
            },
            |root| {
                action = ui.show(root, state);
            },
        );
        action
    }

    #[test]
    fn choosing_a_host_lists_its_sessions() {
        let config = sync::Arc::new(ssh_config::Config::from_text(
            "Host zork\nHostName 10.0.0.2\nUser alice\n",
        ));
        let mut ui = DesktopUi::with_config(config, None);
        ui.form.destination = "zork".to_owned();
        ui.refresh_profile();
        ui.auto_list = true;
        let mut state = desktop::State::default();
        assert!(matches!(
            paint(&mut ui, &mut state),
            Action::ListSessions(_)
        ));
        assert_eq!(ui.listed_destination, "zork");
    }

    #[test]
    fn a_session_list_selects_the_first_name() {
        let mut ui = DesktopUi::default();
        ui.form.destination = "zork".to_owned();
        ui.listed_destination = "zork".to_owned();
        let mut state = desktop::State::default();
        state.discovery = Some(desktop::Discovery::Sessions(vec![
            crate::sessions::Summary {
                name: "0".into(),
                windows: 1,
                attached: 0,
            },
            crate::sessions::Summary {
                name: "work".into(),
                windows: 2,
                attached: 1,
            },
        ]));
        paint(&mut ui, &mut state);
        assert_eq!(ui.form.session, "0");
    }

    #[test]
    fn restored_tabs_do_not_list_on_first_paint() {
        let mut ui = DesktopUi::default();
        ui.restore(store::Tab {
            destination: "zork".into(),
            host: "zork.example".into(),
            user: "alice".into(),
            session: "work".into(),
            port: 22,
            ..store::Tab::default()
        });
        assert!(!ui.auto_list);
        assert_eq!(ui.listed_destination, "zork");
        let mut state = desktop::State::default();
        assert!(matches!(paint(&mut ui, &mut state), Action::None));
    }

    #[test]
    fn a_failed_first_attach_returns_to_the_form() {
        let mut ui = DesktopUi::default();
        ui.open_terminal();
        let mut state = desktop::State::default();
        state.phase = desktop::Phase::Failed;
        state.error = Some(
            "Authentication failed. Starcom will not retry it. SSH Authentication: no SSH agent is available"
                .into(),
        );
        paint(&mut ui, &mut state);
        assert_eq!(ui.screen, Screen::Connection);
    }

    #[test]
    fn a_published_view_replaces_the_form_once() {
        let mut ui = DesktopUi::default();
        let mut state = desktop::State::interactive_demo().unwrap();
        paint(&mut ui, &mut state);
        assert_eq!(ui.screen, Screen::Terminal);
        ui.screen = Screen::Connection;
        paint(&mut ui, &mut state);
        assert_eq!(
            ui.screen,
            Screen::Connection,
            "Connection settings must not be yanked back to the workspace"
        );
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
