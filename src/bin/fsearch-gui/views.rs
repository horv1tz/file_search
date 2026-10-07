//! Отрисовка главного окна: поисковая строка, фильтры, список результатов, строка состояния.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, ComboBox, Context, FontId, Frame, Id, Layout, Margin, Order, ProgressBar, RichText, Sense,
    Shape, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, vec2,
};
use file_search::platform::{format_size, format_time, is_safe_to_open};
use file_search::query::Mode;
use file_search::search::{Hit, Snippet, Sort};

use crate::app::{Action, App, SEARCH_ID};
use crate::theme::{self, GROUPS, Palette};

/// Ширина окна, начиная с которой справа показывается панель подробностей о выбранном файле.
const DETAILS_MIN_WIDTH: f32 = 900.0;
const DETAILS_WIDTH: f32 = 290.0;
/// Начальная ширина панели, когда в ней показан текст файла.
const PREVIEW_WIDTH: f32 = 520.0;

pub fn draw(ctx: &Context, app: &mut App, actions: &mut Vec<Action>) {
    let dark = ctx.style().visuals.dark_mode;
    let pal = theme::palette(dark);
    let fill = ctx.style().visuals.panel_fill;

    egui::TopBottomPanel::top("header")
        .frame(Frame::new().fill(fill).inner_margin(Margin { left: 16, right: 16, top: 14, bottom: 8 }))
        .show(ctx, |ui| header(ui, app, &pal, dark, actions));
    egui::TopBottomPanel::bottom("status")
        .frame(Frame::new().fill(fill).stroke(Stroke::new(1.0, pal.line)).inner_margin(Margin::symmetric(16, 6)))
        .show(ctx, |ui| status_bar(ui, app, &pal, actions));
    let details = ctx.content_rect().width() >= DETAILS_MIN_WIDTH
        && app.backend_error.is_none()
        && app.search_error.is_none()
        && !app.form.is_empty()
        && app.selected.is_some_and(|i| i < app.results.len());
    app.details_visible = details;
    if details {
        app.sync_preview(ctx);
    } else {
        app.preview = None;
    }
    if details {
        // Для текстового файла панель шире и растягивается: в ней виден сам текст.
        let panel = egui::SidePanel::right("details");
        let panel = if app.preview.is_some() {
            let max = (ctx.content_rect().width() * 0.7).max(DETAILS_WIDTH);
            panel.default_width(PREVIEW_WIDTH.min(max)).width_range(DETAILS_WIDTH..=max).resizable(true)
        } else {
            panel.exact_width(DETAILS_WIDTH).resizable(false)
        };
        panel
            .frame(Frame::new().fill(pal.card).stroke(Stroke::new(1.0, pal.line)).inner_margin(Margin {
                left: 16,
                right: 16,
                top: 16,
                bottom: 12,
            }))
            .show(ctx, |ui| details_panel(ui, app, &pal, dark, actions));
    }
    egui::CentralPanel::default()
        .frame(Frame::new().fill(fill).inner_margin(Margin::symmetric(16, 4)))
        .show(ctx, |ui| central(ui, app, &pal, dark, actions));
}

// ------------------------------------------------------------------ шапка

/// Лупа, нарисованная кистью: в системных шрифтах нужного значка может не оказаться.
fn magnifier(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    let painter = ui.painter();
    let stroke = Stroke::new(2.0, color);
    let center = rect.center() - vec2(1.5, 1.5);
    painter.circle_stroke(center, 6.0, stroke);
    painter.line_segment([center + vec2(4.3, 4.3), center + vec2(9.0, 9.0)], stroke);
}

/// Кнопка-«таблетка» фильтра: выбранная закрашена цветом акцента.
fn pill(ui: &mut Ui, text: String, on: bool, pal: &Palette, dark: bool) -> egui::Response {
    let (fill, fg, line) = if on {
        let fg = if dark { Color32::from_rgb(0x0b, 0x14, 0x2b) } else { Color32::WHITE };
        (pal.accent, fg, pal.accent)
    } else {
        (Color32::TRANSPARENT, ui.visuals().text_color(), pal.line)
    };
    ui.add(
        egui::Button::new(RichText::new(text).color(fg).size(13.5))
            .fill(fill)
            .stroke(Stroke::new(1.0, line))
            .corner_radius(14.0)
            .min_size(vec2(0.0, 28.0)),
    )
}

fn header(ui: &mut Ui, app: &mut App, pal: &Palette, dark: bool, actions: &mut Vec<Action>) {
    let mut changed = false;
    let search_id = Id::new(SEARCH_ID);
    let focused = ui.ctx().memory(|m| m.has_focus(search_id));
    ui.horizontal(|ui| {
        let settings_width = 118.0;
        let box_width = (ui.available_width() - settings_width - 14.0).max(220.0);
        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(Stroke::new(if focused { 1.6 } else { 1.0 }, if focused { pal.accent } else { pal.line }))
            .corner_radius(13.0)
            .inner_margin(Margin::symmetric(12, 5))
            .show(ui, |ui| {
                ui.set_width(box_width - 26.0);
                ui.horizontal(|ui| {
                    magnifier(ui, if focused { pal.accent } else { pal.muted });
                    let clear_width = 26.0;
                    let output = TextEdit::singleline(&mut app.form.query)
                        .id(search_id)
                        .frame(false)
                        .hint_text("Имя файла или слова из содержимого…")
                        .font(FontId::proportional(18.0))
                        .margin(Margin::symmetric(4, 6))
                        .desired_width((ui.available_width() - clear_width - 22.0).max(80.0))
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
                        let end = app.form.query.chars().count();
                        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(end))));
                        state.store(ui.ctx(), output.response.id);
                        app.focus_search = false;
                    }
                    changed |= output.response.changed();
                    if app.searching {
                        ui.spinner();
                    } else if !app.form.query.is_empty()
                        && ui
                            .add(egui::Button::new(RichText::new("×").size(18.0).color(pal.muted)).frame(false))
                            .on_hover_text("Очистить (Esc)")
                            .clicked()
                    {
                        app.form.query.clear();
                        changed = true;
                        app.focus_search = true;
                    }
                });
            });
        if ui
            .add_sized([settings_width, 38.0], egui::Button::new("Настройки").corner_radius(13.0))
            .on_hover_text("Папки для поиска, индексация, тема")
            .clicked()
        {
            actions.push(Action::OpenSettings);
        }
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (i, (name, _)) in GROUPS.iter().enumerate() {
            let on = app.form.groups[i];
            let count = app.facets.as_ref().and_then(|f| f.get(i)).filter(|_| !app.form.is_empty());
            let text = match count {
                Some(n) => format!("{name}  {n}"),
                None => (*name).to_string(),
            };
            if pill(ui, text, on, pal, dark).clicked() {
                app.form.groups[i] = !on;
                changed = true;
            }
        }
        ui.separator();
        let mode_label = |m: Mode| match m {
            Mode::All => "Везде",
            Mode::Name => "Только имена",
            Mode::Content => "Только содержимое",
        };
        ComboBox::from_id_salt("mode").selected_text(mode_label(app.form.mode)).show_ui(ui, |ui| {
            for m in [Mode::All, Mode::Name, Mode::Content] {
                changed |= ui.selectable_value(&mut app.form.mode, m, mode_label(m)).changed();
            }
        });
        let sort_label = |s: Sort| match s {
            Sort::Relevance => "По релевантности",
            Sort::Newest => "Сначала новые",
            Sort::Oldest => "Сначала старые",
            Sort::Largest => "Сначала большие",
            Sort::Smallest => "Сначала маленькие",
        };
        ComboBox::from_id_salt("sort").selected_text(sort_label(app.form.sort)).show_ui(ui, |ui| {
            for s in [Sort::Relevance, Sort::Newest, Sort::Oldest, Sort::Largest, Sort::Smallest] {
                changed |= ui.selectable_value(&mut app.form.sort, s, sort_label(s)).changed();
            }
        });
        ui.separator();
        match &app.form.dir {
            Some(dir) => {
                ui.label(format!("Папка: {}", shorten(dir, 44))).on_hover_text(dir.as_str());
                if ui.small_button("×").on_hover_text("Искать везде").clicked() {
                    actions.push(Action::ClearDirFilter);
                }
            }
            None => {
                if ui.button("Только в папке…").clicked() {
                    actions.push(Action::PickFilterFolder);
                }
            }
        }
    });
    if changed {
        app.mark_dirty(false);
    }
}

/// Укорачивает длинный путь, оставляя конец.
pub fn shorten(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - (max - 1)).collect();
    format!("…{tail}")
}

// ------------------------------------------------------------------ строка состояния

fn status_bar(ui: &mut Ui, app: &App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let left = if app.search_error.is_some() && !app.form.is_empty() {
            "Ошибка поиска".to_string()
        } else if app.num_docs > 0 {
            format!("В индексе файлов: {}", app.num_docs)
        } else {
            String::new()
        };
        ui.label(RichText::new(left).size(12.5).color(pal.muted));

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let Some(backend) = &app.backend else { return };
            let st = &backend.state;
            let response = if st.running.load(Ordering::Relaxed) {
                let total = st.total.load(Ordering::Relaxed);
                let done = st.done.load(Ordering::Relaxed);
                let inner = ui.horizontal(|ui| {
                    if total > 0 {
                        let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
                        ui.add(ProgressBar::new(frac).desired_width(150.0).text(format!("{done} / {total}")));
                        ui.label(RichText::new("Индексация").size(12.5).color(pal.accent));
                    } else {
                        ui.spinner();
                        let phase = st.phase();
                        let scanned = st.scanned.load(Ordering::Relaxed);
                        let text = match (phase.is_empty(), scanned) {
                            (true, _) => "Индексация…".to_string(),
                            (false, 0) => phase,
                            (false, n) => format!("{phase} · найдено файлов: {n}"),
                        };
                        ui.label(RichText::new(text).size(12.5).color(pal.accent));
                    }
                });
                inner.response
            } else if st.watching.load(Ordering::Relaxed) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Слежение за изменениями включено").size(12.5).color(pal.ok));
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, pal.ok);
                })
                .response
            } else if app.settings.roots.is_empty() {
                ui.label(RichText::new("Папки не выбраны").size(12.5).color(pal.warn))
            } else {
                ui.label(RichText::new("Автообновление выключено").size(12.5).color(pal.muted))
            };
            if response.interact(Sense::click()).on_hover_text("Открыть настройки индексации").clicked()
            {
                actions.push(Action::OpenSettings);
            }
            if let Some(error) = st.error.lock().unwrap().as_ref() {
                ui.label(RichText::new(format!("Внимание: {}", shorten(error, 60))).size(12.5).color(pal.warn))
                    .on_hover_text(error.as_str());
            }
        });
    });
}

// ------------------------------------------------------------------ центральная область

fn central(ui: &mut Ui, app: &mut App, pal: &Palette, dark: bool, actions: &mut Vec<Action>) {
    if let Some(error) = &app.backend_error {
        return index_error(ui, error, pal, actions);
    }
    if app.settings.roots.is_empty() && app.num_docs == 0 {
        return onboarding(ui, app, pal, actions);
    }
    if app.form.is_empty() {
        return welcome(ui, app, pal, actions);
    }
    if let Some(error) = &app.search_error {
        ui.add_space(20.0);
        ui.label(RichText::new(error).color(pal.danger));
        return;
    }
    if app.results.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            if app.pending() {
                ui.label(RichText::new("Поиск…").color(pal.muted));
            } else {
                ui.label(RichText::new("Ничего не найдено").size(18.0).strong());
                let indexing = app.backend.as_ref().is_some_and(|b| b.state.running.load(Ordering::Relaxed));
                let hint = if indexing {
                    "Индексация ещё идёт — файлы будут появляться в результатах по мере добавления."
                } else {
                    "Попробуйте другие слова, уберите фильтры или допишите * в конце слова: догов*"
                };
                ui.label(RichText::new(hint).color(pal.muted));
            }
        });
        return;
    }
    results_list(ui, app, pal, dark, actions);
}

fn results_list(ui: &mut Ui, app: &mut App, pal: &Palette, dark: bool, actions: &mut Vec<Action>) {
    let details_visible = app.details_visible;
    ui.add_space(2.0);
    ui.label(
        RichText::new(format!(
            "Найдено файлов: {} · показано {} · {:.0} мс",
            app.total,
            app.results.len(),
            app.took_ms.max(1.0)
        ))
        .size(12.5)
        .color(pal.muted),
    );
    let mut scroll = egui::ScrollArea::vertical().id_salt("results").auto_shrink([false, false]);
    if app.scroll_to_top {
        scroll = scroll.vertical_scroll_offset(0.0);
        app.scroll_to_top = false;
    }
    let output = scroll.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.add_space(4.0);
        let mut last_visible = false;
        for (i, hit) in app.results.iter().enumerate() {
            let selected = app.selected == Some(i);
            let rect = hit_row(ui, i, hit, selected, !details_visible, pal, dark, actions);
            if selected && app.scroll_to_selected {
                ui.scroll_to_rect(rect, None);
            }
            if i + 1 == app.results.len() {
                last_visible = ui.is_rect_visible(rect);
            }
        }
        ui.add_space(8.0);
        last_visible
    });
    app.scroll_to_selected = false;
    if output.inner {
        app.load_more();
    }
}

/// Значок файла: цветная плитка с расширением.
fn file_tile(ui: &mut Ui, label: &str, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, size * 0.24, color);
    let text: String = label.chars().take(4).collect();
    let font = (size * 0.27).clamp(9.5, 15.0);
    let galley = ui.painter().layout_no_wrap(text, FontId::proportional(font), Color32::WHITE);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

/// Небольшая рамка с текстом: «Лист «Продажи»», «Слайд 3».
fn chip(ui: &mut Ui, text: &str, color: Color32, line: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(12.0), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(12.0, 4.0), Sense::hover());
    ui.painter().rect_stroke(rect, 5.0, Stroke::new(1.0, line), StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
}

fn snippet_job(snippet: &Snippet, pal: &Palette, text_color: Color32, width: f32) -> LayoutJob {
    let font = FontId::proportional(14.0);
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    let normal = TextFormat { font_id: font.clone(), color: text_color, ..Default::default() };
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

/// Папка, в которой лежит файл (без имени самого файла).
fn parent_of(path: &str) -> String {
    std::path::Path::new(path).parent().map(|p| p.display().to_string()).unwrap_or_else(|| path.to_string())
}

/// Пояснение к файлам, у которых текста нет: пароль или скан.
fn status_note(status: &str) -> Option<&'static str> {
    if status == "encrypted" {
        Some("Файл защищён паролем — найден только по имени")
    } else if status.starts_with("skipped: скан") {
        Some("Скан без текстового слоя — найден только по имени")
    } else {
        None
    }
}

/// Одна карточка результата. Возвращает её прямоугольник.
#[allow(clippy::too_many_arguments)]
fn hit_row(
    ui: &mut Ui,
    index: usize,
    hit: &Hit,
    selected: bool,
    with_buttons: bool,
    pal: &Palette,
    dark: bool,
    actions: &mut Vec<Action>,
) -> egui::Rect {
    // Фон рисуем после содержимого (когда известно, наведён ли курсор), поэтому резервируем место в списке фигур.
    let background = ui.painter().add(Shape::Noop);
    let inner = ui.scope_builder(UiBuilder::new().id_salt(&hit.path).sense(Sense::click()), |ui| {
        Frame::new().inner_margin(Margin::symmetric(14, 12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let (color, label) = theme::badge(&hit.ext, dark);
                file_tile(ui, &label, color, 42.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.horizontal(|ui| {
                        let meta =
                            RichText::new(format!("{} · {}", format_size(hit.size), format_time(hit.modified_ms)))
                                .size(12.5)
                                .color(pal.muted);
                        // Правую часть (размер и дата) резервируем заранее, иначе длинное имя наезжает на неё.
                        let meta_width = ui
                            .painter()
                            .layout_no_wrap(meta.text().to_string(), FontId::proportional(12.5), pal.muted)
                            .size()
                            .x;
                        let name_width = (ui.available_width() - meta_width - 16.0).max(80.0);
                        ui.scope(|ui| {
                            ui.set_max_width(name_width);
                            ui.add(egui::Label::new(RichText::new(&hit.name).strong().size(15.5)).truncate());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(meta);
                        });
                    });
                    ui.add(
                        egui::Label::new(RichText::new(parent_of(&hit.path)).size(12.5).color(pal.muted)).truncate(),
                    );
                    if let Some(note) = status_note(&hit.status) {
                        ui.label(RichText::new(note).size(12.5).color(pal.warn));
                    }
                    if let Some(snippet) = &hit.snippet {
                        ui.add_space(3.0);
                        ui.horizontal_top(|ui| {
                            if let Some(location) = &hit.location {
                                chip(ui, location, pal.ok, pal.line);
                            }
                            let text_color = ui.visuals().text_color();
                            let width = ui.available_width();
                            ui.add(egui::Label::new(snippet_job(snippet, pal, text_color, width)).wrap());
                        });
                    }
                    if with_buttons && selected {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let openable = is_safe_to_open(std::path::Path::new(&hit.path));
                            if ui
                                .add_enabled(openable, egui::Button::new("Открыть").small())
                                .on_disabled_hover_text("Исполняемые файлы не запускаются отсюда")
                                .clicked()
                            {
                                actions.push(Action::Open(index));
                            }
                            if ui.add(egui::Button::new("В папке").small()).clicked() {
                                actions.push(Action::Reveal(index));
                            }
                            if ui.add(egui::Button::new("Копировать путь").small()).clicked() {
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
        pal.card_selected
    } else if response.hovered() {
        pal.card_hover
    } else {
        pal.card
    };
    let stroke = Stroke::new(if selected { 1.5 } else { 1.0 }, if selected { pal.accent } else { pal.line });
    ui.painter().set(background, Shape::rect_filled(rect, 10.0, fill));
    ui.painter().rect_stroke(rect, 10.0, stroke, StrokeKind::Inside);

    if response.double_clicked() {
        actions.push(Action::Open(index));
    } else if response.clicked() || response.secondary_clicked() {
        actions.push(Action::Select(index));
    }
    response.context_menu(|ui| {
        if ui.button("Открыть").clicked() {
            actions.push(Action::Open(index));
            ui.close();
        }
        if ui.button("Показать в папке").clicked() {
            actions.push(Action::Reveal(index));
            ui.close();
        }
        ui.separator();
        if ui.button("Копировать путь").clicked() {
            actions.push(Action::CopyPath(index));
            ui.close();
        }
        if ui.button("Копировать имя").clicked() {
            actions.push(Action::CopyName(index));
            ui.close();
        }
        ui.separator();
        if ui.button("Искать только в этой папке").clicked() {
            actions.push(Action::SearchInFolder(index));
            ui.close();
        }
    });
    rect
}

/// Панель справа: подробности о выбранном файле и все действия над ним.
fn details_panel(ui: &mut Ui, app: &mut App, pal: &Palette, dark: bool, actions: &mut Vec<Action>) {
    let Some(index) = app.selected else { return };
    let Some(hit) = app.results.get(index).cloned() else { return };
    if app.preview.is_some() {
        return text_details(ui, app, &hit, index, pal, dark, actions);
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let (color, label) = theme::badge(&hit.ext, dark);
            file_tile(ui, &label, color, 52.0);
            ui.vertical(|ui| {
                ui.add(egui::Label::new(RichText::new(&hit.name).strong().size(16.5)).wrap());
            });
        });
        ui.add_space(12.0);
        let fact = |ui: &mut Ui, title: &str, value: String| {
            ui.label(RichText::new(title).size(11.5).color(pal.muted));
            ui.add(egui::Label::new(RichText::new(value).size(13.5)).wrap());
            ui.add_space(5.0);
        };
        fact(ui, "Папка", parent_of(&hit.path));
        fact(ui, "Размер", format_size(hit.size));
        fact(ui, "Изменён", format_time(hit.modified_ms));
        if let Some(location) = &hit.location {
            fact(ui, "Найдено", location.clone());
        }
        if let Some(note) = status_note(&hit.status) {
            ui.label(RichText::new(note).size(12.5).color(pal.warn));
            ui.add_space(5.0);
        }
        if let Some(snippet) = &hit.snippet {
            ui.label(RichText::new("Фрагмент").size(11.5).color(pal.muted));
            Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(9.0).inner_margin(Margin::same(10)).show(
                ui,
                |ui| {
                    let text_color = ui.visuals().text_color();
                    let width = ui.available_width();
                    ui.add(egui::Label::new(snippet_job(snippet, pal, text_color, width)).wrap());
                },
            );
            ui.add_space(8.0);
        }
        let width = ui.available_width();
        let openable = is_safe_to_open(std::path::Path::new(&hit.path));
        let on_accent = if dark { Color32::from_rgb(0x0b, 0x14, 0x2b) } else { Color32::WHITE };
        if ui
            .add_enabled(
                openable,
                egui::Button::new(RichText::new("Открыть").strong().color(on_accent))
                    .fill(pal.accent)
                    .corner_radius(10.0)
                    .min_size(vec2(width, 34.0)),
            )
            .on_disabled_hover_text("Исполняемые файлы не запускаются отсюда")
            .clicked()
        {
            actions.push(Action::Open(index));
        }
        for (text, action) in [
            ("Показать в папке", Action::Reveal(index)),
            ("Копировать путь", Action::CopyPath(index)),
            ("Искать только в этой папке", Action::SearchInFolder(index)),
        ] {
            if ui.add(egui::Button::new(text).corner_radius(10.0).min_size(vec2(width, 30.0))).clicked() {
                actions.push(action);
            }
        }
    });
}

/// Панель для текстового файла: коротко о файле, кнопки и под ними сам текст, как в редакторе.
fn text_details(
    ui: &mut Ui,
    app: &mut App,
    hit: &Hit,
    index: usize,
    pal: &Palette,
    dark: bool,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_top(|ui| {
        let (color, label) = theme::badge(&hit.ext, dark);
        file_tile(ui, &label, color, 40.0);
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(&hit.name).strong().size(15.5)).truncate());
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{} · {}", format_size(hit.size), format_time(hit.modified_ms)))
                        .size(12.0)
                        .color(pal.muted),
                )
                .truncate(),
            );
            ui.add(egui::Label::new(RichText::new(parent_of(&hit.path)).size(12.0).color(pal.muted)).truncate())
                .on_hover_text(&hit.path);
        });
    });
    ui.add_space(8.0);
    let on_accent = if dark { Color32::from_rgb(0x0b, 0x14, 0x2b) } else { Color32::WHITE };
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                is_safe_to_open(std::path::Path::new(&hit.path)),
                egui::Button::new(RichText::new("Открыть").strong().color(on_accent))
                    .fill(pal.accent)
                    .corner_radius(8.0),
            )
            .on_hover_text("Открыть в программе по умолчанию (для редактирования)")
            .on_disabled_hover_text("Исполняемые файлы не запускаются отсюда")
            .clicked()
        {
            actions.push(Action::Open(index));
        }
        for (text, action) in [
            ("В папке", Action::Reveal(index)),
            ("Копировать путь", Action::CopyPath(index)),
            ("Искать в этой папке", Action::SearchInFolder(index)),
        ] {
            if ui.add(egui::Button::new(text).corner_radius(8.0)).clicked() {
                actions.push(action);
            }
        }
    });
    ui.add_space(6.0);
    if let Some(preview) = &mut app.preview {
        preview.show(ui, pal, dark);
    }
}

// ------------------------------------------------------------------ пустые состояния

fn card(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.line))
        .corner_radius(12.0)
        .inner_margin(Margin::symmetric(24, 20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn index_error(ui: &mut Ui, error: &str, pal: &Palette, actions: &mut Vec<Action>) {
    ui.add_space(24.0);
    card(ui, pal, |ui| {
        ui.label(RichText::new("Не удалось открыть индекс").size(20.0).strong().color(pal.danger));
        ui.add_space(6.0);
        ui.label(error);
        ui.add_space(10.0);
        ui.label(RichText::new("Если индекс создан другой версией программы, его нужно пересоздать — файлы будут проиндексированы заново.").color(pal.muted));
        ui.add_space(10.0);
        if ui.button("Пересоздать индекс").clicked() {
            actions.push(Action::RecreateIndex);
        }
    });
}

fn onboarding(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.add_space(24.0);
    card(ui, pal, |ui| {
        ui.label(RichText::new("Добро пожаловать!").size(24.0).strong());
        ui.add_space(6.0);
        ui.label("Это поиск по файлам: он находит документы не только по названию, но и по тексту внутри — Word, Excel, PowerPoint, PDF и обычные текстовые файлы.");
        ui.add_space(14.0);
        ui.label(RichText::new("Выберите, где искать").strong().size(16.0));
        ui.label(
            RichText::new("Программа один раз прочитает файлы, а потом будет следить за изменениями сама.")
                .color(pal.muted),
        );
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for (name, path) in &app.places {
                if ui.button(name).on_hover_text(path.display().to_string()).clicked() {
                    actions.push(Action::AddRoot(path.clone()));
                }
            }
            if ui.button("Выбрать папку…").clicked() {
                actions.push(Action::PickRoot);
            }
        });
        ui.add_space(12.0);
        ui.label(RichText::new("Папка на другом компьютере").strong());
        network_input(ui, app, pal, actions);
    });
}

/// Поле для адреса сетевой папки: `\\192.168.1.10\документы`.
pub fn network_input(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let mut submit = false;
    ui.horizontal(|ui| {
        let response = TextEdit::singleline(&mut app.net_input)
            .hint_text("\\\\192.168.1.10\\документы")
            .desired_width((ui.available_width() - 130.0).clamp(160.0, 420.0))
            .show(ui)
            .response;
        if response.changed() {
            app.net_error = None;
        }
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            submit = true;
            // Enter снимает фокус с поля; возвращаем его, чтобы можно было сразу поправить адрес.
            response.request_focus();
        }
        let label = if app.net_checking { "Проверяю…" } else { "Добавить" };
        let ready = !app.net_checking && !app.net_input.trim().is_empty();
        submit |= ui.add_enabled(ready, egui::Button::new(label)).clicked();
    });
    if submit && !app.net_checking && !app.net_input.trim().is_empty() {
        actions.push(Action::AddRootText(app.net_input.clone()));
    }
    if let Some(error) = &app.net_error {
        ui.label(RichText::new(error).size(12.5).color(pal.danger));
    } else {
        ui.label(
            RichText::new("Нужен доступ к папке из проводника: если спросит пароль, один раз введите его там.")
                .size(12.5)
                .color(pal.muted),
        );
    }
}

const EXAMPLES: [(&str, &str); 8] = [
    ("договор аренды", "оба слова"),
    ("\"срок действия\"", "точная фраза"),
    ("отчёт NOT черновик", "исключить слово"),
    ("догов*", "начало слова"),
    ("аренда~", "с опечатками"),
    ("аренда OR лизинг", "любое из слов"),
    ("name:смета", "только в имени файла"),
    ("content:смета", "только в тексте внутри файлов"),
];

fn welcome(ui: &mut Ui, app: &App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.add_space(20.0);
    card(ui, pal, |ui| {
        let indexing = app.backend.as_ref().is_some_and(|b| b.state.running.load(Ordering::Relaxed));
        ui.label(RichText::new("Начните вводить запрос").size(22.0).strong());
        ui.add_space(4.0);
        let status = if indexing && app.num_docs == 0 {
            "Идёт первичная индексация — она может занять несколько минут, искать можно уже сейчас.".to_string()
        } else if app.num_docs > 0 {
            format!("В индексе файлов: {}. Поиск учитывает словоформы и опечатки.", app.num_docs)
        } else {
            "Индекс пока пуст.".to_string()
        };
        ui.label(RichText::new(status).color(pal.muted));
        ui.add_space(12.0);
        ui.label(RichText::new("Примеры запросов (нажмите, чтобы попробовать)").strong());
        ui.add_space(4.0);
        egui::Grid::new("examples").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for (code, description) in EXAMPLES {
                if ui.button(RichText::new(code).monospace()).clicked() {
                    actions.push(Action::UseExample(code.to_string()));
                }
                ui.label(RichText::new(description).color(pal.muted));
                ui.end_row();
            }
        });
        ui.add_space(10.0);
        ui.label(RichText::new("Горячие клавиши: стрелки вверх и вниз — выбор, Enter — открыть, Shift+Enter — показать в папке, Ctrl+F — в строку поиска, Esc — очистить").size(12.5).color(pal.muted));
    });
}

// ------------------------------------------------------------------ всплывающее сообщение

pub fn toast(ctx: &Context, toast: &mut Option<(String, Instant)>) {
    const LIFETIME: Duration = Duration::from_secs(3);
    let Some((text, since)) = toast.as_ref() else { return };
    let age = since.elapsed();
    if age >= LIFETIME {
        *toast = None;
        return;
    }
    egui::Area::new(Id::new("toast"))
        .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -48.0))
        .order(Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(text);
            });
        });
    ctx.request_repaint_after(LIFETIME - age);
}
