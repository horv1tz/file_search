//! Отрисовка главного окна: поисковая строка, фильтры, список результатов, подробности, строка состояния.

use std::sync::atomic::Ordering;
use std::time::Duration;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, ComboBox, Context, FontId, Frame, Id, Layout, Margin, Order, Rect, RichText, Sense, Shape,
    Stroke, StrokeKind, TextEdit, Ui, UiBuilder, pos2, vec2,
};
use file_search::platform::{format_size, format_time, is_safe_to_open};
use file_search::query::Mode;
use file_search::search::{Hit, Snippet, Sort};

use crate::app::{Action, App, SEARCH_ID, Toast};
use crate::theme::{
    self, CONTROL_H, FONT_BODY, FONT_CAPTION, FONT_SMALL, GROUPS, Icon, Kind, LINE_W, Palette, RADIUS_LG, RADIUS_MD,
    RADIUS_SM, SEARCH_H, SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XL, SPACE_XS,
};
use crate::updater::Phase;

/// Ширина окна, начиная с которой справа показывается панель подробностей о выбранном файле.
const DETAILS_MIN_WIDTH: f32 = 900.0;
const DETAILS_WIDTH: f32 = 300.0;
/// Начальная ширина панели, когда в ней показан текст файла.
const PREVIEW_WIDTH: f32 = 520.0;
/// Ширина колонки с пустыми состояниями.
const COLUMN_WIDTH: f32 = 680.0;
/// С какой ширины фильтры по типам и остальные элементы помещаются в одну строку.
const FILTERS_ONE_ROW: f32 = 960.0;

fn margin(left: f32, top: f32, right: f32, bottom: f32) -> Margin {
    Margin { left: left as i8, right: right as i8, top: top as i8, bottom: bottom as i8 }
}

pub fn draw(ctx: &Context, app: &mut App, actions: &mut Vec<Action>) {
    let pal = theme::palette(ctx.style().visuals.dark_mode);
    let wide = ctx.content_rect().width() >= DETAILS_MIN_WIDTH;
    let can_show = app.backend_error.is_none()
        && app.search_error.is_none()
        && !app.form.is_empty()
        && app.selected.is_some_and(|i| i < app.results.len());
    if wide {
        app.narrow_details = false;
    }
    // Узкое окно: подробности открываются вторым экраном поверх списка.
    let second_screen = !wide && can_show && app.narrow_details;
    let side_panel = wide && can_show;
    app.details_visible = side_panel;
    if side_panel || second_screen {
        app.sync_preview(ctx);
    } else {
        app.preview = None;
        app.narrow_details = false;
    }

    if second_screen {
        egui::TopBottomPanel::top("back")
            .frame(Frame::new().fill(pal.bg).inner_margin(margin(SPACE_LG, SPACE_SM, SPACE_LG, SPACE_SM)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if theme::button(ui, &pal, "‹  Назад к списку").on_hover_text("Esc").clicked() {
                        actions.push(Action::BackToList);
                    }
                    if app.total > 0 {
                        ui.label(theme::caption(&pal, format!("Найдено файлов: {}", thousands(app.total as u64))));
                    }
                });
            });
    } else {
        egui::TopBottomPanel::top("header")
            .frame(Frame::new().fill(pal.bg).inner_margin(margin(SPACE_LG, SPACE_MD, SPACE_LG, SPACE_SM)))
            .show(ctx, |ui| header(ui, app, &pal, actions));
    }
    if update_banner_visible(app) {
        let kind = match &app.updater.phase {
            Phase::Failed { .. } => Kind::Error,
            Phase::Ready { .. } => Kind::Success,
            _ => Kind::Info,
        };
        egui::TopBottomPanel::top("update")
            .frame(Frame::new().fill(kind.soft(&pal)).inner_margin(margin(SPACE_LG, SPACE_SM, SPACE_LG, SPACE_SM)))
            .show(ctx, |ui| update_banner(ui, app, &pal, actions));
    }
    egui::TopBottomPanel::bottom("status")
        .frame(Frame::new().fill(pal.bg).inner_margin(margin(SPACE_LG, SPACE_XS, SPACE_LG, SPACE_XS)))
        .show(ctx, |ui| status_bar(ui, app, &pal, actions));
    if side_panel {
        // Для текстового файла панель шире и растягивается: в ней виден сам текст.
        let panel = egui::SidePanel::right("details");
        let panel = if app.preview.is_some() {
            let max = (ctx.content_rect().width() * 0.7).max(DETAILS_WIDTH);
            panel.default_width(PREVIEW_WIDTH.min(max)).width_range(DETAILS_WIDTH..=max).resizable(true)
        } else {
            panel.exact_width(DETAILS_WIDTH).resizable(false)
        };
        // Границу панели рисует сама панель (одна линия), рамка без обводки.
        panel
            .frame(Frame::new().fill(pal.surface).inner_margin(margin(SPACE_LG, SPACE_LG, SPACE_LG, SPACE_MD)))
            .show(ctx, |ui| details_panel(ui, app, &pal, actions));
    }
    if second_screen {
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.surface).inner_margin(margin(SPACE_LG, SPACE_LG, SPACE_LG, SPACE_MD)))
            .show(ctx, |ui| details_panel(ui, app, &pal, actions));
    } else {
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.bg).inner_margin(margin(SPACE_LG, SPACE_XS, SPACE_LG, 0.0)))
            .show(ctx, |ui| central(ui, app, &pal, actions));
    }
}

// ------------------------------------------------------------------ обновление

fn update_banner_visible(app: &App) -> bool {
    !app.updater.dismissed
        && match &app.updater.phase {
            Phase::Available(_) | Phase::Downloading { .. } | Phase::Ready { .. } => true,
            // Ошибка показывается в полосе, только если обновление уже пытались поставить.
            Phase::Failed { release, .. } => release.is_some(),
            Phase::Idle | Phase::Checking | Phase::UpToDate => false,
        }
}

/// Полоса под шапкой: «Доступна новая версия», ход загрузки, предложение перезапуститься.
fn update_banner(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let bold = |text: String| theme::body(pal, text);
    ui.horizontal_wrapped(|ui| match &app.updater.phase {
        Phase::Available(release) => {
            ui.label(bold(format!("Доступна новая версия {}", release.version)));
            ui.label(theme::caption(pal, format!("(у вас {})", file_search::update::Version::current())));
            let installable = release.installable_asset().is_some();
            if theme::primary_button(ui, pal, if installable { "Обновить" } else { "Скачать" }).clicked()
            {
                actions.push(Action::DownloadUpdate);
            }
            if theme::button(ui, pal, "Что нового").clicked() {
                actions.push(Action::OpenUrl(release.url.clone()));
            }
            if theme::button(ui, pal, "Позже").clicked() {
                actions.push(Action::DismissUpdate);
            }
            if theme::ghost_button(ui, pal, "Пропустить эту версию")
                .on_hover_text("Не напоминать об этой версии; о следующей сообщим")
                .clicked()
            {
                actions.push(Action::SkipUpdateVersion);
            }
        }
        Phase::Downloading { release, done, total } => {
            ui.label(bold(format!("Загружаю версию {}…", release.version)));
            let frac = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
            theme::progress(ui, pal, frac, 220.0);
            let text = if *total > 0 {
                format!("{} из {}", format_size(*done), format_size(*total))
            } else {
                format_size(*done)
            };
            ui.label(theme::caption(pal, text));
        }
        Phase::Ready { release, .. } => {
            ui.label(bold(format!("Версия {} загружена и проверена", release.version)));
            if theme::primary_button(ui, pal, "Перезапустить и обновить").clicked() {
                actions.push(Action::InstallUpdate);
            }
            if theme::button(ui, pal, "Позже").clicked() {
                actions.push(Action::DismissUpdate);
            }
        }
        Phase::Failed { message, release } => {
            ui.label(RichText::new("Не удалось обновить:").size(FONT_BODY).color(pal.danger));
            ui.label(RichText::new(shorten(message, 90)).size(FONT_BODY).color(pal.text))
                .on_hover_text(message.as_str());
            if let Some(release) = release {
                if theme::button(ui, pal, "Открыть страницу загрузки").clicked() {
                    actions.push(Action::OpenUrl(release.url.clone()));
                }
                if theme::button(ui, pal, "Повторить").clicked() {
                    actions.push(Action::DownloadUpdate);
                }
            }
            if theme::button(ui, pal, "Закрыть").clicked() {
                actions.push(Action::DismissUpdate);
            }
        }
        Phase::Idle | Phase::Checking | Phase::UpToDate => {}
    });
}

// ------------------------------------------------------------------ шапка

/// Лупа, нарисованная кистью: в системных шрифтах нужного значка может не оказаться.
fn magnifier(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let painter = ui.painter();
    let stroke = Stroke::new(2.0, color);
    let center = rect.center() - vec2(2.0, 2.0);
    painter.circle_stroke(center, 6.5, stroke);
    painter.line_segment([center + vec2(4.7, 4.7), center + vec2(9.5, 9.5)], stroke);
}

fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::All => "Везде",
        Mode::Name => "Только имена",
        Mode::Content => "Только содержимое",
    }
}

fn sort_label(s: Sort) -> &'static str {
    match s {
        Sort::Relevance => "По соответствию",
        Sort::Newest => "Сначала новые",
        Sort::Oldest => "Сначала старые",
        Sort::Largest => "Сначала большие",
        Sort::Smallest => "Сначала маленькие",
    }
}

/// Тонкая вертикальная черта между группами фильтров.
fn vertical_line(ui: &mut Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(LINE_W, CONTROL_H - SPACE_SM), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(LINE_W, pal.line));
}

fn header(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
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
                    let output = TextEdit::singleline(&mut app.form.query)
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
                        let end = app.form.query.chars().count();
                        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(end))));
                        state.store(ui.ctx(), output.response.id);
                        app.focus_search = false;
                    }
                    changed |= output.response.changed();
                    if app.searching {
                        ui.add_sized([CONTROL_H, CONTROL_H], egui::Spinner::new().size(18.0));
                    } else if !app.form.query.is_empty()
                        && theme::close_button(ui, pal).on_hover_text("Очистить (Esc)").clicked()
                    {
                        app.form.query.clear();
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
    if app.settings.roots.is_empty() && app.num_docs == 0 {
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
fn filter_pills(ui: &mut Ui, app: &mut App, pal: &Palette, changed: &mut bool) {
    for (i, (name, _)) in GROUPS.iter().enumerate() {
        let on = app.form.groups[i];
        let count = app.facets.as_ref().and_then(|f| f.get(i)).filter(|_| !app.form.is_empty());
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
            app.form.groups[i] = !on;
            *changed = true;
        }
    }
}

/// Режим поиска, порядок и папка.
fn filter_controls(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>, changed: &mut bool) {
    ComboBox::from_id_salt("mode").width(150.0).selected_text(mode_label(app.form.mode)).show_ui(ui, |ui| {
        for m in [Mode::All, Mode::Name, Mode::Content] {
            *changed |= ui.selectable_value(&mut app.form.mode, m, mode_label(m)).changed();
        }
    });
    ComboBox::from_id_salt("sort").width(170.0).selected_text(sort_label(app.form.sort)).show_ui(ui, |ui| {
        for s in [Sort::Relevance, Sort::Newest, Sort::Oldest, Sort::Largest, Sort::Smallest] {
            *changed |= ui.selectable_value(&mut app.form.sort, s, sort_label(s)).changed();
        }
    });
    match &app.form.dir {
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

/// Число с разделителями тысяч: 23 501.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{a0}');
        }
        out.push(c);
    }
    out
}

/// Путь, укороченный до ширины `width` отбрасыванием начальных папок: конец пути (ближайшие папки) важнее начала.
fn fit_path(ui: &Ui, path: &str, width: f32, font: FontId, color: Color32) -> String {
    let fits = |text: &str| ui.painter().layout_no_wrap(text.to_string(), font.clone(), color).size().x <= width;
    if fits(path) {
        return path.to_string();
    }
    let sep = if path.contains('\\') { '\\' } else { '/' };
    let parts: Vec<&str> = path.split(sep).collect();
    for skip in 1..parts.len() {
        let candidate = format!("…{sep}{}", parts[skip..].join(&sep.to_string()));
        if fits(&candidate) {
            return candidate;
        }
    }
    shorten(path, 24)
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

fn status_dot(ui: &mut Ui, color: Color32) {
    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(dot.center(), 4.0, color);
}

fn status_bar(ui: &mut Ui, app: &App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        let (left, left_color) = if app.search_error.is_some() && !app.form.is_empty() {
            ("Ошибка поиска".to_string(), pal.danger)
        } else if app.num_docs > 0 {
            (format!("Файлов в индексе: {}", thousands(app.num_docs)), pal.muted)
        } else {
            (String::new(), pal.muted)
        };
        ui.label(RichText::new(left).size(FONT_CAPTION).color(left_color));

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let Some(backend) = &app.backend else { return };
            let st = &backend.state;
            let caption = |text: String, color: Color32| RichText::new(text).size(FONT_CAPTION).color(color);
            if let Some(error) = st.error.lock().unwrap().as_ref() {
                ui.label(caption(format!("Внимание: {}", shorten(error, 50)), pal.warn)).on_hover_text(error.as_str());
            }
            let response = if st.running.load(Ordering::Relaxed) {
                let total = st.total.load(Ordering::Relaxed);
                let done = st.done.load(Ordering::Relaxed);
                let phase = st.phase();
                ui.horizontal(|ui| {
                    if total > 0 && phase == "Индексация" {
                        let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
                        let eta = match app.index_eta_secs {
                            Some(secs) if secs >= 90 => format!(" · осталось ~{} мин", secs.div_ceil(60)),
                            Some(_) => " · почти готово".to_string(),
                            None => String::new(),
                        };
                        ui.label(caption(
                            format!("Индексация {}\u{a0}%{eta}", (frac * 100.0).floor() as u32),
                            pal.accent,
                        ))
                        .on_hover_text(format!(
                            "Прочитано файлов: {} из {}",
                            thousands(done),
                            thousands(total)
                        ));
                        theme::progress(ui, pal, frac, 120.0);
                    } else {
                        let scanned = st.scanned.load(Ordering::Relaxed);
                        let text = match (phase.is_empty(), scanned) {
                            (true, _) => "Индексация…".to_string(),
                            (false, 0) => phase,
                            (false, n) => format!("{phase} · найдено файлов: {}", thousands(n)),
                        };
                        ui.label(caption(text, pal.accent));
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                })
                .response
            } else if st.watching.load(Ordering::Relaxed) {
                ui.horizontal(|ui| {
                    ui.label(caption("Слежение включено".to_string(), pal.ok))
                        .on_hover_text("Изменения на диске попадают в поиск сами");
                    status_dot(ui, pal.ok);
                })
                .response
            } else if app.settings.roots.is_empty() {
                ui.label(caption("Папки не выбраны".to_string(), pal.warn))
            } else {
                ui.label(caption("Слежение выключено".to_string(), pal.muted))
            };
            if response.interact(Sense::click()).on_hover_text("Открыть настройки индексации").clicked()
            {
                actions.push(Action::OpenSettings);
            }
        });
    });
}

// ------------------------------------------------------------------ центральная область

/// Колонка по центру окна для пустых состояний.
fn centered_column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let avail = ui.available_rect_before_wrap();
    let width = avail.width().min(COLUMN_WIDTH);
    let rect = Rect::from_min_size(pos2(avail.center().x - width / 2.0, avail.top()), vec2(width, avail.height()));
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center)), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// Единое пустое состояние: значок, заголовок, пояснение и (по желанию) кнопки или подсказки.
fn empty_state(ui: &mut Ui, pal: &Palette, icon: Icon, title: &str, body: &str, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(SPACE_XL);
        centered_column(ui, |ui| {
            theme::icon_badge(ui, pal, icon);
            ui.add_space(SPACE_XS);
            ui.label(theme::title(pal, title));
            if !body.is_empty() {
                ui.add(egui::Label::new(theme::small(pal, body)).wrap().halign(Align::Center));
            }
            ui.add_space(SPACE_XS);
            add(ui);
            ui.add_space(SPACE_XL);
        });
    });
}

fn central(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
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

fn no_results(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
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

fn results_list(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
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

fn snippet_job(snippet: &Snippet, pal: &Palette, width: f32) -> LayoutJob {
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

/// Папка, в которой лежит файл (без имени самого файла).
fn parent_of(path: &str) -> String {
    std::path::Path::new(path).parent().map(|p| p.display().to_string()).unwrap_or_else(|| path.to_string())
}

/// Пояснение к файлам, у которых текста нет: пароль или скан.
fn status_note(status: &str) -> Option<&'static str> {
    if status == "encrypted" {
        Some("Файл защищён паролем — найден только по имени")
    } else if status == "scan" {
        Some("Скан без текстового слоя — найден только по имени (включите распознавание в настройках)")
    } else if status == "ocr" {
        Some("Скан: текст распознан автоматически, возможны ошибки")
    } else if status == "error" {
        Some("Не удалось прочитать текст — найден только по имени")
    } else {
        None
    }
}

/// Одна карточка результата: значок, имя, путь, фрагмент. Возвращает её прямоугольник.
fn hit_row(
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

/// Панель справа (или второй экран): подробности о выбранном файле и все действия над ним.
fn details_panel(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let Some(index) = app.selected else { return };
    let Some(hit) = app.results.get(index).cloned() else { return };
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
fn text_details(ui: &mut Ui, app: &mut App, hit: &Hit, index: usize, pal: &Palette, actions: &mut Vec<Action>) {
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

fn index_error(ui: &mut Ui, confirm: bool, error: &str, pal: &Palette, actions: &mut Vec<Action>) {
    empty_state(ui, pal, Icon::Warning, "Не удалось открыть индекс", error, |ui| {
        ui.add(
            egui::Label::new(theme::small(
                pal,
                "Если индекс создан другой версией программы, его нужно пересоздать — файлы будут проиндексированы заново.",
            ))
            .wrap()
            .halign(Align::Center),
        );
        ui.add_space(SPACE_SM);
        if confirm {
            // Подтверждение — как и везде: красная кнопка и «Отмена».
            match theme::confirm_row(ui, pal, "Удалить индекс и построить заново?", "Да, пересоздать")
            {
                Some(true) => actions.push(Action::RecreateIndex),
                Some(false) => actions.push(Action::CancelRecreate),
                None => {}
            }
        } else if theme::primary_button(ui, pal, "Пересоздать индекс…").clicked() {
            actions.push(Action::AskRecreate);
        }
    });
}

fn onboarding(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    empty_state(
        ui,
        pal,
        Icon::Folder,
        "Добро пожаловать!",
        "Поиск находит документы не только по названию, но и по тексту внутри: Word, Excel, PowerPoint, PDF и обычные текстовые файлы.",
        |ui| {
            theme::card_frame(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.label(theme::heading(pal, "Где искать?"));
                    ui.add(
                        egui::Label::new(theme::small(
                            pal,
                            "Программа один раз прочитает файлы, а потом будет следить за изменениями сама. Папку можно и перетащить в это окно.",
                        ))
                        .wrap(),
                    );
                    ui.add_space(SPACE_XS);
                    ui.horizontal_wrapped(|ui| {
                        for (name, path) in &app.places {
                            if theme::button(ui, pal, name).on_hover_text(path.display().to_string()).clicked() {
                                actions.push(Action::AddRoot(path.clone()));
                            }
                        }
                        if theme::primary_button(ui, pal, "Выбрать папку…").clicked() {
                            actions.push(Action::PickRoot);
                        }
                    });
                    ui.add_space(SPACE_XS);
                    egui::CollapsingHeader::new(theme::body(pal, "Папка на другом компьютере")).show(ui, |ui| {
                        network_input(ui, app, pal, actions);
                    });
                });
            });
        },
    );
}

/// Поле для адреса сетевой папки: `\\192.168.1.10\документы`.
pub fn network_input(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let mut submit = false;
    ui.horizontal(|ui| {
        let response = TextEdit::singleline(&mut app.net_input)
            .hint_text("\\\\192.168.1.10\\документы")
            .font(theme::font(FONT_BODY))
            .margin(Margin::symmetric(SPACE_SM as i8, 7))
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
        submit |= ui.add_enabled_ui(ready, |ui| theme::button(ui, pal, label)).inner.clicked();
    });
    if submit && !app.net_checking && !app.net_input.trim().is_empty() {
        actions.push(Action::AddRootText(app.net_input.clone()));
    }
    if let Some(error) = &app.net_error {
        ui.add(egui::Label::new(RichText::new(error).size(FONT_CAPTION).color(pal.danger)).wrap());
    } else {
        ui.add(
            egui::Label::new(theme::caption(
                pal,
                "Нужен доступ к папке из проводника: если спросит пароль, один раз введите его там.",
            ))
            .wrap(),
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
    ("content:смета", "только в тексте"),
];

/// Горячие клавиши для приветственного экрана: (клавиши, действие).
const SHORTCUTS: [(&str, &str); 10] = [
    ("Стрелки", "выбор файла"),
    ("Enter", "открыть"),
    ("Shift+Enter", "показать в папке"),
    ("Ctrl+C", "копировать путь"),
    ("Ctrl+F", "в строку поиска"),
    ("Ctrl+1…5", "типы файлов"),
    ("Ctrl+Home", "к первому файлу"),
    ("F3", "следующее совпадение"),
    ("Ctrl+End", "к последнему файлу"),
    ("Esc", "очистить запрос"),
];

fn welcome(ui: &mut Ui, app: &App, pal: &Palette, actions: &mut Vec<Action>) {
    let indexing = app.indexing();
    let status = if indexing && app.num_docs == 0 {
        "Идёт первичная индексация — она может занять несколько минут, искать можно уже сейчас.".to_string()
    } else if indexing {
        format!(
            "Индексация ещё идёт: в поиске пока {} файлов, остальные появятся по мере чтения. Искать можно уже сейчас.",
            thousands(app.num_docs)
        )
    } else if app.num_docs > 0 {
        format!("В индексе файлов: {}. Поиск учитывает словоформы и опечатки.", thousands(app.num_docs))
    } else {
        "Индекс пока пуст.".to_string()
    };
    empty_state(ui, pal, Icon::Search, "Что будем искать?", &status, |ui| {
        ui.add_space(SPACE_SM);
        let block = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
            theme::card_frame(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.with_layout(Layout::top_down(Align::Min), |ui| add(ui));
            });
            ui.add_space(SPACE_SM);
        };
        if !app.settings.history.is_empty() {
            block(ui, &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label(theme::heading(pal, "Недавние запросы"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::ghost_button(ui, pal, "Очистить").clicked() {
                            actions.push(Action::ClearHistory);
                        }
                    });
                });
                ui.horizontal_wrapped(|ui| {
                    for query in app.settings.history.iter().take(8) {
                        if theme::pill(ui, pal, &shorten(query, 36), false).on_hover_text(query.as_str()).clicked() {
                            actions.push(Action::UseQuery(query.clone()));
                        }
                    }
                });
                ui.label(theme::caption(pal, "Стрелка вверх в пустой строке поиска подставляет прошлый запрос."));
            });
        }
        block(ui, &mut |ui| {
            ui.label(theme::heading(pal, "Примеры запросов"));
            ui.label(theme::caption(pal, "Нажмите на пример, чтобы попробовать"));
            ui.add_space(SPACE_XS);
            let columns = if ui.available_width() >= 520.0 { 2 } else { 1 };
            egui::Grid::new("examples").num_columns(columns * 2).spacing([SPACE_LG, SPACE_SM]).show(ui, |ui| {
                for (n, (code, description)) in EXAMPLES.iter().enumerate() {
                    let button =
                        egui::Button::new(RichText::new(*code).monospace().size(theme::FONT_MONO).color(pal.accent))
                            .fill(pal.surface_alt)
                            .corner_radius(theme::radius(RADIUS_SM))
                            .min_size(vec2(0.0, CONTROL_H));
                    if ui.add(button).clicked() {
                        actions.push(Action::UseQuery(code.to_string()));
                    }
                    ui.label(theme::caption(pal, *description));
                    if (n + 1) % columns == 0 {
                        ui.end_row();
                    }
                }
            });
        });
        block(ui, &mut |ui| {
            ui.label(theme::heading(pal, "Горячие клавиши"));
            ui.add_space(SPACE_XS);
            let columns = if ui.available_width() >= 520.0 { 2 } else { 1 };
            egui::Grid::new("shortcuts").num_columns(columns * 2).spacing([SPACE_LG, SPACE_SM]).show(ui, |ui| {
                for (n, (keys, what)) in SHORTCUTS.iter().enumerate() {
                    theme::keycap(ui, pal, keys);
                    ui.label(theme::caption(pal, *what));
                    if (n + 1) % columns == 0 {
                        ui.end_row();
                    }
                }
            });
        });
    });
}

// ------------------------------------------------------------------ всплывающее сообщение

/// Сколько показывать сообщение: чем длиннее текст, тем дольше; ошибки висят дольше.
fn toast_lifetime(text: &str, kind: Kind) -> Duration {
    let secs = (2.5 + text.chars().count() as f32 * 0.05).clamp(3.0, 8.0);
    Duration::from_secs_f32(if kind == Kind::Error { secs * 1.5 } else { secs })
}

pub fn toast(ctx: &Context, toast: &mut Option<Toast>) {
    let Some(current) = toast.as_ref() else { return };
    let life = toast_lifetime(&current.text, current.kind);
    let age = current.since.elapsed();
    if age >= life {
        *toast = None;
        return;
    }
    let pal = theme::palette(ctx.style().visuals.dark_mode);
    let (kind, text) = (current.kind, current.text.clone());
    let mut dismissed = false;
    egui::Area::new(Id::new("toast")).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -48.0)).order(Order::Foreground).show(
        ctx,
        |ui| {
            let frame = Frame::new()
                .fill(kind.soft(&pal))
                .stroke(Stroke::new(LINE_W, kind.color(&pal)))
                .corner_radius(theme::radius(RADIUS_MD))
                .shadow(ctx.style().visuals.popup_shadow)
                .inner_margin(Margin::symmetric(SPACE_MD as i8, SPACE_SM as i8))
                .show(ui, |ui| {
                    ui.set_max_width(480.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = SPACE_MD;
                        theme::kind_icon(ui, &pal, kind);
                        ui.add(egui::Label::new(theme::body(&pal, &text)).wrap());
                    });
                });
            if frame.response.interact(Sense::click()).on_hover_text("Нажмите, чтобы скрыть").clicked()
            {
                dismissed = true;
            }
        },
    );
    if dismissed {
        *toast = None;
    } else {
        ctx.request_repaint_after(life - age);
    }
}
