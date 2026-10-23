//! Панель подробностей выбранного файла.

use super::*;
use crate::app::{Action, App};
use crate::theme::{self, FONT_CAPTION, FONT_SMALL, Palette, RADIUS_SM, SPACE_MD, SPACE_SM, SPACE_XS};
use egui::{Align, Frame, Layout, Margin, RichText, Ui};
use file_search::platform::{format_size, format_time, is_safe_to_open};
use file_search::search::Hit;

/// Панель справа (или второй экран): подробности о выбранном файле и все действия над ним.
pub(super) fn details_panel(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let Some(index) = app.search.selected else { return };
    let Some(hit) = app.search.results.get(index).cloned() else { return };
    if app.preview.is_some() {
        return text_details(ui, app, &hit, index, pal, actions);
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let (color, label) = theme::badge(&hit.ext, pal.dark);
            theme::file_tile(ui, &label, color, 48.0);
            ui.add(egui::Label::new(theme::heading(pal, &hit.name)).wrap());
        });
        ui.add_space(SPACE_MD);
        let fact = |ui: &mut Ui, title: &str, value: String| {
            ui.label(theme::caption(pal, title));
            ui.add(egui::Label::new(theme::body(pal, value)).wrap());
            ui.add_space(SPACE_XS);
        };
        fact(ui, "Папка", parent_of(&hit.path));
        fact(ui, "Размер", format_size(hit.size));
        fact(ui, "Изменён", format_time(hit.modified_ms));
        if let Some(location) = &hit.location {
            fact(ui, "Найдено", location.clone());
        }
        if let Some(note) = status_note(&hit.status) {
            ui.add(egui::Label::new(RichText::new(note).size(FONT_SMALL).color(pal.warn)).wrap());
            ui.add_space(SPACE_XS);
        }
        if let Some(snippet) = &hit.snippet {
            ui.label(theme::caption(pal, "Фрагмент"));
            Frame::new()
                .fill(pal.surface_alt)
                .corner_radius(theme::radius(RADIUS_SM))
                .inner_margin(Margin::same(SPACE_MD as i8))
                .show(ui, |ui| {
                    let width = ui.available_width();
                    ui.add(egui::Label::new(snippet_job(snippet, pal, width)).wrap());
                });
            ui.add_space(SPACE_SM);
        }
        ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
            let openable = is_safe_to_open(std::path::Path::new(&hit.path));
            if ui
                .add_enabled_ui(openable, |ui| theme::primary_button(ui, pal, "Открыть"))
                .inner
                .on_hover_text("Enter")
                .on_disabled_hover_text("Исполняемые файлы не запускаются отсюда")
                .clicked()
            {
                actions.push(Action::Open(index));
            }
            if theme::button(ui, pal, "Показать в папке").on_hover_text("Shift+Enter").clicked() {
                actions.push(Action::Reveal(index));
            }
            if theme::button(ui, pal, "Копировать путь").on_hover_text("Ctrl+C").clicked() {
                actions.push(Action::CopyPath(index));
            }
            if theme::button(ui, pal, "Искать только в этой папке").clicked() {
                actions.push(Action::SearchInFolder(index));
            }
        });
    });
}

/// Панель для текстового файла: коротко о файле, кнопки и под ними сам текст, как в редакторе.
pub(super) fn text_details(
    ui: &mut Ui,
    app: &mut App,
    hit: &Hit,
    index: usize,
    pal: &Palette,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_top(|ui| {
        let (color, label) = theme::badge(&hit.ext, pal.dark);
        theme::file_tile(ui, &label, color, 40.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(theme::heading(pal, &hit.name)).truncate());
            ui.add(
                egui::Label::new(theme::caption(
                    pal,
                    format!("{} · {}", format_size(hit.size), format_time(hit.modified_ms)),
                ))
                .truncate(),
            );
            let folder =
                fit_path(ui, &parent_of(&hit.path), ui.available_width(), theme::font(FONT_CAPTION), pal.muted);
            ui.add(egui::Label::new(theme::caption(pal, folder)).truncate()).on_hover_text(&hit.path);
        });
    });
    ui.add_space(SPACE_SM);
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled_ui(is_safe_to_open(std::path::Path::new(&hit.path)), |ui| {
                theme::primary_button(ui, pal, "Открыть")
            })
            .inner
            .on_hover_text("Открыть в программе по умолчанию (для редактирования). Enter")
            .on_disabled_hover_text("Исполняемые файлы не запускаются отсюда")
            .clicked()
        {
            actions.push(Action::Open(index));
        }
        for (text, hint, action) in [
            ("В папке", "Shift+Enter", Action::Reveal(index)),
            ("Копировать путь", "Ctrl+C", Action::CopyPath(index)),
            ("Искать в этой папке", "Искать только среди файлов этой папки", Action::SearchInFolder(index)),
        ] {
            if theme::button(ui, pal, text).on_hover_text(hint).clicked() {
                actions.push(action);
            }
        }
    });
    ui.add_space(SPACE_XS);
    if let Some(preview) = &mut app.preview {
        preview.show(ui, pal);
    }
}

// ------------------------------------------------------------------ пустые состояния
