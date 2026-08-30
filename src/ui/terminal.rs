//! Visible-row terminal painting and local selection. No TextEdit, no widget per
//! cell, and no path from GUI input to the remote application in this increment.

use alacritty_terminal::{grid::Dimensions, index, selection, term, vte::ansi};

use crate::snapshot;

const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(18, 21, 26);
const FOREGROUND: egui::Color32 = egui::Color32::from_rgb(214, 220, 229);

pub struct PaneUi {
    pub rect: egui::Rect,
    pub painted_rows: usize,
}

impl Default for PaneUi {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            painted_rows: 0,
        }
    }
}

impl PaneUi {
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        pane: &mut snapshot::Pane,
        generation: u64,
        font_size: f32,
        focused: &mut Option<tmuxctl::PaneId>,
        notice: &mut Option<String>,
    ) {
        self.rect = rect;
        self.painted_rows = 0;
        if rect.width() < 16.0 || rect.height() < 40.0 {
            return;
        }
        let pane_id = pane.state.pane;
        let id = egui::Id::new(("terminal", generation, pane_id.0));
        let border = if *focused == Some(pane_id) {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        };
        ui.painter().rect_filled(rect, 3.0, BACKGROUND);
        ui.painter().rect_stroke(
            rect,
            3.0,
            egui::Stroke::new(1.0_f32, border),
            egui::StrokeKind::Inside,
        );
        ui.scope_builder(egui::UiBuilder::new().id_salt(id).max_rect(rect.shrink(6.0)), |ui| {
            ui.set_clip_rect(rect.shrink(1.0).intersect(ui.clip_rect()));
            ui.set_min_size(rect.shrink(6.0).size());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("Pane {pane_id}")).strong());
                ui.weak(format!("{}×{}", pane.state.size.columns(), pane.state.size.rows()));
                ui.weak(if pane.terminal.is_alternate_screen() { "alternate" } else { "shell" });
                if pane.history_may_be_truncated { ui.label("*").on_hover_text("Older output may not have been retained by tmux or this client's history budget."); }
            });
            ui.separator();
            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
            let font = egui::FontId::monospace(font_size);
            let (cell_width, row_height) = ui.fonts_mut(|fonts| (fonts.glyph_width(&font, 'M'), fonts.row_height(&font).ceil() + 2.0));
            let columns = pane.state.size.columns();
            let history = pane.terminal.model().grid().history_size();
            let total_rows = pane.terminal.model().grid().total_lines();
            let rows = &mut self.painted_rows;
            egui::ScrollArea::both().id_salt(id.with("scroll")).auto_shrink([false, false])
                .stick_to_bottom(true).show_rows(ui, row_height, total_rows, |ui, range| {
                    *rows = range.len();
                    let (_, content) = ui.allocate_space(egui::vec2(columns as f32 * cell_width, range.len() as f32 * row_height));
                    let response = ui.interact(content, id, egui::Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Text);
                    let point_at = |position: egui::Pos2| {
                        let row = (range.start as isize + ((position.y - content.top()) / row_height).floor() as isize)
                            .clamp(0, total_rows as isize - 1) as usize;
                        let x = ((position.x - content.left()) / cell_width).clamp(0.0, columns as f32 - 0.001);
                        (index::Point::new(index::Line(row as i32 - history as i32), index::Column(x as usize)),
                            if x.fract() < 0.5 { index::Side::Left } else { index::Side::Right })
                    };
                    if response.clicked() || response.drag_started() || response.secondary_clicked() {
                        *focused = Some(pane_id); response.request_focus();
                    }
                    if let Some(position) = response.interact_pointer_pos() {
                        let (point, side) = point_at(position);
                        if response.triple_clicked() {
                            pane.terminal.begin_selection(point, side, selection::SelectionType::Lines);
                        } else if response.double_clicked() {
                            pane.terminal.begin_selection(point, side, selection::SelectionType::Semantic);
                        } else if response.clicked() {
                            pane.terminal.clear_selection();
                        } else if response.drag_started() {
                            let origin = ui.input(|input| input.pointer.press_origin()).unwrap_or(position);
                            let (anchor, anchor_side) = point_at(origin);
                            pane.terminal.begin_selection(anchor, anchor_side, selection::SelectionType::Simple);
                        }
                        if response.dragged() {
                            pane.terminal.update_selection(point, side);
                            let clip = ui.clip_rect();
                            let delta = if position.y < clip.top() + 12.0 { row_height }
                                else if position.y > clip.bottom() - 12.0 { -row_height } else { 0.0 };
                            if delta != 0.0 {
                                ui.scroll_with_delta(egui::vec2(0.0, delta));
                                ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
                            }
                        }
                    }
                    let copying = response.has_focus() && ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Copy)));
                    if copying && let Some(text) = pane.terminal.selected_text() { copy(ui.ctx(), text, notice); }
                    response.context_menu(|ui| {
                        if ui.add_enabled(pane.terminal.selection_range().is_some(), egui::Button::new("Copy selection")).clicked() {
                            if let Some(text) = pane.terminal.selected_text() { copy(ui.ctx(), text, notice); }
                            ui.close();
                        }
                        if ui.button("Copy current screen").clicked() {
                            copy(ui.ctx(), pane.terminal.screen_lines().join("\n"), notice); ui.close();
                        }
                    });
                    let model = pane.terminal.model();
                    let selection_range = pane.terminal.selection_range();
                    let grid = model.grid();
                    let clip = ui.clip_rect();
                    for row in range.clone() {
                        let line = index::Line(row as i32 - history as i32);
                        let y = content.top() + (row - range.start) as f32 * row_height;
                        // Paint backgrounds before glyphs. In particular, a wide
                        // glyph spans its following spacer cell; painting that
                        // spacer afterwards would erase half of the glyph.
                        for column in 0..columns {
                            let cell = &grid[line][index::Column(column)];
                            let point = index::Point::new(line, index::Column(column));
                            let selected = selection_range.is_some_and(|range| range.contains(point) ||
                                cell.flags.contains(term::cell::Flags::WIDE_CHAR) && range.contains(index::Point::new(point.line, point.column + 1)));
                            let background = if selected { ui.visuals().selection.bg_fill }
                                else { cell_colors(cell, model.colors()).1 };
                            if background != BACKGROUND {
                                let cell_rect = egui::Rect::from_min_size(egui::pos2(content.left() + column as f32 * cell_width, y), egui::vec2(cell_width, row_height));
                                ui.painter().rect_filled(cell_rect, 0.0, background);
                            }
                        }
                        let mut column = 0;
                        while column < columns {
                            let cell = &grid[line][index::Column(column)];
                            let foreground = cell_colors(cell, model.colors()).0;
                            if cell.flags.intersects(term::cell::Flags::WIDE_CHAR_SPACER | term::cell::Flags::LEADING_WIDE_CHAR_SPACER | term::cell::Flags::HIDDEN) {
                                column += 1; continue;
                            }
                            // Batch ordinary ASCII into fixed-width runs. A wide or
                            // combining glyph gets its own positioned cell cluster,
                            // so fallback font metrics cannot move the next column.
                            let start = column;
                            let mut text = String::new();
                            if cell.c.is_ascii() && cell.zerowidth().is_none_or(<[char]>::is_empty) {
                                while column < columns {
                                    let next = &grid[line][index::Column(column)];
                                    if !next.c.is_ascii() || next.zerowidth().is_some_and(|chars| !chars.is_empty())
                                        || next.flags != cell.flags || next.fg != cell.fg || next.bg != cell.bg { break; }
                                    text.push(if next.c.is_control() { ' ' } else { next.c });
                                    column += 1;
                                }
                            } else {
                                text.push(cell.c);
                                if let Some(chars) = cell.zerowidth() { text.extend(chars); }
                                column += 1;
                            }
                            if text.trim().is_empty() && !cell.flags.intersects(term::cell::Flags::ALL_UNDERLINES | term::cell::Flags::STRIKEOUT) { continue; }
                            let format = egui::TextFormat {
                                font_id: font.clone(), color: foreground, italics: cell.flags.contains(term::cell::Flags::ITALIC),
                                underline: if cell.flags.intersects(term::cell::Flags::ALL_UNDERLINES) { egui::Stroke::new(1.0_f32, foreground) } else { egui::Stroke::NONE },
                                strikethrough: if cell.flags.contains(term::cell::Flags::STRIKEOUT) { egui::Stroke::new(1.0_f32, foreground) } else { egui::Stroke::NONE },
                                ..Default::default()
                            };
                            let job = egui::text::LayoutJob::simple_format(text, format);
                            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                            let x = content.left() + start as f32 * cell_width;
                            let width = if cell.flags.contains(term::cell::Flags::WIDE_CHAR) { 2 } else { column - start };
                            let glyph_clip = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width as f32 * cell_width, row_height));
                            ui.painter().with_clip_rect(clip.intersect(glyph_clip)).galley(egui::pos2(x, y + (row_height - galley.size().y) * 0.5), galley, foreground);
                        }
                    }
                    let cursor = model.renderable_content().cursor;
                    let row = (cursor.point.line.0 + history as i32) as usize;
                    if range.contains(&row) && cursor.shape != ansi::CursorShape::Hidden {
                        let rect = egui::Rect::from_min_size(egui::pos2(content.left() + cursor.point.column.0 as f32 * cell_width,
                            content.top() + (row - range.start) as f32 * row_height), egui::vec2(cell_width, row_height));
                        let stroke = egui::Stroke::new(1.0_f32, FOREGROUND);
                        match cursor.shape {
                            ansi::CursorShape::Beam => { ui.painter().line_segment([rect.left_top(), rect.left_bottom()], stroke); }
                            ansi::CursorShape::Underline => { ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], stroke); }
                            _ => { ui.painter().rect_stroke(rect.shrink(0.5), 0.0, stroke, egui::StrokeKind::Inside); }
                        }
                    }
                });
        });
    }
}

pub fn copy(ctx: &egui::Context, text: String, notice: &mut Option<String>) {
    if text.len() > 1024 * 1024 {
        *notice = Some("Selection exceeds the 1 MiB clipboard limit.".to_owned());
    } else {
        ctx.copy_text(text);
        *notice = Some("Copied.".to_owned());
    }
}

fn cell_colors(
    cell: &term::cell::Cell,
    colors: &term::color::Colors,
) -> (egui::Color32, egui::Color32) {
    let mut foreground = color(cell.fg, colors);
    let mut background = color(cell.bg, colors);
    if cell.flags.contains(term::cell::Flags::BOLD) {
        foreground = match cell.fg {
            ansi::Color::Named(named) if (named as usize) < 8 => {
                indexed(named as usize + 8, colors)
            }
            ansi::Color::Indexed(index) if index < 8 => indexed(usize::from(index) + 8, colors),
            _ => foreground,
        };
    }
    if cell.flags.contains(term::cell::Flags::DIM) {
        foreground = foreground.gamma_multiply(0.65);
    }
    if cell.flags.contains(term::cell::Flags::INVERSE) {
        std::mem::swap(&mut foreground, &mut background);
    }
    (foreground, background)
}

fn color(color: ansi::Color, colors: &term::color::Colors) -> egui::Color32 {
    match color {
        ansi::Color::Spec(rgb) => egui::Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        ansi::Color::Indexed(index) => indexed(usize::from(index), colors),
        ansi::Color::Named(named) => indexed(named as usize, colors),
    }
}

fn indexed(index: usize, colors: &term::color::Colors) -> egui::Color32 {
    if let Some(rgb) = colors[index] {
        return egui::Color32::from_rgb(rgb.r, rgb.g, rgb.b);
    }
    const ANSI: [[u8; 3]; 16] = [
        [30, 34, 42],
        [224, 108, 117],
        [152, 195, 121],
        [229, 192, 123],
        [97, 175, 239],
        [198, 120, 221],
        [86, 182, 194],
        [214, 220, 229],
        [110, 121, 138],
        [239, 134, 143],
        [174, 217, 143],
        [245, 215, 151],
        [129, 194, 249],
        [215, 151, 235],
        [121, 205, 213],
        [244, 246, 250],
    ];
    let rgb = match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let n = index - 16;
            let level = |n| if n == 0 { 0 } else { 55 + n as u8 * 40 };
            [level(n / 36), level(n / 6 % 6), level(n % 6)]
        }
        232..=255 => [8 + (index - 232) as u8 * 10; 3],
        257 => return BACKGROUND,
        268 => return FOREGROUND.gamma_multiply(0.65),
        259..=266 => return indexed(index - 259, colors).gamma_multiply(0.65),
        _ => return FOREGROUND,
    };
    egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_rgb_and_inverse_colors_are_preserved() {
        let colors = term::color::Colors::default();
        assert_eq!(indexed(196, &colors), egui::Color32::from_rgb(255, 0, 0));
        assert_eq!(
            color(ansi::Color::Named(ansi::NamedColor::DimForeground), &colors),
            FOREGROUND.gamma_multiply(0.65)
        );
        let cell = term::cell::Cell {
            fg: ansi::Color::Spec(ansi::Rgb {
                r: 12,
                g: 34,
                b: 56,
            }),
            bg: ansi::Color::Named(ansi::NamedColor::Background),
            flags: term::cell::Flags::INVERSE,
            ..Default::default()
        };
        assert_eq!(
            cell_colors(&cell, &colors),
            (BACKGROUND, egui::Color32::from_rgb(12, 34, 56))
        );
    }
}
