//! Reconstruct the split hierarchy from tmux's pane rectangles.
//! Ratios are local viewport sizes; dragging NEVER resizes a remote pane.

use crate::snapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug)]
pub enum Node {
    Pane(tmuxctl::PaneId),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    pub fn from_panes(panes: &[&snapshot::Pane]) -> Option<Self> {
        if panes.len() == 1 {
            return Some(Self::Pane(panes[0].state.pane));
        }
        if panes.is_empty() {
            return None;
        }
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let start = |pane: &&snapshot::Pane| match axis {
                Axis::Horizontal => pane.state.left,
                Axis::Vertical => pane.state.top,
            };
            let end = |pane: &&snapshot::Pane| {
                start(pane)
                    + match axis {
                        Axis::Horizontal => pane.state.size.columns(),
                        Axis::Vertical => pane.state.size.rows(),
                    }
            };
            let low = panes.iter().map(start).min()?;
            let high = panes.iter().map(end).max()?;
            let mut cuts: Vec<_> = panes.iter().map(start).filter(|&x| x > low).collect();
            cuts.sort_unstable_by_key(|&x| (x * 2).abs_diff(low + high));
            cuts.dedup();
            for cut in cuts {
                if panes
                    .iter()
                    .any(|pane| start(pane) < cut && end(pane) > cut)
                {
                    continue;
                }
                let (first, second): (Vec<_>, Vec<_>) =
                    panes.iter().copied().partition(|pane| start(pane) < cut);
                if first.is_empty() || second.is_empty() {
                    continue;
                }
                return Some(Self::Split {
                    axis,
                    ratio: (cut - low) as f32 / (high - low) as f32,
                    first: Box::new(Self::from_panes(&first)?),
                    second: Box::new(Self::from_panes(&second)?),
                });
            }
        }
        None
    }

    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        id: egui::Id,
        pane: &mut impl FnMut(&mut egui::Ui, egui::Rect, tmuxctl::PaneId),
    ) {
        match *self {
            Self::Pane(pane_id) => pane(ui, rect, pane_id),
            Self::Split {
                axis,
                ref mut ratio,
                ref mut first,
                ref mut second,
            } => {
                let extent = match axis {
                    Axis::Horizontal => rect.width(),
                    Axis::Vertical => rect.height(),
                };
                let gap = 6.0_f32.min(extent.max(0.0));
                let available = (extent - gap).max(0.0);
                let minimum = 80.0_f32.min(available * 0.4);
                let boundary =
                    (available * *ratio).clamp(minimum, (available - minimum).max(minimum));
                let (mut a, mut divider, mut b) = (rect, rect, rect);
                match axis {
                    Axis::Horizontal => {
                        a.max.x = rect.min.x + boundary;
                        divider.min.x = a.max.x;
                        divider.max.x = a.max.x + gap;
                        b.min.x = divider.max.x;
                    }
                    Axis::Vertical => {
                        a.max.y = rect.min.y + boundary;
                        divider.min.y = a.max.y;
                        divider.max.y = a.max.y + gap;
                        b.min.y = divider.max.y;
                    }
                }
                let response = ui
                    .interact(divider, id, egui::Sense::drag())
                    .on_hover_cursor(match axis {
                        Axis::Horizontal => egui::CursorIcon::ResizeHorizontal,
                        Axis::Vertical => egui::CursorIcon::ResizeVertical,
                    })
                    .on_hover_text("Resize local views only. Remote tmux geometry is unchanged.");
                if response.dragged()
                    && available > 0.0
                    && let Some(position) = response.interact_pointer_pos()
                {
                    let position = match axis {
                        Axis::Horizontal => position.x - rect.min.x,
                        Axis::Vertical => position.y - rect.min.y,
                    };
                    *ratio = ((position - gap * 0.5) / available).clamp(0.05, 0.95);
                    ui.ctx().request_repaint();
                }
                if response.hovered() || response.dragged() {
                    ui.painter().rect_filled(
                        divider.shrink(1.0),
                        1.0,
                        ui.visuals().widgets.hovered.bg_fill,
                    );
                }
                first.draw(ui, a, id.with(0), pane);
                second.draw(ui, b, id.with(1), pane);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn demo_recovers_independent_window_splits() {
        let view = crate::desktop::demo_view().unwrap();
        let panes: Vec<_> = view
            .panes()
            .values()
            .filter(|pane| pane.state.window == tmuxctl::WindowId(0))
            .collect();
        let node = Node::from_panes(&panes).unwrap();
        match node {
            Node::Split {
                axis: Axis::Horizontal,
                ratio,
                ..
            } => assert!((0.49..0.52).contains(&ratio)),
            _ => panic!("expected side-by-side split"),
        }
    }
}
