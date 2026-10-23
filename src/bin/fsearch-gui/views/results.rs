//! Список результатов: строки с фрагментами, пустые результаты.

use super::*;
use crate::app::{Action, App};
use crate::theme::{
    self, CONTROL_H, FONT_CAPTION, FONT_SMALL, Icon, LINE_W, Palette, RADIUS_MD, SPACE_LG, SPACE_MD, SPACE_SM,
    SPACE_XL, SPACE_XS,
};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Frame, Layout, Margin, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, vec2};
use file_search::platform::{format_size, format_time, is_safe_to_open};
use file_search::search::{Hit, Snippet};

pub(super) fn central(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    if let Some(error) = &app.backend_error {
        return index_error(ui, app.confirm_reset, error, pal, actions);
    }
    if app.settings.roots.is_empty() && app.num_docs == 0 {
        return onboarding(ui, app, pal, actions);
    }
    if app.form.is_empty() {
        return welcome(ui, app, pal, actions);
    }
    if let Some(error) = &app.search_error {
        let error = error.clone();
        return empty_state(
            ui,
            pal,
            Icon::Warning,
            "Запрос не получилось выполнить",
            &error,
            |ui| {
                ui.label(theme::caption(pal, "Проверьте кавычки и скобки в запросе или упростите его."));
            },
        );
    }
    if app.results.is_empty() {
        return no_results(ui, app, pal, actions);
    }
    results_list(ui, app, pal, actions);
}

pub(super) fn no_results(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    if app.pending() {
        ui.add_space(SPACE_XL);
        ui.vertical_centered(|ui| {
            ui.add(egui::Spinner::new().size(20.0));
            ui.label(theme::small(pal, "Ищу…"));
        });
        return;
    }
    let indexing = app.indexing();
    let body = if indexing {
        "Индексация ещё идёт — файлы будут появляться в результатах по мере добавления."
    } else if app.filters_active() {
        "Возможно, мешают фильтры: попробуйте сбросить их и искать везде."
    } else {
        "В выбранных папках таких слов не нашлось."
    };
    let filters = app.filters_active();
    empty_state(ui, pal, Icon::Search, "Ничего не найдено", body, |ui| {
        if filters && theme::primary_button(ui, pal, "Сбросить фильтры").on_hover_text("Ctrl+0").clicked()
        {
            actions.push(Action::ResetFilters);
        }
        ui.add_space(SPACE_SM);
        theme::card_frame(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.label(theme::body(pal, "Что можно попробовать"));
                ui.add_space(SPACE_XS);
                for tip in [
                    "Проверьте написание: добавьте ~ после слова, и найдутся варианты с опечатками (аренда~).",
                    "Допишите * в конце слова, чтобы искать по началу: догов*",
                    "Оставьте меньше слов — достаточно самого важного.",
                    "Проверьте, что нужная папка добавлена в настройках.",
                ] {
                    ui.add(egui::Label::new(theme::small(pal, format!("•  {tip}"))).wrap());
                }
            });
        });
    });
}

pub(super) fn results_list(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let details_visible = app.details_visible;
    ui.add_space(SPACE_XS);
    ui.horizontal(|ui| {
        ui.set_min_height(CONTROL_H);
        let shown = if app.results.len() < app.total {
            format!(" · показано {}", thousands(app.results.len() as u64))
        } else {
            String::new()
        };
        ui.label(theme::caption(pal, format!("Найдено файлов: {}{shown}", thousands(app.total as u64))))
            .on_hover_text(format!("Поиск занял {:.0} мс", app.took_ms.max(1.0)));
        if app.indexing() {
            ui.label(RichText::new("· индексация ещё идёт").size(FONT_CAPTION).color(pal.accent))
                .on_hover_text("Не прочитанные файлы пока не находятся. Ход индексации — в строке состояния внизу");
        }
        if app.filters_active() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::ghost_button(ui, pal, "Сбросить фильтры").on_hover_text("Ctrl+0").clicked() {
                    actions.push(Action::ResetFilters);
                }
            });
        }
    });
    let mut scroll = egui::ScrollArea::vertical().id_salt("results").auto_shrink([false, false]);
    if app.scroll_to_top {
        scroll = scroll.vertical_scroll_offset(0.0);
        app.scroll_to_top = false;
    }
    let output = scroll.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = SPACE_SM;
        let mut last_visible = false;
        for (i, hit) in app.results.iter().enumerate() {
            let selected = app.selected == Some(i);
            let rect = hit_row(ui, i, hit, selected, !details_visible, pal, actions);
            if selected && app.scroll_to_selected {
                ui.scroll_to_rect(rect, None);
            }
            if i + 1 == app.results.len() {
                last_visible = ui.is_rect_visible(rect);
            }
        }
        ui.add_space(SPACE_MD);
        last_visible
    });
    app.scroll_to_selected = false;
    if output.inner {
        app.load_more();
    }
}

pub(super) fn snippet_job(snippet: &Snippet, pal: &Palette, width: f32) -> LayoutJob {
    let font = theme::font(FONT_SMALL);
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    let normal = TextFormat { font_id: font.clone(), color: pal.text_2, ..Default::default() };
    let marked = TextFormat { font_id: font, color: pal.mark_fg, background: pal.mark_bg, ..Default::default() };
    if snippet.more_before {
        job.append("… ", 0.0, normal.clone());
    }
    for part in &snippet.parts {
        job.append(&part.text, 0.0, if part.hit { marked.clone() } else { normal.clone() });
    }
    if snippet.more_after {
        job.append(" …", 0.0, normal);
    }
    job
}

/// Одна карточка результата: значок, имя, путь, фрагмент. Возвращает её прямоугольник.
pub(super) fn hit_row(
    ui: &mut Ui,
    index: usize,
    hit: &Hit,
    selected: bool,
    with_buttons: bool,
    pal: &Palette,
    actions: &mut Vec<Action>,
) -> Rect {
    // Фон рисуем после содержимого (когда известно, наведён ли курсор), поэтому резервируем место в списке фигур.
    let background = ui.painter().add(Shape::Noop);
    let inner = ui.scope_builder(UiBuilder::new().id_salt(&hit.path).sense(Sense::click()), |ui| {
        Frame::new().inner_margin(Margin::symmetric(SPACE_LG as i8, SPACE_MD as i8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.x = SPACE_MD;
            ui.horizontal_top(|ui| {
                let (color, label) = theme::badge(&hit.ext, pal.dark);
                theme::file_tile(ui, &label, color, 40.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = SPACE_XS;
                    ui.horizontal(|ui| {
                        let meta = format!("{} · {}", format_size(hit.size), format_time(hit.modified_ms));
                        // Правую часть (размер и дата) резервируем заранее, иначе длинное имя наезжает на неё.
                        let meta_width =
                            ui.painter().layout_no_wrap(meta.clone(), theme::font(FONT_CAPTION), pal.muted).size().x;
                        let name_width = (ui.available_width() - meta_width - SPACE_LG).max(80.0);
                        ui.scope(|ui| {
                            ui.set_max_width(name_width);
                            ui.add(egui::Label::new(theme::heading(pal, &hit.name)).truncate());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(theme::caption(pal, meta));
                        });
                    });
                    let folder =
                        fit_path(ui, &parent_of(&hit.path), ui.available_width(), theme::font(FONT_CAPTION), pal.muted);
                    ui.add(egui::Label::new(theme::caption(pal, folder)).truncate()).on_hover_text(&hit.path);
                    if let Some(note) = status_note(&hit.status) {
                        ui.label(RichText::new(note).size(FONT_CAPTION).color(pal.warn));
                    }
                    if let Some(snippet) = &hit.snippet {
                        ui.add_space(SPACE_XS);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = SPACE_SM;
                            if let Some(location) = &hit.location {
                                theme::chip(ui, location, pal.ok, pal.ok_soft);
                            }
                            let width = ui.available_width();
                            ui.add(egui::Label::new(snippet_job(snippet, pal, width)).wrap());
                        });
                    }
                    if with_buttons && selected {
                        // Узкое окно: панели справа нет, поэтому действия — прямо на карточке.
                        ui.add_space(SPACE_XS);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = SPACE_SM;
                            if theme::primary_button(ui, pal, "Просмотр")
                                .on_hover_text("Подробности и текст файла")
                                .clicked()
                            {
                                actions.push(Action::ShowDetails(index));
                            }
                            let openable = is_safe_to_open(std::path::Path::new(&hit.path));
                            if ui
                                .add_enabled_ui(openable, |ui| theme::button(ui, pal, "Открыть"))
                                .inner
                                .on_hover_text("Enter")
                                .on_disabled_hover_text("Исполняемые файлы не запускаются отсюда")
                                .clicked()
                            {
                                actions.push(Action::Open(index));
                            }
                            if theme::button(ui, pal, "В папке").on_hover_text("Shift+Enter").clicked() {
                                actions.push(Action::Reveal(index));
                            }
                            if theme::button(ui, pal, "Копировать путь").on_hover_text("Ctrl+C").clicked()
                            {
                                actions.push(Action::CopyPath(index));
                            }
                        });
                    }
                });
            });
        });
    });
    let response = inner.response;
    let rect = response.rect;

    let fill = if selected {
        pal.selected
    } else if response.hovered() {
        pal.hover
    } else {
        pal.surface
    };
    let stroke = if selected {
        Stroke::new(LINE_W + 0.5, pal.accent)
    } else if response.hovered() {
        Stroke::new(LINE_W, pal.muted.gamma_multiply(0.5))
    } else {
        Stroke::new(LINE_W, pal.line)
    };
    ui.painter().set(background, Shape::rect_filled(rect, theme::radius(RADIUS_MD), fill));
    ui.painter().rect_stroke(rect, theme::radius(RADIUS_MD), stroke, StrokeKind::Inside);

    if response.double_clicked() {
        actions.push(Action::Open(index));
    } else if response.clicked() || response.secondary_clicked() {
        actions.push(Action::Select(index));
    }
    response.context_menu(|ui| {
        let mut item = |ui: &mut Ui, text: &str, action: Action| {
            if ui.add(egui::Button::new(theme::body(pal, text)).frame(false).min_size(vec2(200.0, CONTROL_H))).clicked()
            {
                actions.push(action);
                ui.close();
            }
        };
        item(ui, "Открыть", Action::Open(index));
        item(ui, "Показать в папке", Action::Reveal(index));
        ui.separator();
        item(ui, "Копировать путь", Action::CopyPath(index));
        item(ui, "Копировать имя", Action::CopyName(index));
        ui.separator();
        item(ui, "Искать только в этой папке", Action::SearchInFolder(index));
    });
    rect
}
