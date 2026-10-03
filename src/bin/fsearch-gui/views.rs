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

pub fn draw(ctx: &Context, app: &mut App, actions: &mut Vec<Action>) {
    let dark = ctx.style().visuals.dark_mode;
    let pal = theme::palette(dark);
    let fill = ctx.style().visuals.panel_fill;

    egui::TopBottomPanel::top("header")
        .frame(Frame::new().fill(fill).inner_margin(Margin { left: 16, right: 16, top: 14, bottom: 8 }))
        .show(ctx, |ui| header(ui, app, actions));
    egui::TopBottomPanel::bottom("status")
        .frame(Frame::new().fill(fill).stroke(Stroke::new(1.0, pal.line)).inner_margin(Margin::symmetric(16, 6)))
        .show(ctx, |ui| status_bar(ui, app, &pal, actions));
    egui::CentralPanel::default()
        .frame(Frame::new().fill(fill).inner_margin(Margin::symmetric(16, 4)))
        .show(ctx, |ui| central(ui, app, &pal, dark, actions));
}

// ------------------------------------------------------------------ шапка

fn header(ui: &mut Ui, app: &mut App, actions: &mut Vec<Action>) {
    let mut changed = false;
    ui.horizontal(|ui| {
        let button_width = 110.0;
        let output = TextEdit::singleline(&mut app.form.query)
            .id(Id::new(SEARCH_ID))
            .hint_text("Имя файла или слова из содержимого…")
            .font(FontId::proportional(18.0))
            .margin(Margin::symmetric(10, 8))
            .desired_width((ui.available_width() - button_width - 30.0).max(160.0))
            .show(ui);
        if output.response.has_focus() {
            // Esc очищает запрос, а не уводит фокус из поля: иначе дальнейший набор уходил бы в пустоту.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    output.response.id,
                    egui::EventFilter { escape: true, horizontal_arrows: true, vertical_arrows: true, tab: false },
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
        }
        if ui.add_sized([button_width, 36.0], egui::Button::new("Индексация")).clicked() {
            actions.push(Action::OpenSettings);
        }
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (i, (name, _)) in GROUPS.iter().enumerate() {
            let on = app.form.groups[i];
            if ui.add(egui::Button::new(*name).selected(on).min_size(vec2(0.0, 26.0))).clicked() {
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
        let left = if app.form.is_empty() {
            if app.num_docs > 0 { format!("В индексе файлов: {}", app.num_docs) } else { String::new() }
        } else if app.pending() && app.results.is_empty() {
            "Поиск…".to_string()
        } else if app.search_error.is_some() {
            "Ошибка поиска".to_string()
        } else {
            let shown = app.results.len();
            format!("Найдено файлов: {} · показано {} · {:.0} мс", app.total, shown, app.took_ms.max(1.0))
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
            let rect = hit_row(ui, i, hit, selected, pal, dark, actions);
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

/// Значок типа файла (цветная «таблетка» с расширением).
fn badge(ui: &mut Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(11.0), Color32::WHITE);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(12.0, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, 5.0, color);
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

/// Одна карточка результата. Возвращает её прямоугольник.
fn hit_row(
    ui: &mut Ui,
    index: usize,
    hit: &Hit,
    selected: bool,
    pal: &Palette,
    dark: bool,
    actions: &mut Vec<Action>,
) -> egui::Rect {
    // Фон рисуем после содержимого (когда известно, наведён ли курсор), поэтому резервируем место в списке фигур.
    let background = ui.painter().add(Shape::Noop);
    let inner = ui.scope_builder(UiBuilder::new().id_salt(&hit.path).sense(Sense::click()), |ui| {
        Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (color, label) = theme::badge(&hit.ext, dark);
                badge(ui, &label, color);
                let meta = RichText::new(format!("{} · {}", format_size(hit.size), format_time(hit.modified_ms)))
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
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(&hit.path).size(12.5).color(pal.muted)).truncate());
            });
            if hit.status == "encrypted" {
                ui.label(RichText::new("Файл защищён паролем — найден только по имени").size(12.5).color(pal.warn));
            }
            if let Some(snippet) = &hit.snippet {
                ui.add_space(2.0);
                ui.horizontal_top(|ui| {
                    if let Some(location) = &hit.location {
                        chip(ui, location, pal.ok, pal.line);
                    }
                    let text_color = ui.visuals().text_color();
                    let width = ui.available_width();
                    ui.add(egui::Label::new(snippet_job(snippet, pal, text_color, width)).wrap());
                });
            }
            ui.add_space(2.0);
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
    ui.painter().set(background, Shape::rect_filled(rect, 9.0, fill));
    ui.painter().rect_stroke(rect, 9.0, stroke, StrokeKind::Inside);

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
