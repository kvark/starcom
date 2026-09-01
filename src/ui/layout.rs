//! Reconstruct the split hierarchy from tmux's pane rectangles.
//! Local dividers request a tmux resize on release, in the same cell units
//! used to paint. The server owns the final geometry.

use crate::{input, snapshot};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug)]
pub struct Boundary {
    pane: tmuxctl::PaneId,
    pane_cells: usize,
}

impl Boundary {
    /// Size from the GUI pixels and the same cell metrics used to paint.
    fn resize_pixels(
        self,
        axis: Axis,
        first_pixels: f32,
        cell: f32,
    ) -> Option<(tmuxctl::PaneId, input::Resize)> {
        if !(cell.is_finite() && cell > 0.0 && first_pixels.is_finite()) {
            return None;
        }
        let cells = (first_pixels / cell).round().clamp(1.0, 4096.0) as usize;
        (cells != self.pane_cells).then_some((
            self.pane,
            input::Resize {
                axis: match axis {
                    Axis::Horizontal => input::Axis::Columns,
                    Axis::Vertical => input::Axis::Rows,
                },
                cells,
            },
        ))
    }
}

#[derive(Debug)]
pub enum Node {
    Pane(tmuxctl::PaneId),
    Split {
        axis: Axis,
        ratio: f32,
        boundary: Option<Boundary>,
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
                let first_end = first.iter().map(end).max()?;
                // A normal tmux split has exactly one separator cell. Unusual
                // border/status layouts remain viewable but cannot be resized
                // from an inferred boundary without a more complete layout model.
                // A leaf must span the entire first side along this axis.
                // Otherwise resize-pane can select a nearer nested split and
                // move the wrong boundary. Such dividers stay local-only.
                let boundary = if first_end + 1 == cut {
                    first
                        .iter()
                        .find(|pane| start(pane) == low && end(pane) == first_end)
                        .map(|pane| Boundary {
                            pane: pane.state.pane,
                            pane_cells: match axis {
                                Axis::Horizontal => pane.state.size.columns(),
                                Axis::Vertical => pane.state.size.rows(),
                            },
                        })
                } else {
                    None
                };
                return Some(Self::Split {
                    axis,
                    ratio: (first_end - low) as f32 / (high - low - (cut - first_end)) as f32,
                    boundary,
                    first: Box::new(Self::from_panes(&first)?),
                    second: Box::new(Self::from_panes(&second)?),
                });
            }
        }
        None
    }

    /// Zoomed tmux windows still list every pane, overlapping. Show the largest
    /// (the zoomed one filling the window) until tmux unzooms.
    pub fn from_panes_or_zoom(panes: &[&snapshot::Pane]) -> Option<Self> {
        Self::from_panes(panes).or_else(|| {
            panes
                .iter()
                .max_by_key(|pane| {
                    pane.state
                        .size
                        .columns()
                        .saturating_mul(pane.state.size.rows())
                })
                .map(|pane| Self::Pane(pane.state.pane))
        })
    }

    pub fn pane_ids(&self) -> Vec<tmuxctl::PaneId> {
        match self {
            Self::Pane(id) => vec![*id],
            Self::Split { first, second, .. } => {
                let mut ids = first.pane_ids();
                ids.extend(second.pane_ids());
                ids
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        id: egui::Id,
        remote_resize: bool,
        cell_width: f32,
        row_height: f32,
        resizes: &mut Vec<(tmuxctl::PaneId, input::Resize)>,
        pane: &mut impl FnMut(&mut egui::Ui, egui::Rect, tmuxctl::PaneId),
    ) {
        match *self {
            Self::Pane(pane_id) => pane(ui, rect, pane_id),
            Self::Split {
                axis,
                ref mut ratio,
                boundary: remote_boundary,
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
                    .on_hover_text(if remote_resize && remote_boundary.is_some() {
                        "Release to resize the tmux pane. Other attached clients see the change."
                    } else {
                        "Remote pane resize is off. Enable it to change tmux geometry."
                    });
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
                if remote_resize
                    && response.drag_stopped()
                    && let Some(resize) = remote_boundary.and_then(|edge| {
                        let cell = match axis {
                            Axis::Horizontal => cell_width,
                            Axis::Vertical => row_height,
                        };
                        edge.resize_pixels(axis, boundary, cell)
                    })
                {
                    resizes.push(resize);
                }
                if response.hovered() || response.dragged() {
                    ui.painter().rect_filled(
                        divider.shrink(1.0),
                        1.0,
                        ui.visuals().widgets.hovered.bg_fill,
                    );
                }
                first.draw(
                    ui,
                    a,
                    id.with(0),
                    remote_resize,
                    cell_width,
                    row_height,
                    resizes,
                    pane,
                );
                second.draw(
                    ui,
                    b,
                    id.with(1),
                    remote_resize,
                    cell_width,
                    row_height,
                    resizes,
                    pane,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn divider_changes_one_axis_relative_to_observed_cells() {
        let boundary = Boundary {
            pane: tmuxctl::PaneId(2),
            pane_cells: 40,
        };
        let (pane, resize) = boundary
            .resize_pixels(Axis::Horizontal, 400.0, 8.0)
            .unwrap();
        assert_eq!(pane, tmuxctl::PaneId(2));
        assert_eq!(
            resize,
            input::Resize {
                axis: input::Axis::Columns,
                cells: 50
            }
        );
        assert!(
            boundary
                .resize_pixels(Axis::Horizontal, 320.0, 8.0)
                .is_none()
        );
        assert!(
            boundary
                .resize_pixels(Axis::Vertical, 10.0, f32::NAN)
                .is_none()
        );
    }

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

    fn pane(id: u32, left: usize, top: usize, columns: usize, rows: usize) -> snapshot::Pane {
        let state = snapshot::State::parse(&format!(
            "%{id}|@0|{columns}|{rows}|{left}|{top}|0|0|0|0|2000|||0|{}|1|0|0|0|1|0|0|0|0|0|1|",
            rows - 1
        ))
        .unwrap();
        let size = state.size;
        snapshot::Pane {
            state,
            terminal: crate::terminal::Terminal::new(size, 0),
            history_may_be_truncated: false,
        }
    }

    #[test]
    fn overlapping_panes_are_treated_as_a_zoomed_window() {
        let zoomed = pane(0, 0, 0, 80, 24);
        let hidden = pane(1, 0, 0, 40, 24);
        let panes = [&zoomed, &hidden];
        assert!(
            Node::from_panes(&panes).is_none(),
            "overlapping geometry is not a split"
        );
        match Node::from_panes_or_zoom(&panes) {
            Some(Node::Pane(id)) => assert_eq!(id, tmuxctl::PaneId(0)),
            other => panic!("expected the larger pane, got {other:?}"),
        }
    }
}
