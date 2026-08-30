//! Stable public desktop entry points around the winit/Blade runtime.

use std::{path, sync};

use crate::{desktop, ui, window_runtime, workspace};

#[cfg(test)]
pub(crate) fn configure(ctx: &egui::Context) {
    window_runtime::configure(ctx);
}

pub fn run(startup: desktop::Startup) -> anyhow::Result<()> {
    window_runtime::run(startup)
}

/// `desktop::save_demo` supplies demo state for historical API compatibility.
/// Build a real demo workspace so the snapshot exercises the same tab layout as
/// the running application.
pub fn save_snapshot(
    _state: &mut desktop::State,
    _ui: &mut ui::DesktopUi,
    path: &path::Path,
) -> anyhow::Result<()> {
    let mut workspace = workspace::Workspace::new(sync::Arc::new(|| {}), desktop::Startup::Demo)?;
    window_runtime::save_snapshot(&mut workspace, path)
}
