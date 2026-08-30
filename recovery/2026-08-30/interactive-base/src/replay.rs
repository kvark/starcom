//! Synthetic transcript harness. Live topology discovery and snapshot
//! reconstruction are intentionally not implemented by this fixed-size replay.

use std::collections;

use crate::{control, core, terminal};

const MAX_PANES: usize = 16;

pub struct Replay {
    control: control::Control,
    size: core::Size,
    panes: collections::BTreeMap<tmuxctl::PaneId, terminal::Terminal>,
}

impl Replay {
    pub fn new(size: core::Size) -> Self {
        Self {
            control: control::Control::default(),
            size,
            panes: collections::BTreeMap::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        // Bound the temporary event vector even when the caller supplies a huge
        // transcript in one call. The protocol's own buffers have separate limits.
        for chunk in bytes.chunks(4096) {
            let mut events = Vec::new();
            self.control.feed(chunk, |event| events.push(event))?;
            for event in events {
                self.apply(event)?;
            }
        }
        Ok(())
    }

    pub fn finish(&mut self) -> anyhow::Result<()> {
        let mut events = Vec::new();
        self.control.finish(|event| events.push(event))?;
        for event in events {
            self.apply(event)?;
        }
        Ok(())
    }

    pub fn panes(&self) -> &collections::BTreeMap<tmuxctl::PaneId, terminal::Terminal> {
        &self.panes
    }

    fn apply(&mut self, event: tmuxctl::Incoming) -> anyhow::Result<()> {
        match event {
            tmuxctl::Incoming::Notification(tmuxctl::Notification::Output { pane, bytes })
            | tmuxctl::Incoming::Notification(tmuxctl::Notification::ExtendedOutput {
                pane,
                bytes,
                ..
            }) => {
                if !self.panes.contains_key(&pane) && self.panes.len() >= MAX_PANES {
                    anyhow::bail!("replay exceeds the {MAX_PANES}-pane safety budget");
                }
                self.panes
                    .entry(pane)
                    .or_insert_with(|| terminal::Terminal::new(self.size, 64))
                    .feed(&bytes);
            }
            tmuxctl::Incoming::Notification(tmuxctl::Notification::LayoutChange { .. }) => {
                anyhow::bail!("fixed-size replay does not implement layout changes");
            }
            tmuxctl::Incoming::Reply {
                result: Err(error), ..
            } => return Err(error.into()),
            _ => {}
        }
        Ok(())
    }
}
