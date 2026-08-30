//! Connection tabs own independent clients, forms, selection and input tokens.
//! A new tab displays a form, never a sidebar alongside somebody else's panes.

use std::sync;

use crate::{desktop, ssh_config, ui};

const MAX_TABS: usize = 16;
type Wake = sync::Arc<dyn Fn() + Send + Sync>;

struct Tab {
    id: u64,
    label: String,
    client: desktop::Client,
    ui: ui::DesktopUi,
}

pub(crate) enum Action {
    None,
    New,
    Select(u64),
    Close(u64),
    Tab(u64, ui::Action),
}

pub(crate) struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    next: u64,
    wake: Wake,
    config: sync::Arc<ssh_config::Config>,
    config_error: Option<String>,
    notice: Option<String>,
}

impl Workspace {
    pub fn new(wake: Wake, startup: desktop::Startup) -> anyhow::Result<Self> {
        let mut workspace = Self {
            tabs: Vec::new(),
            active: 0,
            next: 1,
            wake,
            config: sync::Arc::new(ssh_config::Config::default()),
            config_error: None,
            notice: None,
        };
        if startup != desktop::Startup::Demo {
            workspace.reload_config();
        }
        workspace.new_tab()?;
        if startup == desktop::Startup::Demo {
            workspace.tabs[0].client.demo()?;
            workspace.tabs[0].label = "Demo".into();
            workspace.tabs[0].ui.open_terminal();
        }
        Ok(workspace)
    }

    fn new_tab(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.tabs.len() < MAX_TABS,
            "at most {MAX_TABS} connection tabs may be open"
        );
        self.cancel_transient();
        let id = self.next;
        self.next = self.next.checked_add(1).expect("tab identity exhausted");
        self.tabs.push(Tab {
            id,
            label: "New connection".into(),
            client: desktop::Client::new(sync::Arc::clone(&self.wake))?,
            ui: ui::DesktopUi::with_config(
                sync::Arc::clone(&self.config),
                self.config_error.clone(),
            ),
        });
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    fn reload_config(&mut self) {
        match desktop::home_path().map_or_else(
            || Err(anyhow::anyhow!("home directory is unavailable")),
            |home| ssh_config::Config::load(&home),
        ) {
            Ok(config) => {
                self.config = sync::Arc::new(config);
                self.config_error = None;
            }
            Err(error) => {
                self.config_error = Some(format!("Could not load SSH config: {error:#}"));
            }
        }
        for tab in &mut self.tabs {
            tab.ui.config = sync::Arc::clone(&self.config);
            tab.ui.config_load_error = self.config_error.clone();
            tab.ui.refresh_profile();
        }
    }

    fn cancel_transient(&mut self) {
        for tab in &mut self.tabs {
            tab.ui.cancel_transient();
        }
    }

    pub fn terminal_focused(&self, ctx: &egui::Context) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|tab| tab.ui.terminal_focused(ctx))
    }

    pub fn show(&mut self, root: &mut egui::Ui) -> Action {
        let mut navigation = Action::None;
        let new = egui::KeyboardShortcut::new(
            if cfg!(target_os = "macos") {
                egui::Modifiers::MAC_CMD
            } else {
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT
            },
            egui::Key::T,
        );
        let close = egui::KeyboardShortcut::new(new.modifiers, egui::Key::W);
        if root.input_mut(|input| input.consume_shortcut(&new)) {
            navigation = Action::New;
        }
        if root.input_mut(|input| input.consume_shortcut(&close))
            && let Some(tab) = self.tabs.get(self.active)
        {
            navigation = Action::Close(tab.id);
        }
        egui::Panel::top("connection-tabs").show_inside(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Starcom").strong());
                ui.separator();
                for (index, tab) in self.tabs.iter().enumerate() {
                    ui.push_id(tab.id, |ui| {
                        if ui
                            .selectable_label(index == self.active, &tab.label)
                            .clicked()
                        {
                            navigation = Action::Select(tab.id);
                        }
                        if ui
                            .small_button("×")
                            .on_hover_text("Close this connection tab; remote jobs keep running")
                            .clicked()
                        {
                            navigation = Action::Close(tab.id);
                        }
                    });
                }
                if ui
                    .add_enabled(self.tabs.len() < MAX_TABS, egui::Button::new("+"))
                    .on_hover_text("New connection tab")
                    .clicked()
                {
                    navigation = Action::New;
                }
                if let Some(ref notice) = self.notice {
                    ui.colored_label(ui.visuals().error_fg_color, notice);
                }
            });
        });
        if !matches!(navigation, Action::None) {
            // Switching tabs consumes the whole navigation frame. Keyboard and
            // clipboard events collected for the old tab cannot hit the new one.
            return navigation;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return Action::New;
        };
        let action = root
            .push_id(tab.id, |root| tab.ui.show(root, &mut tab.client.lock()))
            .inner;
        Action::Tab(tab.id, action)
    }

    /// Apply once, after egui finished its potentially repeated layout passes.
    pub fn apply(&mut self, action: Action, mut clipboard: impl FnMut() -> Option<String>) {
        if matches!(action, Action::None | Action::Tab(_, ui::Action::None)) {
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            match action {
                Action::None => {}
                Action::New => self.new_tab()?,
                Action::Select(id) => {
                    if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                        self.cancel_transient();
                        self.active = index;
                    }
                }
                Action::Close(id) => {
                    if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                        self.cancel_transient();
                        self.tabs.remove(index); // Client::drop invalidates tokens and wakes its worker.
                        if index < self.active {
                            self.active -= 1;
                        }
                        self.active = self.active.min(self.tabs.len().saturating_sub(1));
                        if self.tabs.is_empty() {
                            self.new_tab()?;
                        }
                    }
                }
                Action::Tab(id, action) => {
                    let Some(tab) = self.tabs.get_mut(self.active).filter(|tab| tab.id == id)
                    else {
                        return Ok(());
                    };
                    let result = match action {
                        ui::Action::None => Ok(()),
                        ui::Action::Connect(connection) => {
                            let label = format!(
                                "{} / {}",
                                tab.ui.form.destination(),
                                connection.session.as_str()
                            );
                            tab.client.connect(connection).map(|()| {
                                tab.label = label;
                                tab.ui.open_terminal();
                            })
                        }
                        ui::Action::Demo => tab.client.demo().map(|()| {
                            tab.label = "Demo".into();
                            tab.ui.open_terminal();
                        }),
                        ui::Action::Disconnect => {
                            tab.client.disconnect();
                            Ok(())
                        }
                        // Resolve clipboard reads in place so the whole frame
                        // still reaches the worker as one ordered, atomic batch.
                        ui::Action::Frame(steps) => {
                            let mut actions = Vec::with_capacity(steps.len());
                            for step in steps {
                                match step {
                                    ui::Step::Send(target, action) => {
                                        actions.push((target, action))
                                    }
                                    ui::Step::RequestPaste(target) => {
                                        if let Some(text) = clipboard()
                                            && let Some(action) = tab.ui.clipboard_paste(
                                                &tab.client.lock(),
                                                target,
                                                &text,
                                            )
                                        {
                                            actions.push((target, action));
                                        }
                                    }
                                }
                            }
                            if actions.is_empty() {
                                Ok(())
                            } else {
                                tab.client.submit_batch(actions)
                            }
                        }
                        ui::Action::ReloadConfig => {
                            self.reload_config();
                            return Ok(());
                        }
                    };
                    if let Err(error) = result {
                        tab.client.lock().error = Some(error.to_string());
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.notice = Some(error.to_string());
        }
        (self.wake)();
    }

    pub fn shutdown(&mut self) {
        self.cancel_transient();
        self.tabs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_tab_preserves_existing_session_and_close_only_detaches_its_client() {
        let mut workspace = Workspace::new(sync::Arc::new(|| {}), desktop::Startup::Demo).unwrap();
        let original = workspace.tabs[0].id;
        workspace.new_tab().unwrap();
        assert_eq!(workspace.tabs.len(), 2);
        assert_eq!(workspace.tabs[0].client.phase(), desktop::Phase::Demo);
        assert_eq!(workspace.tabs[1].client.phase(), desktop::Phase::Idle);
        workspace.apply(Action::Close(workspace.tabs[1].id), || None);
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(workspace.tabs[0].id, original);
        assert_eq!(workspace.tabs[0].client.phase(), desktop::Phase::Demo);
        workspace.shutdown();
        workspace.shutdown();
        assert!(workspace.tabs.is_empty());
    }
    #[test]
    fn stale_tab_actions_do_not_modify_the_current_tab() {
        let mut workspace = Workspace::new(sync::Arc::new(|| {}), desktop::Startup::Demo).unwrap();
        let original = workspace.tabs[0].id;
        workspace.new_tab().unwrap();
        workspace.apply(Action::Tab(original, ui::Action::Disconnect), || None);
        assert_eq!(workspace.tabs[0].client.phase(), desktop::Phase::Demo);
        assert_eq!(workspace.tabs[1].client.phase(), desktop::Phase::Idle);
    }
    #[test]
    fn connections_and_tabs_are_bounded() {
        let mut workspace = Workspace::new(sync::Arc::new(|| {}), desktop::Startup::Demo).unwrap();
        for _ in 1..MAX_TABS {
            workspace.new_tab().unwrap();
        }
        assert!(workspace.new_tab().is_err());
    }
    #[test]
    fn native_smoke_geometry() {
        let ctx = egui::Context::default();
        crate::window::configure(&ctx);
        let mut workspace = Workspace::new(sync::Arc::new(|| {}), desktop::Startup::Demo).unwrap();
        for pass in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 760.0),
                )),
                time: Some(pass as f64 / 60.0),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |root| {
                workspace.show(root);
            });
        }
        let (start, end) = workspace.tabs[0].ui.smoke_selection(&ctx);
        assert!(start.x > 0.0 && start.x < end.x);
        if let Some(path) = std::env::var_os("STARCOM_SMOKE_GEOMETRY") {
            std::fs::write(
                path,
                format!(
                    "{{\"start\":[{},{}],\"end\":[{},{}]}}",
                    start.x, start.y, end.x, end.y
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn idle_frames_do_not_request_another_repaint() {
        let count = sync::Arc::new(sync::atomic::AtomicUsize::new(0));
        let wake = sync::Arc::clone(&count);
        let mut workspace = Workspace::new(
            sync::Arc::new(move || {
                wake.fetch_add(1, sync::atomic::Ordering::Relaxed);
            }),
            desktop::Startup::Demo,
        )
        .unwrap();
        let before = count.load(sync::atomic::Ordering::Relaxed);
        workspace.apply(Action::Tab(workspace.tabs[0].id, ui::Action::None), || None);
        assert_eq!(count.load(sync::atomic::Ordering::Relaxed), before);
    }
}
