//! A result grid (the C# DataGrid of BuildGrid): sortable, resizable columns, extended row selection, a context menu
//! and, for tables with a primary key, cell editing.

use std::collections::BTreeSet;

use aio_remote::sql_session::{SqlColumn, SqlResult, SqlValue};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};

use super::text;
use crate::gui::theme::Palette;
use crate::i18n::t;

/// What the user did in a grid that needs the server.
pub enum GridAction {
    /// A header was clicked in a grid sorted on the server.
    SortServer(String),
    /// A changed cell (None: Set to NULL).
    Save { row: usize, column: usize, value: Option<String> },
    Delete,
}

struct Edit {
    row: usize,
    column: usize,
    text: String,
    original: String,
    focused: bool,
}

pub struct Grid {
    pub id: u64,
    pub result: SqlResult,
    columns: Vec<SqlColumn>,
    cells: Vec<Vec<String>>,
    pub editable: bool,
    server_sort: bool,
    /// The rows in the order shown.
    order: Vec<usize>,
    sort: Option<(usize, bool)>,
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    current: Option<(usize, usize)>,
    edit: Option<Edit>,
}

impl Grid {
    pub fn new(id: u64, result: SqlResult, editable: bool, server_sort: bool) -> Self {
        let columns = result.columns.clone().unwrap_or_default();
        let cells = result.rows.iter().map(|row| row.iter().zip(&columns).map(|(v, c)| text::cell(v, c)).collect()).collect();
        let order = (0..result.rows.len()).collect();
        Grid { id, result, columns, cells, editable, server_sort, order, sort: None, selected: BTreeSet::new(), anchor: None, current: None, edit: None }
    }

    pub fn columns(&self) -> &[SqlColumn] {
        &self.columns
    }

    pub fn row(&self, row: usize) -> Option<&Vec<SqlValue>> {
        self.result.rows.get(row)
    }

    /// The selected rows, in the order shown.
    pub fn selected_rows(&self) -> Vec<usize> {
        self.order.iter().copied().filter(|r| self.selected.contains(r)).collect()
    }

    /// A saved cell: the row keeps the new value (C# `row.Values[column] = text`).
    pub fn set_value(&mut self, row: usize, column: usize, value: Option<String>) {
        if let (Some(cells), Some(values)) = (self.cells.get_mut(row), self.result.rows.get_mut(row)) {
            values[column] = value.map(SqlValue::Text).unwrap_or(SqlValue::Null);
            cells[column] = text::cell(&values[column], &self.columns[column]);
        }
    }

    fn copy_rows(&self, ctx: &egui::Context) {
        let rows = self.selected_rows();
        if rows.is_empty() {
            return;
        }
        let rows: Vec<&Vec<SqlValue>> = rows.iter().map(|&r| &self.result.rows[r]).collect();
        ctx.copy_text(text::copy_rows(&self.columns, &rows));
    }

    fn sort_client(&mut self, column: usize) {
        let descending = self.sort == Some((column, false));
        self.sort = Some((column, descending));
        let rows = &self.result.rows;
        self.order.sort_by(|&a, &b| {
            let order = text::compare(&rows[a][column], &rows[b][column]);
            if descending { order.reverse() } else { order }
        });
    }

    /// Draws the grid in the space left. `server_order` is the server sort of a table's data, for the header arrows.
    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, server_order: Option<(&str, bool)>) -> Option<GridAction> {
        let mut action = None;
        if self.columns.is_empty() {
            ui.label(RichText::new(t("No rows")).size(12.5).color(palette.muted));
            return None;
        }
        let ctx = ui.ctx().clone();
        let modifiers = ui.input(|i| i.modifiers);
        let mut header_clicked = None;
        let mut clicked: Option<(usize, usize, bool)> = None; // view row, column, right click
        let mut double_clicked: Option<(usize, usize)> = None;
        let mut menu: Option<&'static str> = None;
        let mut commit: Option<bool> = None; // true: save, false: cancel
        let row_height = 22.0;
        let sort_arrow = |index: usize, name: &str| -> &'static str {
            let sorted = match server_order {
                Some((column, descending)) if column == name => Some(descending),
                Some(_) => None,
                None => self.sort.filter(|(c, _)| *c == index).map(|(_, d)| d),
            };
            match sorted {
                Some(true) => " ▼",
                Some(false) => " ▲",
                None => "",
            }
        };
        let headers: Vec<String> = self.columns.iter().enumerate().map(|(i, c)| format!("{}{}", c.name, sort_arrow(i, &c.name))).collect();
        let area = egui::ScrollArea::horizontal().id_salt(("sql-grid-h", self.id)).auto_shrink([false, false]).show(ui, |ui| {
            let mut table = TableBuilder::new(ui)
                .id_salt(("sql-grid", self.id))
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .auto_shrink([false, false])
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for _ in &self.columns {
                table = table.column(Column::auto().at_least(48.0).at_most(420.0).clip(true).resizable(true));
            }
            let editable = self.editable;
            let Grid { cells, order, selected, edit, columns, .. } = &mut *self;
            table
                .header(24.0, |mut header| {
                    for (index, name) in headers.iter().enumerate() {
                        header.col(|ui| {
                            let response = ui.add(egui::Label::new(RichText::new(name).size(12.5).strong()).sense(egui::Sense::click()).truncate());
                            if response.clicked() {
                                header_clicked = Some(index);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_height, order.len(), |mut row| {
                        let view = row.index();
                        let index = order[view];
                        row.set_selected(selected.contains(&index));
                        // The number itself is carried into `clicked`, `double_clicked` and the edit test,
                        // so iterating the cells instead would still need it.
                        #[allow(clippy::needless_range_loop)]
                        for column in 0..columns.len() {
                            let (_, response) = row.col(|ui| {
                                if let Some(e) = edit.as_mut().filter(|e| e.row == index && e.column == column) {
                                    let field = ui.add(egui::TextEdit::singleline(&mut e.text).font(egui::FontId::proportional(12.5)).desired_width(f32::INFINITY));
                                    if !e.focused {
                                        e.focused = true;
                                        field.request_focus();
                                    } else if field.lost_focus() {
                                        commit = Some(!ui.input(|i| i.key_pressed(egui::Key::Escape)));
                                    }
                                } else {
                                    let value = &cells[index][column];
                                    let rich = RichText::new(value.as_str()).size(12.5);
                                    let rich = if value == text::NULL_TEXT { rich.color(palette.subtle).italics() } else { rich };
                                    ui.add(egui::Label::new(rich).truncate().selectable(false));
                                }
                            });
                            if response.clicked() {
                                clicked = Some((view, column, false));
                            }
                            if response.secondary_clicked() {
                                clicked = Some((view, column, true));
                            }
                            if response.double_clicked() {
                                double_clicked = Some((index, column));
                            }
                            response.context_menu(|ui| {
                                let mut items = vec![("copy", t("Copy value")), ("rows", t("Copy rows (with column names)"))];
                                if editable {
                                    items.push(("null", t("Set to NULL")));
                                    items.push(("delete", t("Delete selected rows")));
                                }
                                for (key, label) in items {
                                    if ui.button(label).clicked() {
                                        menu = Some(key);
                                        ui.close_menu();
                                    }
                                }
                            });
                        }
                    });
                });
        });
        let _ = area;

        // Selection (Ctrl toggles, Shift extends), like the DataGrid's extended mode.
        if let Some((view, column, right)) = clicked {
            let index = self.order[view];
            if right {
                if !self.selected.contains(&index) {
                    self.selected = BTreeSet::from([index]);
                    self.anchor = Some(view);
                }
            } else if modifiers.command {
                if !self.selected.remove(&index) {
                    self.selected.insert(index);
                }
                self.anchor = Some(view);
            } else if let (true, Some(anchor)) = (modifiers.shift, self.anchor) {
                self.selected = (anchor.min(view)..=anchor.max(view)).map(|v| self.order[v]).collect();
            } else {
                self.selected = BTreeSet::from([index]);
                self.anchor = Some(view);
            }
            self.current = Some((index, column));
        }
        if let Some((row, column)) = double_clicked {
            if self.editable && !self.columns[column].is_binary() && self.edit.is_none() {
                let value = self.cells[row][column].clone();
                self.edit = Some(Edit { row, column, text: value.clone(), original: value, focused: false });
            }
        }
        if let Some(save) = commit {
            if let Some(edit) = self.edit.take() {
                if save && edit.text != edit.original {
                    action = Some(GridAction::Save { row: edit.row, column: edit.column, value: Some(edit.text) });
                }
            }
        }
        match menu {
            Some("copy") => {
                if let Some((row, column)) = self.current {
                    ctx.copy_text(text::full(&self.result.rows[row][column], &self.columns[column]).unwrap_or_default());
                }
            }
            Some("rows") => self.copy_rows(&ctx),
            Some("null") if self.editable => {
                if let Some((row, column)) = self.current {
                    if !self.result.rows[row][column].is_null() {
                        action = Some(GridAction::Save { row, column, value: None });
                    }
                }
            }
            Some("delete") if self.editable => action = Some(GridAction::Delete),
            _ => {}
        }
        // Ctrl+C copies the selected rows with the column names (ClipboardCopyMode.IncludeHeader).
        let pointer_inside = ui.rect_contains_pointer(ui.min_rect());
        if pointer_inside && ctx.memory(|m| m.focused().is_none()) && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
            self.copy_rows(&ctx);
        }
        if let Some(column) = header_clicked {
            if self.server_sort {
                action = Some(GridAction::SortServer(self.columns[column].name.clone()));
            } else {
                self.sort_client(column);
            }
        }
        action
    }
}
