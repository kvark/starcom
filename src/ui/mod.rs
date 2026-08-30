//! Connection form and terminal workspace for one connection tab.

pub(crate) mod input;
mod layout;
mod terminal;

use std::{collections, env, path, sync, time};

use crate::{core, desktop, input as terminal_input, session, snapshot, ssh, ssh_config};

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
            unsupported: Vec::new(),
            profile_error: None,
        }
    }
}

impl Form {
    pub fn destination(&self) -> &str {
        self.destination.trim()
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
        })
    }
}

pub enum Action {
    None,
    Connect(desktop::Connection),
    Disconnect,
    Demo,
    Send(Vec<(desktop::Target, terminal_input::Action)>),
    RequestPaste(desktop::Target),
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
        }
    }

    pub fn open_terminal(&mut self) {
        self.screen = Screen::Terminal;
        self.pending_paste = None;
    }

    pub fn cancel_transient(&mut self) {
        self.pending_paste = None;
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

    pub fn clipboard_paste(
        &mut self,
        state: &desktop::State,
        target: desktop::Target,
        text: &str,
    ) -> Action {
        if state.target(target.pane()) != Some(target) {
            self.notice = Some("Paste target changed; nothing was sent.".to_owned());
            return Action::None;
        }
        match terminal_input::Paste::new(text) {
            Ok(paste) if paste.is_multiline() => {
                self.pending_paste = Some((target, paste));
                Action::None
            }
            Ok(paste) => Action::Send(vec![(target, terminal_input::Action::Paste(paste))]),
            Err(error) => {
                self.notice = Some(error.to_string());
                Action::None
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
                    if let Some(ref notice) = self.notice {
                        ui.colored_label(ui.visuals().warn_fg_color, notice);
                    }
                    ui.add_space(8.0);
                    ui.weak("The remote host needs only stock SSH and tmux. Host keys are never accepted automatically.");
                });
            });
        });
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
        let mut action = Action::None;
        let mut outgoing = Vec::new();

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
                if ui
                    .add_enabled(
                        matches!(
                            state.phase,
                            desktop::Phase::Connecting
                                | desktop::Phase::Watching
                                | desktop::Phase::Resynchronizing
                        ),
                        egui::Button::new("Disconnect"),
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
                        action = Action::RequestPaste(target);
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
                    egui::Id::new(("split", id.0, generation)),
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
                        outgoing.push((target, terminal_input::Action::Resize(resize)));
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
                            outgoing.extend(actions.into_iter().map(|action| (target, action)));
                        }
                        Ok(Some(input::Event::Copy)) => self.copy_selection(root.ctx(), state),
                        Ok(Some(input::Event::Paste(paste))) => {
                            if paste.is_multiline() {
                                self.pending_paste = Some((target, paste));
                            } else {
                                outgoing.push((target, terminal_input::Action::Paste(paste)));
                            }
                        }
                        Ok(Some(input::Event::RequestPaste)) => {
                            if outgoing.is_empty() {
                                action = Action::RequestPaste(target);
                            }
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
                outgoing.push((target, terminal_input::Action::Paste(paste)));
            }
        }

        if matches!(action, Action::None) && !outgoing.is_empty() {
            Action::Send(outgoing)
        } else {
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
}
