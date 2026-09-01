//! Connection tabs own independent clients, forms, selection and input tokens.
//! A new tab displays a form, never a sidebar alongside somebody else's panes.

use std::{fs, io, path, sync, time};

use anyhow::Context;

use crate::{desktop, dialog, reconnect, ssh_config, store, ui};

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
    Tab(u64, Box<ui::Action>),
}

pub(crate) struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    next: u64,
    wake: Wake,
    config: sync::Arc<ssh_config::Config>,
    config_error: Option<String>,
    notice: Option<String>,
    /// Where saved tabs live. None disables persistence entirely, which is what
    /// happens with no home directory and in the demo.
    store: Option<path::PathBuf>,
    fps: u32,
    /// GUI-side copy of the last event-loop clock, used to notice a machine
    /// sleep while the SSH worker is blocked in poll.
    suspend_clock: reconnect::AliveClock,
}

/// A restored tab is labelled by where it points, not by a live connection.
fn label(tab: &store::Tab) -> String {
    let destination = if tab.destination.trim().is_empty() {
        tab.host.trim()
    } else {
        tab.destination.trim()
    };
    match (destination.is_empty(), tab.session.trim()) {
        (true, "") => "New connection".to_owned(),
        (true, session) => session.to_owned(),
        (false, "") => destination.to_owned(),
        (false, session) => format!("{destination} / {session}"),
    }
}

impl Workspace {
    pub fn new(wake: Wake, startup: desktop::Startup) -> anyhow::Result<Self> {
        Self::try_new(wake, startup, |_, _| dialog::BrokenStore::Exit)?
            .ok_or_else(|| anyhow::anyhow!("saved tabs were unreadable"))
    }

    /// Open a workspace, asking with a system dialog if saved tabs cannot be
    /// read. `None` means the user chose to exit before the window opened.
    pub(crate) fn launch(wake: Wake, startup: desktop::Startup) -> anyhow::Result<Option<Self>> {
        Self::try_new(wake, startup, dialog::ask_clear_or_exit)
    }

    fn try_new(
        wake: Wake,
        startup: desktop::Startup,
        on_broken: impl Fn(&path::Path, &anyhow::Error) -> dialog::BrokenStore,
    ) -> anyhow::Result<Option<Self>> {
        let mut workspace = Self {
            tabs: Vec::new(),
            active: 0,
            next: 1,
            wake,
            config: sync::Arc::new(ssh_config::Config::default()),
            config_error: None,
            notice: None,
            // The demo must not read or overwrite a real saved workspace.
            store: (startup != desktop::Startup::Demo)
                .then(desktop::home_path)
                .flatten()
                .map(|home| store::path(&home)),
            fps: store::DEFAULT_FPS,
            suspend_clock: reconnect::AliveClock::now(),
        };
        if startup != desktop::Startup::Demo {
            workspace.reload_config();
            if !workspace.restore(on_broken)? {
                return Ok(None);
            }
        }
        if workspace.tabs.is_empty() {
            workspace.new_tab()?;
        }
        if startup == desktop::Startup::Demo {
            workspace.tabs[0].client.demo()?;
            workspace.tabs[0].label = "Demo".into();
            workspace.tabs[0].ui.open_terminal();
        }
        Ok(Some(workspace))
    }

    /// Reopen saved tabs on their connection forms. Restoring never connects and
    /// never authenticates: the user presses Connect, exactly as on a cold start.
    /// `Ok(false)` means the user chose to exit and the file was left alone.
    fn restore(
        &mut self,
        on_broken: impl Fn(&path::Path, &anyhow::Error) -> dialog::BrokenStore,
    ) -> anyhow::Result<bool> {
        let Some(file) = self.store.clone() else {
            return Ok(true);
        };
        let saved = match store::load(&file) {
            Ok(Some(saved)) => saved,
            Ok(None) => return Ok(true),
            Err(error) => match on_broken(&file, &error) {
                dialog::BrokenStore::Exit => {
                    // Disable saving so a later persist cannot overwrite a file
                    // we refused to clear.
                    self.store = None;
                    return Ok(false);
                }
                dialog::BrokenStore::Clear => {
                    match fs::remove_file(&file) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(error).context(format!("clear {}", file.display()));
                        }
                    }
                    return Ok(true);
                }
            },
        };
        for tab in saved.tabs {
            if self.new_tab().is_err() {
                break;
            }
            let index = self.tabs.len() - 1;
            self.tabs[index].label = label(&tab);
            self.tabs[index].ui.restore(tab);
        }
        self.active = saved.active.min(self.tabs.len().saturating_sub(1));
        self.fps = store::clamp_fps(saved.fps);
        Ok(true)
    }

    pub(crate) fn repaint_interval(&self) -> time::Duration {
        time::Duration::from_secs_f64(1.0 / f64::from(store::clamp_fps(self.fps)))
    }

    /// Persist after a change to which tabs exist or where they point. Failure
    /// is reported once and then disables saving, rather than repeating on
    /// every action.
    fn persist(&mut self) {
        let Some(ref file) = self.store else { return };
        let saved = store::Workspace {
            tabs: self
                .tabs
                .iter()
                .take(store::MAX_TABS)
                .map(|tab| tab.ui.saved())
                .collect(),
            active: self.active,
            fps: store::clamp_fps(self.fps),
        };
        if let Err(error) = store::save(file, &saved) {
            self.notice = Some(format!("Could not save tabs: {error:#}"));
            self.store = None;
        }
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
                    });
                }
                if ui
                    .add_enabled(self.tabs.len() < MAX_TABS, egui::Button::new("+"))
                    .on_hover_text("New connection tab")
                    .clicked()
                {
                    navigation = Action::New;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut fps = self.fps;
                    let response = ui
                        .add(
                            egui::DragValue::new(&mut fps)
                                .range(1..=i64::from(store::MAX_FPS))
                                .suffix(" fps"),
                        )
                        .on_hover_text(
                            "Maximum redraw rate for live output. Pointer and key events \
                             still paint immediately.",
                        );
                    if response.changed() {
                        self.fps = store::clamp_fps(fps);
                        self.persist();
                    }
                });
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
        Action::Tab(tab.id, Box::new(action))
    }

    /// Apply once, after egui finished its potentially repeated layout passes.
    pub fn apply(&mut self, action: Action, mut clipboard: impl FnMut() -> Option<String>) {
        let nothing_to_do = match &action {
            Action::None => true,
            Action::Tab(_, tab_action) => matches!(**tab_action, ui::Action::None),
            _ => false,
        };
        if nothing_to_do {
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            match action {
                Action::None => {}
                Action::New => {
                    self.new_tab()?;
                    self.persist();
                }
                Action::Select(id) => {
                    if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                        self.cancel_transient();
                        self.active = index;
                        self.persist();
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
                        self.persist();
                    }
                }
                Action::Tab(id, action) => {
                    let mut save = false;
                    let Some(tab) = self.tabs.get_mut(self.active).filter(|tab| tab.id == id)
                    else {
                        return Ok(());
                    };
                    let result = match *action {
                        ui::Action::None => Ok(()),
                        ui::Action::Connect(connection) => {
                            let label = format!(
                                "{} / {}",
                                tab.ui.form.destination(),
                                connection.session.as_str()
                            );
                            let started = tab.client.connect(connection).map(|()| {
                                tab.label = label;
                                tab.ui.reset_client_size();
                            });
                            // Remember where a successful connection pointed, so
                            // the next start reopens the same form.
                            save = started.is_ok();
                            started
                        }
                        ui::Action::Demo => tab.client.demo().map(|()| {
                            tab.label = "Demo".into();
                            tab.ui.open_terminal();
                        }),
                        ui::Action::ListSessions(connection) => {
                            tab.client.list_sessions(connection)
                        }
                        ui::Action::CreateSession(connection) => {
                            // A creation size only sets the new session's initial
                            // geometry; tmux owns it from then on.
                            tab.client
                                .create_session(connection, crate::core::Size::default())
                        }
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
                    if save {
                        self.persist();
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

    /// If wall time jumped while this process's monotonic clock did not, the
    /// machine slept. Wake SSH workers so they can treat the control stream as
    /// lost instead of sitting on "Connected" until the next keystroke.
    pub(crate) fn notice_suspend(&mut self) {
        if !self.suspend_clock.suspended() {
            return;
        }
        self.suspend_clock = reconnect::AliveClock::now();
        for tab in &self.tabs {
            tab.client.nudge();
        }
    }

    pub fn shutdown(&mut self) {
        self.cancel_transient();
        // Save before dropping the tabs: the forms are the thing being saved.
        self.persist();
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
        workspace.apply(
            Action::Tab(original, Box::new(ui::Action::Disconnect)),
            || None,
        );
        assert_eq!(workspace.tabs[0].client.phase(), desktop::Phase::Demo);
        assert_eq!(workspace.tabs[1].client.phase(), desktop::Phase::Idle);
    }
    #[test]
    fn saved_tabs_reopen_on_their_form_without_connecting() {
        let directory = std::env::temp_dir().join(format!(
            "starcom-workspace-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let file = directory.join("workspace.conf");
        store::save(
            &file,
            &store::Workspace {
                tabs: vec![
                    store::Tab {
                        destination: "dev".into(),
                        host: "10.0.0.2".into(),
                        user: "alice".into(),
                        session: "work".into(),
                        port: 2222,
                        history: 300,
                        interactive: true,
                        reconnect: true,
                        ..store::Tab::default()
                    },
                    store::Tab {
                        host: "build.example.test".into(),
                        user: "bob".into(),
                        session: "ci".into(),
                        port: 22,
                        history: 200,
                        ..store::Tab::default()
                    },
                ],
                active: 1,
                fps: store::DEFAULT_FPS,
            },
        )
        .unwrap();

        let mut workspace = Workspace {
            tabs: Vec::new(),
            active: 0,
            next: 1,
            wake: sync::Arc::new(|| {}),
            config: sync::Arc::new(ssh_config::Config::default()),
            config_error: None,
            notice: None,
            store: Some(file.clone()),
            fps: store::DEFAULT_FPS,
            suspend_clock: reconnect::AliveClock::now(),
        };
        assert!(workspace.restore(|_, _| dialog::BrokenStore::Exit).unwrap());
        assert_eq!(workspace.tabs.len(), 2);
        assert_eq!(workspace.active, 1);
        // The whole point: no tab may be connecting or connected at startup.
        for tab in &workspace.tabs {
            assert_eq!(
                tab.client.phase(),
                desktop::Phase::Idle,
                "restoring a workspace must not authenticate"
            );
            assert!(tab.client.retry().is_none());
        }
        assert_eq!(workspace.tabs[0].label, "dev / work");
        assert_eq!(workspace.tabs[1].label, "build.example.test / ci");
        // Round-tripping through the live tabs must not lose or alter anything.
        workspace.persist();
        let reloaded = store::load(&file).unwrap().unwrap();
        assert_eq!(reloaded.tabs.len(), 2);
        assert_eq!(reloaded.tabs[0].host, "10.0.0.2");
        assert_eq!(reloaded.tabs[0].port, 2222);
        assert_eq!(reloaded.tabs[1].session, "ci");
        assert_eq!(reloaded.active, 1);
    }

    fn broken_workspace(file: path::PathBuf) -> Workspace {
        Workspace {
            tabs: Vec::new(),
            active: 0,
            next: 1,
            wake: sync::Arc::new(|| {}),
            config: sync::Arc::new(ssh_config::Config::default()),
            config_error: None,
            notice: None,
            store: Some(file),
            fps: store::DEFAULT_FPS,
            suspend_clock: reconnect::AliveClock::now(),
        }
    }

    #[test]
    fn an_unreadable_saved_workspace_is_left_alone_when_the_user_exits() {
        let directory = std::env::temp_dir().join(format!(
            "starcom-workspace-bad-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("workspace.conf");
        let original = "[tab]\nport not-a-number\n";
        std::fs::write(&file, original).unwrap();
        let mut workspace = broken_workspace(file.clone());
        assert!(!workspace.restore(|_, _| dialog::BrokenStore::Exit).unwrap());
        assert!(workspace.tabs.is_empty());
        workspace.new_tab().unwrap();
        workspace.persist();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    }

    #[test]
    fn clearing_an_unreadable_saved_workspace_deletes_the_file() {
        let directory = std::env::temp_dir().join(format!(
            "starcom-workspace-clear-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("workspace.conf");
        std::fs::write(&file, "[tab]\nport not-a-number\n").unwrap();
        let mut workspace = broken_workspace(file.clone());
        assert!(
            workspace
                .restore(|_, _| dialog::BrokenStore::Clear)
                .unwrap()
        );
        assert!(workspace.tabs.is_empty());
        assert!(!file.exists(), "Clear must delete the unreadable file");
        workspace.new_tab().unwrap();
        workspace.persist();
        assert!(store::load(&file).unwrap().is_some());
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
        workspace.apply(
            Action::Tab(workspace.tabs[0].id, Box::new(ui::Action::None)),
            || None,
        );
        assert_eq!(count.load(sync::atomic::Ordering::Relaxed), before);
    }
}
