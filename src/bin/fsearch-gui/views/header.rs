//! Верхняя панель: строка поиска, режимы, фильтры и сортировка.

use super::*;
use crate::app::{Action, App, SEARCH_ID};
use crate::theme::{self, CONTROL_H, GROUPS, LINE_W, Palette, RADIUS_LG, SEARCH_H, SPACE_MD, SPACE_SM, SPACE_XS};
use egui::text::{CCursor, CCursorRange};
use egui::{Color32, ComboBox, Frame, Id, Margin, RichText, Sense, Stroke, TextEdit, Ui, vec2};
use file_search::query::Mode;
use file_search::search::Sort;

/// Лупа, нарисованная кистью: в системных шрифтах нужного значка может не оказаться.
pub(super) fn magnifier(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let painter = ui.painter();
    let stroke = Stroke::new(2.0, color);
    let center = rect.center() - vec2(2.0, 2.0);
    painter.circle_stroke(center, 6.5, stroke);
    painter.line_segment([center + vec2(4.7, 4.7), center + vec2(9.5, 9.5)], stroke);
}

pub(super) fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::All => "Везде",
        Mode::Name => "Только имена",
        Mode::Content => "Только содержимое",
    }
}

pub(super) fn sort_label(s: Sort) -> &'static str {
    match s {
        Sort::Relevance => "По соответствию",
        Sort::Newest => "Сначала новые",
        Sort::Oldest => "Сначала старые",
        Sort::Largest => "Сначала большие",
        Sort::Smallest => "Сначала маленькие",
    }
}

/// Тонкая вертикальная черта между группами фильтров.
pub(super) fn vertical_line(ui: &mut Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(LINE_W, CONTROL_H - SPACE_SM), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(LINE_W, pal.line));
}

pub(super) fn header(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let mut changed = false;
    let search_id = Id::new(SEARCH_ID);
    let focused = ui.ctx().memory(|m| m.has_focus(search_id));
    ui.horizontal(|ui| {
        let settings_width = 116.0;
        let box_width = (ui.available_width() - settings_width - SPACE_MD).max(220.0);
        Frame::new()
            .fill(pal.surface)
            .stroke(Stroke::new(if focused { 2.0 } else { LINE_W }, if focused { pal.accent } else { pal.line }))
            .corner_radius(theme::radius(RADIUS_LG))
            .inner_margin(Margin::symmetric(SPACE_MD as i8, 5))
            .show(ui, |ui| {
                ui.set_width(box_width - 2.0 * SPACE_MD - 4.0);
                ui.set_min_height(SEARCH_H - 10.0 - 4.0);
                ui.horizontal_centered(|ui| {
                    magnifier(ui, if focused { pal.accent } else { pal.muted });
                    let side = CONTROL_H + SPACE_XS;
                    let output = TextEdit::singleline(&mut app.search.form.query)
                        .id(search_id)
                        .frame(false)
                        .hint_text(RichText::new("Имя файла или слова из содержимого…").color(pal.muted))
                        .font(theme::font(theme::FONT_TITLE))
                        .text_color(pal.text)
                        .margin(Margin::symmetric(SPACE_XS as i8, 6))
                        .desired_width((ui.available_width() - side - SPACE_SM).max(80.0))
                        .show(ui);
                    if output.response.has_focus() {
                        // Esc очищает запрос, а не уводит фокус из поля: иначе дальнейший набор уходил бы в пустоту.
                        ui.memory_mut(|m| {
                            m.set_focus_lock_filter(
                                output.response.id,
                                egui::EventFilter {
                                    escape: true,
                                    horizontal_arrows: true,
                                    vertical_arrows: true,
                                    tab: false,
                                },
                            )
                        });
                    }
                    if app.focus_search {
                        output.response.request_focus();
                        // При Ctrl+F выделяем весь запрос, чтобы его можно было сразу заменить.
                        let mut state = output.state.clone();
                        let end = app.search.form.query.chars().count();
                        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(end))));
                        state.store(ui.ctx(), output.response.id);
                        app.focus_search = false;
                    }
                    changed |= output.response.changed();
                    if app.search.searching {
                        ui.add_sized([CONTROL_H, CONTROL_H], egui::Spinner::new().size(18.0));
                    } else if !app.search.form.query.is_empty()
                        && theme::close_button(ui, pal).on_hover_text("Очистить (Esc)").clicked()
                    {
                        app.search.form.query.clear();
                        changed = true;
                        app.focus_search = true;
                    }
                });
            });
        if theme::button(ui, pal, "Настройки").on_hover_text("Папки для поиска, индексация, тема").clicked()
        {
            actions.push(Action::OpenSettings);
        }
    });
    // Пока папки не выбраны, фильтровать нечего: остаётся одна строка поиска.
    if app.settings.roots.is_empty() && app.index.num_docs == 0 {
        if changed {
            app.mark_dirty(false);
        }
        return;
    }
    ui.add_space(SPACE_SM);
    if ui.available_width() >= FILTERS_ONE_ROW {
        ui.horizontal(|ui| {
            filter_pills(ui, app, pal, &mut changed);
            vertical_line(ui, pal);
            filter_controls(ui, app, pal, actions, &mut changed);
        });
    } else {
        ui.horizontal_wrapped(|ui| filter_pills(ui, app, pal, &mut changed));
        ui.horizontal(|ui| filter_controls(ui, app, pal, actions, &mut changed));
    }
    if changed {
        app.mark_dirty(false);
    }
}

/// Фильтры по типам файлов.
pub(super) fn filter_pills(ui: &mut Ui, app: &mut App, pal: &Palette, changed: &mut bool) {
    for (i, (name, _)) in GROUPS.iter().enumerate() {
        let on = app.search.form.groups[i];
        let count = app.search.facets.as_ref().and_then(|f| f.get(i)).filter(|_| !app.search.form.is_empty());
        let text = match count {
            Some(n) => format!("{name}  {n}"),
            None => (*name).to_string(),
        };
        // Тип, которого нет среди найденного, включать бессмысленно: таблетка гаснет (выключить её можно всегда).
        let enabled = on || count != Some(&0);
        if ui
            .add_enabled_ui(enabled, |ui| theme::pill(ui, pal, &text, on))
            .inner
            .on_hover_text(format!("Только {name} (Ctrl+{})", i + 1))
            .clicked()
        {
            app.search.form.groups[i] = !on;
            *changed = true;
        }
    }
}

/// Режим поиска, порядок и папка.
pub(super) fn filter_controls(
    ui: &mut Ui,
    app: &mut App,
    pal: &Palette,
    actions: &mut Vec<Action>,
    changed: &mut bool,
) {
    ComboBox::from_id_salt("mode").width(150.0).selected_text(mode_label(app.search.form.mode)).show_ui(ui, |ui| {
        for m in [Mode::All, Mode::Name, Mode::Content] {
            *changed |= ui.selectable_value(&mut app.search.form.mode, m, mode_label(m)).changed();
        }
    });
    ComboBox::from_id_salt("sort").width(170.0).selected_text(sort_label(app.search.form.sort)).show_ui(ui, |ui| {
        for s in [Sort::Relevance, Sort::Newest, Sort::Oldest, Sort::Largest, Sort::Smallest] {
            *changed |= ui.selectable_value(&mut app.search.form.sort, s, sort_label(s)).changed();
        }
    });
    match &app.search.form.dir {
        Some(dir) => {
            let name = std::path::Path::new(dir)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.clone());
            if theme::pill(ui, pal, &format!("Папка: {}   ×", shorten(&name, 20)), true)
                .on_hover_text(format!("{dir}\nНажмите, чтобы искать везде"))
                .clicked()
            {
                actions.push(Action::ClearDirFilter);
            }
        }
        None => {
            if theme::button(ui, pal, "Только в папке…").on_hover_text("Искать внутри одной папки").clicked()
            {
                actions.push(Action::PickFilterFolder);
            }
        }
    }
}
