//! Small desktop chrome around the terminal grids. No networking or remote writes.

mod layout;
mod terminal;

use std::{collections, env, path, time};

use crate::{core, desktop, snapshot, ssh};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authentication {
    Agent,
    Key,
}

pub struct Form {
    host: String,
    pub user: String,
    session: String,
    port: u16,
    authentication: Authentication,
    identity: String,
    known_hosts: String,
    socket: String,
    history: usize,
}

impl Default for Form {
    fn default() -> Self {
        Self {
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
        }
    }
}

impl Form {
    fn connection(&self) -> anyhow::Result<desktop::Connection> {
        fn local_path(text: &str) -> anyhow::Result<path::PathBuf> {
            if let Some(tail) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
                return desktop::home_path()
                    .map(|home| home.join(tail))
                    .ok_or_else(|| anyhow::anyhow!("home directory is unavailable"));
            }
            Ok(path::PathBuf::from(text))
        }
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
        })
    }
}

pub enum Action {
    None,
    Connect(desktop::Connection),
    Disconnect,
    Demo,
}

pub struct DesktopUi {
    pub form: Form,
    font_size: f32,
    generation: u64,
    windows: collections::BTreeMap<tmuxctl::WindowId, layout::Node>,
    window: Option<tmuxctl::WindowId>,
    focused: Option<tmuxctl::PaneId>,
    pane_ui: collections::BTreeMap<tmuxctl::PaneId, terminal::PaneUi>,
    notice: Option<String>,
}

impl Default for DesktopUi {
    fn default() -> Self {
        Self {
            form: Form::default(),
            font_size: 14.0,
            generation: u64::MAX,
            windows: collections::BTreeMap::new(),
            window: None,
            focused: None,
            pane_ui: collections::BTreeMap::new(),
            notice: None,
        }
    }
}

impl DesktopUi {
    pub fn show(&mut self, root: &mut egui::Ui, state: &mut desktop::State) -> Action {
        let mut action = Action::None;
        if self.generation != state.generation {
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
                        self.notice =
                            Some(format!("Cannot lay out overlapping panes in window {id}"));
                    }
                }
            }
            if self.window.is_none_or(|id| !self.windows.contains_key(&id)) {
                self.window = self.windows.keys().next().copied();
            }
            self.generation = state.generation;
        }
        egui::Panel::top("toolbar").show_inside(root, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Starcom").strong().size(19.0));
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("READ-ONLY")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(12.0);
                ui.label(state.phase.label());
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
                    ui.separator();
                    if ui.button("Copy selection").clicked() {
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
                            Some(text) => terminal::copy(ui.ctx(), text, &mut self.notice),
                            None => self.notice = Some("Select text in a pane first.".to_owned()),
                        }
                    }
                });
            });
        });
        egui::Panel::bottom("status").show_inside(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.small(if state.phase == desktop::Phase::Demo {
                    "Local demo — no SSH connection"
                } else {
                    "Wheel: scroll  ·  Drag: select  ·  Right-click: copy"
                });
                ui.separator();
                ui.small("Dividers resize local views, not remote panes.");
                if let Some(ref notice) = self.notice {
                    ui.separator();
                    ui.small(notice);
                }
            });
        });
        egui::Panel::left("connection").default_size(230.0).size_range(205.0..=350.0).show_inside(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading("Connection");
                ui.add_space(6.0);
                field(ui, "Host", &mut self.form.host);
                field(ui, "User", &mut self.form.user);
                field(ui, "tmux session", &mut self.form.session);
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.form.authentication, Authentication::Agent, "SSH agent");
                    ui.radio_value(&mut self.form.authentication, Authentication::Key, "Key file");
                });
                if self.form.authentication == Authentication::Key { field(ui, "Private key", &mut self.form.identity); }
                ui.add_space(5.0);
                ui.collapsing("SSH options", |ui| {
                    field(ui, "Known hosts", &mut self.form.known_hosts);
                    field(ui, "Remote tmux socket (optional)", &mut self.form.socket);
                    ui.horizontal(|ui| { ui.label("Port"); ui.add(egui::DragValue::new(&mut self.form.port).range(1..=65535)); });
                    ui.horizontal(|ui| { ui.label("History lines"); ui.add(egui::DragValue::new(&mut self.form.history).range(0..=snapshot::MAX_HISTORY_LINES)); });
                    ui.small("Explicit settings only; SSH config aliases and jump hosts are not supported yet.");
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        match self.form.connection() {
                            Ok(connection) => { self.notice = None; action = Action::Connect(connection); }
                            Err(error) => self.notice = Some(error.to_string()),
                        }
                    }
                    let active = matches!(state.phase, desktop::Phase::Connecting | desktop::Phase::Watching | desktop::Phase::Resynchronizing);
                    if ui.add_enabled(active, egui::Button::new("Disconnect")).clicked() { action = Action::Disconnect; }
                });
                ui.add_space(8.0);
                ui.small("Uses the host's existing SSH and tmux. Nothing is installed remotely.");
                ui.add_space(4.0);
                ui.small("Host keys must already be trusted. Connect again to retry; no automatic reconnect yet.");
                if let Some(ref error) = state.error {
                    ui.separator();
                    ui.label(egui::RichText::new(error).color(ui.visuals().error_fg_color));
                }
                ui.add_space(18.0);
                ui.separator();
                if ui.button("Open local demo").clicked() { action = Action::Demo; }
                ui.small("Try selection and scrolling without a remote connection.");
            });
        });
        egui::CentralPanel::default().show_inside(root, |ui| {
            if state.view.is_none() {
                ui.vertical_centered(|ui| {
                    ui.add_space(90.0);
                    ui.heading("Your sessions, without the tmux shortcuts.");
                    ui.add_space(12.0);
                    ui.label("Connect to an existing session, or open the local demo.");
                    ui.add_space(6.0);
                    ui.weak("This first desktop version is read-only.");
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
                node.draw(
                    ui,
                    rect,
                    egui::Id::new(("split", id.0, generation)),
                    &mut |ui, rect, pane_id| {
                        if let Some(pane) = view.panes_mut().get_mut(&pane_id) {
                            pane_ui
                                .entry(pane_id)
                                .or_default()
                                .show(ui, rect, pane, generation, font_size, focused, notice);
                        }
                    },
                );
            }
        });
        action
    }
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .char_limit(1024),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_connection_form_does_not_start_networking() {
        let form = Form::default();
        assert!(form.connection().is_err());
    }

    #[test]
    fn form_requires_key_path_when_selected() {
        let form = Form {
            host: "localhost".to_owned(),
            user: "demo".to_owned(),
            authentication: Authentication::Key,
            known_hosts: "known_hosts".to_owned(),
            ..Form::default()
        };
        assert!(form.connection().is_err());
    }
    fn fixture() -> (egui::Context, DesktopUi, desktop::State) {
        let ctx = egui::Context::default();
        crate::window::configure(&ctx);
        let mut state = desktop::State::default();
        state.view = Some(desktop::demo_view().unwrap());
        state.generation = 1;
        state.phase = desktop::Phase::Demo;
        let mut ui = DesktopUi::default();
        for step in 0..3 {
            frame(&ctx, &mut ui, &mut state, step, Vec::new());
        }
        (ctx, ui, state)
    }

    fn frame(
        ctx: &egui::Context,
        ui: &mut DesktopUi,
        state: &mut desktop::State,
        step: u32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 760.0));
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(f64::from(step) / 30.0),
                events,
                ..Default::default()
            },
            |root| {
                ui.show(root, state);
            },
        )
    }

    fn button(position: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn actual_drag_selection_reaches_the_clipboard_without_remote_input() {
        let (ctx, mut ui, mut state) = fixture();
        let id = tmuxctl::PaneId(0);
        let content = ctx
            .read_response(egui::Id::new(("terminal", 1u64, id.0)))
            .unwrap()
            .rect;
        let font = egui::FontId::monospace(ui.font_size);
        let (width, height) = ctx.fonts_mut(|fonts| {
            (
                fonts.glyph_width(&font, 'M'),
                fonts.row_height(&font).ceil() + 2.0,
            )
        });
        let start = content.min + egui::vec2(width * 0.1, height * 0.5);
        let end = content.min + egui::vec2(width * 6.8, height * 0.5);
        frame(
            &ctx,
            &mut ui,
            &mut state,
            3,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        frame(
            &ctx,
            &mut ui,
            &mut state,
            4,
            vec![egui::Event::PointerMoved(end)],
        );
        frame(&ctx, &mut ui, &mut state, 5, vec![button(end, false)]);
        assert_eq!(ui.focused, Some(id));
        let text = state.view.as_ref().unwrap().panes()[&id]
            .terminal
            .selected_text();
        assert_eq!(text.as_deref(), Some("Starcom"));
        let output = frame(&ctx, &mut ui, &mut state, 6, vec![egui::Event::Copy]);
        assert!(
            output
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text == "Starcom"))
        );
        assert_eq!(state.phase, desktop::Phase::Demo);
    }

    #[test]
    fn drawing_only_visits_visible_history_rows() {
        let (ctx, mut ui, mut state) = fixture();
        let id = tmuxctl::PaneId(1);
        let pane = state
            .view
            .as_mut()
            .unwrap()
            .panes_mut()
            .get_mut(&id)
            .unwrap();
        for _ in 0..1000 {
            pane.terminal.feed(b"still producing output\r\n");
        }
        frame(&ctx, &mut ui, &mut state, 3, Vec::new());
        let painted = ui.pane_ui[&id].painted_rows;
        assert!(
            (1..45).contains(&painted),
            "visited {painted} rows instead of the viewport"
        );
        assert!(
            alacritty_terminal::grid::Dimensions::total_lines(
                state.view.as_ref().unwrap().panes()[&id]
                    .terminal
                    .model()
                    .grid()
            ) > painted * 3
        );
    }

    #[test]
    fn divider_drag_changes_local_viewports_not_remote_geometry() {
        let (ctx, mut ui, mut state) = fixture();
        let first = tmuxctl::PaneId(0);
        let second = tmuxctl::PaneId(1);
        let before = ui.pane_ui[&first].rect.width();
        let remote = state.view.as_ref().unwrap().panes()[&first].state.size;
        let id = egui::Id::new(("split", tmuxctl::WindowId(0).0, 1u64));
        let start = ctx.read_response(id).unwrap().rect.center();
        let end = start + egui::vec2(-100.0, 0.0);
        frame(
            &ctx,
            &mut ui,
            &mut state,
            3,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        frame(
            &ctx,
            &mut ui,
            &mut state,
            4,
            vec![egui::Event::PointerMoved(end)],
        );
        frame(&ctx, &mut ui, &mut state, 5, vec![button(end, false)]);
        frame(&ctx, &mut ui, &mut state, 6, Vec::new());
        assert!(ui.pane_ui[&first].rect.width() < before - 70.0);
        assert!(ui.pane_ui[&first].rect.right() < ui.pane_ui[&second].rect.left());
        assert_eq!(
            state.view.as_ref().unwrap().panes()[&first].state.size,
            remote
        );
    }

    #[test]
    fn typing_and_pasting_in_a_terminal_do_not_mutate_read_only_models() {
        let (ctx, mut ui, mut state) = fixture();
        let id = tmuxctl::PaneId(0);
        let before = state.view.as_ref().unwrap().panes()[&id]
            .terminal
            .screen_lines();
        let point = ui.pane_ui[&id].rect.center();
        frame(
            &ctx,
            &mut ui,
            &mut state,
            3,
            vec![egui::Event::PointerMoved(point), button(point, true)],
        );
        frame(&ctx, &mut ui, &mut state, 4, vec![button(point, false)]);
        frame(
            &ctx,
            &mut ui,
            &mut state,
            5,
            vec![
                egui::Event::Text("rm -rf /".into()),
                egui::Event::Paste("echo unsafe\n".into()),
            ],
        );
        assert_eq!(
            before,
            state.view.as_ref().unwrap().panes()[&id]
                .terminal
                .screen_lines()
        );
    }
}
