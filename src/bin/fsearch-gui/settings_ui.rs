//! Окно «Настройки»: слева разделы, справа содержимое выбранного раздела, внизу — «Применить» (если есть что применять).

use std::sync::atomic::Ordering;

use egui::{Align, Align2, Context, Frame, Layout, Margin, RichText, TextEdit, Ui, vec2};
use file_search::platform::format_size;

use crate::app::{Action, App};
use crate::settings::{Load, Theme};
use crate::theme::{self, FONT_BODY, FONT_CAPTION, Kind, Palette, SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XL, SPACE_XS};
use crate::updater::Phase;
use crate::views::{shorten, thousands};

/// Раздел окна настроек.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Tab {
    #[default]
    Folders,
    Search,
    Index,
    Updates,
    About,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Folders, Tab::Search, Tab::Index, Tab::Updates, Tab::About];

    fn title(self) -> &'static str {
        match self {
            Tab::Folders => "Папки",
            Tab::Search => "Поиск",
            Tab::Index => "Индексация",
            Tab::Updates => "Обновления",
            Tab::About => "О программе",
        }
    }

    /// Для снимков экрана при проверке: раздел по имени.
    pub fn from_name(name: &str) -> Option<Tab> {
        match name {
            "folders" => Some(Tab::Folders),
            "search" => Some(Tab::Search),
            "index" => Some(Tab::Index),
            "updates" => Some(Tab::Updates),
            "about" => Some(Tab::About),
            _ => None,
        }
    }
}

pub fn show(ctx: &Context, app: &mut App, actions: &mut Vec<Action>) {
    if !app.show_settings {
        return;
    }
    let pal = theme::palette(ctx.style().visuals.dark_mode);
    let screen = ctx.content_rect().size();
    let size = vec2((screen.x - 2.0 * SPACE_LG).min(800.0), (screen.y - 130.0).clamp(300.0, 600.0));
    let mut open = true;
    egui::Window::new("Настройки")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(size)
        .max_size(vec2(screen.x - SPACE_LG, screen.y - SPACE_LG))
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_size(size);
            if app.pending_changes() {
                egui::TopBottomPanel::bottom("settings_apply")
                    .frame(Frame::new().inner_margin(Margin { left: 0, right: 0, top: SPACE_SM as i8, bottom: 0 }))
                    .show_inside(ui, |ui| apply_bar(ui, app, &pal, actions));
            }
            egui::SidePanel::left("settings_nav")
                .exact_width(160.0)
                .resizable(false)
                .frame(Frame::new().inner_margin(Margin { left: 0, right: SPACE_SM as i8, top: 0, bottom: 0 }))
                .show_inside(ui, |ui| nav(ui, app, &pal));
            egui::CentralPanel::default()
                .frame(Frame::new().inner_margin(Margin { left: SPACE_MD as i8, right: 0, top: 0, bottom: 0 }))
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(app.settings_tab as u8).show(
                        ui,
                        |ui| {
                            ui.add_space(SPACE_XS);
                            match app.settings_tab {
                                Tab::Folders => folders_tab(ui, app, &pal, actions),
                                Tab::Search => search_tab(ui, app, &pal),
                                Tab::Index => index_tab(ui, app, &pal, actions),
                                Tab::Updates => updates_tab(ui, app, &pal, actions),
                                Tab::About => about_tab(ui, &pal, actions),
                            }
                            ui.add_space(SPACE_LG);
                        },
                    );
                });
        });
    if !open {
        actions.push(Action::CloseSettings);
    }
}

fn nav(ui: &mut Ui, app: &mut App, pal: &Palette) {
    ui.spacing_mut().item_spacing.y = SPACE_XS;
    for tab in Tab::ALL {
        let selected = app.settings_tab == tab;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), theme::CONTROL_H), egui::Sense::click());
        let (bg, fg) = if selected {
            (pal.selected, pal.accent)
        } else if response.hovered() {
            (pal.surface_alt, pal.text)
        } else {
            (egui::Color32::TRANSPARENT, pal.text_2)
        };
        ui.painter().rect_filled(rect, theme::radius(theme::RADIUS_SM), bg);
        let galley = ui.painter().layout_no_wrap(tab.title().to_string(), theme::font(FONT_BODY), fg);
        ui.painter().galley(egui::pos2(rect.left() + SPACE_MD, rect.center().y - galley.size().y / 2.0), galley, fg);
        if response.clicked() {
            app.settings_tab = tab;
        }
    }
}

fn apply_bar(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if theme::primary_button(ui, pal, "Применить").on_hover_text("Сохранить и обновить индекс").clicked()
        {
            actions.push(Action::ApplySettings { force: false });
        }
        if theme::button(ui, pal, "Отменить").clicked() {
            app.revert_pending();
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new("Есть изменения: они вступят в силу после обновления индекса")
                        .size(FONT_BODY)
                        .color(pal.warn),
                )
                .truncate(),
            );
        });
    });
}

// ------------------------------------------------------------------ общие заготовки

fn tab_title(ui: &mut Ui, pal: &Palette, title: &str, hint: &str) {
    ui.label(theme::title(pal, title));
    if !hint.is_empty() {
        ui.add(egui::Label::new(theme::small(pal, hint)).wrap());
    }
    ui.add_space(SPACE_SM);
}

fn section(ui: &mut Ui, pal: &Palette, title: &str) {
    ui.add_space(SPACE_MD);
    ui.label(theme::heading(pal, title));
    ui.add_space(SPACE_XS);
}

fn card(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    theme::card_frame(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

/// Линия между строками внутри карточки.
fn row_line(ui: &mut Ui, pal: &Palette) {
    ui.add_space(SPACE_XS);
    theme::separator(ui, pal);
    ui.add_space(SPACE_XS);
}

/// Выбор одного из нескольких вариантов в одну строку.
fn segmented<T: PartialEq + Copy>(ui: &mut Ui, pal: &Palette, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (option, label) in options {
            if theme::pill(ui, pal, label, *value == *option).clicked() && *value != *option {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

// ------------------------------------------------------------------ папки

fn folders_tab(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    tab_title(
        ui,
        pal,
        "Папки для поиска",
        "Программа ищет только в этих папках и сама следит за их изменениями. Папку можно перетащить прямо в окно.",
    );
    if app.settings.roots.is_empty() {
        ui.label(RichText::new("Папки не выбраны — добавьте хотя бы одну.").size(FONT_BODY).color(pal.warn));
        ui.add_space(SPACE_SM);
    }
    let roots = app.settings.roots.clone();
    if !roots.is_empty() {
        card(ui, pal, |ui| {
            for (i, root) in roots.iter().enumerate() {
                if i > 0 {
                    row_line(ui, pal);
                }
                let full = root.display().to_string();
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| full.clone());
                if app.confirm_remove == Some(i) {
                    ui.label(theme::body(pal, name));
                    match theme::confirm_row(ui, pal, "Убрать из поиска и из индекса?", "Да, убрать")
                    {
                        Some(true) => actions.push(Action::RemoveRoot(i)),
                        Some(false) => actions.push(Action::CancelRemoveRoot),
                        None => {}
                    }
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::button(ui, pal, "Убрать")
                            .on_hover_text("Убрать из поиска и удалить из индекса")
                            .clicked()
                        {
                            actions.push(Action::ConfirmRemoveRoot(i));
                        }
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.horizontal(|ui| {
                                ui.label(theme::body(pal, name));
                                if file_search::netpath::is_network_path(root) {
                                    theme::chip(ui, "сеть", pal.accent, pal.selected);
                                }
                            });
                            ui.add(egui::Label::new(theme::caption(pal, shorten(&full, 80))).truncate())
                                .on_hover_text(full.as_str());
                        });
                    })
                });
            }
        });
        ui.add_space(SPACE_SM);
    }
    ui.horizontal_wrapped(|ui| {
        if theme::primary_button(ui, pal, "Добавить папку…").clicked() {
            actions.push(Action::PickRoot);
        }
        for (name, path) in &app.places {
            if !app.settings.roots.iter().any(|r| file_search::schema::path_covers(r, path))
                && theme::button(ui, pal, name).on_hover_text(path.display().to_string()).clicked()
            {
                actions.push(Action::AddRoot(path.clone()));
            }
        }
    });
    section(ui, pal, "Папка на другом компьютере");
    card(ui, pal, |ui| crate::views::network_input(ui, app, pal, actions));
}

// ------------------------------------------------------------------ поиск

/// Распознавание текста на сканах: переключатель, пояснение и состояние движка.
/// Состояние читается из `engine_info()` только здесь.
fn ocr_setting(ui: &mut Ui, app: &mut App, pal: &Palette) {
    let info = file_search::extract::ocr::engine_info();
    theme::setting_row(
        ui,
        pal,
        "Распознавать текст на сканах",
        "Находит слова в PDF, где вместо текста — картинки (сканы, фотографии страниц). Это медленно: страница занимает около секунды, поэтому первая индексация сканов дольше. Без распознавания такие файлы находятся только по имени.",
        |ui| {
            if info.available {
                theme::toggle(ui, pal, &mut app.settings.ocr);
            } else {
                // Движка нет: показываем выключенным и недоступным, сохранённое значение не трогаем.
                ui.add_enabled_ui(false, |ui| theme::toggle(ui, pal, &mut false));
            }
        },
    );
    ui.add_space(SPACE_SM);
    let (kind, text) = if info.available && info.russian {
        (Kind::Success, format!("Распознавание готово к работе · язык: {}", info.language))
    } else if info.available {
        (Kind::Error, info.note.clone())
    } else {
        (Kind::Error, format!("Распознавание недоступно. {}", info.note))
    };
    let (fg, bg) = match kind {
        Kind::Success => (pal.ok, pal.ok_soft),
        _ => (pal.warn, pal.warn_soft),
    };
    Frame::new()
        .fill(bg)
        .corner_radius(theme::radius(theme::RADIUS_SM))
        .inner_margin(Margin::symmetric(SPACE_MD as i8, SPACE_SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).size(theme::FONT_SMALL).color(fg)).wrap());
        });
}

fn search_tab(ui: &mut Ui, app: &mut App, pal: &Palette) {
    tab_title(ui, pal, "Поиск", "");
    section(ui, pal, "Сканы и фотографии документов");
    card(ui, pal, |ui| ocr_setting(ui, app, pal));

    section(ui, pal, "Нагрузка на компьютер");
    card(ui, pal, |ui| {
        let options = [(Load::Minimal, "Минимальная"), (Load::Moderate, "Умеренная"), (Load::Maximum, "Максимальная")];
        segmented(ui, pal, &mut app.settings.load, &options);
        let hint = match app.settings.load {
            Load::Minimal => "Индексация идёт медленно, но компьютер почти не замечает её работы.",
            Load::Moderate => "Обычно хватает: индексация работает в фоне и не мешает остальным программам.",
            Load::Maximum => "Самая быстрая индексация, но компьютер может притормаживать, пока она идёт.",
        };
        ui.add(egui::Label::new(theme::small(pal, hint)).wrap());
    });

    section(ui, pal, "Слежение за папками");
    card(ui, pal, |ui| {
        theme::setting_row(
            ui,
            pal,
            "Обновлять поиск автоматически",
            "Новые и изменённые файлы попадают в поиск сразу, без повторной индексации.",
            |ui| {
                theme::toggle(ui, pal, &mut app.settings.watch);
            },
        );
    });

    section(ui, pal, "Внешний вид");
    card(ui, pal, |ui| {
        let options = [(Theme::Auto, "Как в системе"), (Theme::Light, "Светлая"), (Theme::Dark, "Тёмная")];
        segmented(ui, pal, &mut app.settings.theme, &options);
    });
}

// ------------------------------------------------------------------ индексация

fn index_tab(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    tab_title(
        ui,
        pal,
        "Индексация",
        "Программа один раз читает файлы и запоминает их содержимое: так поиск работает мгновенно.",
    );
    card(ui, pal, |ui| status_card(ui, app, pal, actions));

    section(ui, pal, "Что не индексировать");
    card(ui, pal, |ui| {
        ui.add(
            egui::Label::new(theme::small(
                pal,
                "Шаблоны исключений, по одному в строке. Например: *.tmp или **/Резерв/**",
            ))
            .wrap(),
        );
        ui.add(
            TextEdit::multiline(&mut app.excludes_text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .font(theme::font(FONT_BODY))
                .hint_text("*.tmp"),
        );
        row_line(ui, pal);
        theme::setting_row(
            ui,
            pal,
            "Пропускать служебные папки",
            "Корзина ($RECYCLE.BIN), .git, node_modules и подобные: в них нет рабочих документов.",
            |ui| {
                theme::toggle(ui, pal, &mut app.settings.default_excludes);
            },
        );
    });

    section(ui, pal, "Параметры");
    card(ui, pal, |ui| {
        theme::setting_row(
            ui,
            pal,
            "Полная проверка папок раз в",
            "Страховка на случай пропущенных уведомлений системы. Для папок на другом компьютере проверка идёт каждые 10 минут независимо от этого значения.",
            |ui| {
                ui.add(egui::DragValue::new(&mut app.settings.rescan_minutes).range(5..=10080).suffix(" мин"));
            },
        );
        row_line(ui, pal);
        theme::setting_row(
            ui,
            pal,
            "Максимум текста из одного файла",
            "Очень большие файлы читаются не целиком.",
            |ui| {
                ui.add(egui::DragValue::new(&mut app.settings.max_text_mb).range(1..=512).suffix(" МБ"));
            },
        );
    });

    section(ui, pal, "Индекс");
    card(ui, pal, |ui| {
        ui.add(egui::Label::new(theme::caption(pal, app.index_dir.display().to_string())).wrap());
        ui.add_space(SPACE_XS);
        if app.confirm_reset {
            match theme::confirm_row(ui, pal, "Удалить индекс и построить заново?", "Да, пересоздать")
            {
                Some(true) => actions.push(Action::RecreateIndex),
                Some(false) => app.confirm_reset = false,
                None => {}
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                if theme::button(ui, pal, "Открыть папку индекса").clicked() {
                    actions.push(Action::OpenIndexFolder);
                }
                if theme::button(ui, pal, "Пересоздать индекс…").clicked() {
                    app.confirm_reset = true;
                }
            });
        }
    });
}

fn status_card(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    let Some(backend) = &app.index.backend else {
        ui.label(theme::body(pal, "Индекс недоступен."));
        return;
    };
    let st = backend.state.clone();
    ui.horizontal(|ui| {
        ui.label(theme::title(pal, thousands(app.index.num_docs)));
        ui.label(theme::small(pal, "файлов в индексе"));
    });
    if st.running.load(Ordering::Relaxed) {
        let total = st.total.load(Ordering::Relaxed);
        let done = st.done.load(Ordering::Relaxed);
        if total > 0 {
            theme::progress(ui, pal, done as f32 / total as f32, ui.available_width());
            let eta = match app.index.index_eta_secs {
                Some(secs) if secs >= 90 => format!(" · осталось ~{} мин", secs.div_ceil(60)),
                Some(_) => " · почти готово".to_string(),
                None => String::new(),
            };
            ui.label(theme::small(pal, format!("Прочитано {} из {}{eta}", thousands(done), thousands(total))));
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(theme::small(
                    pal,
                    format!("Обход папок: найдено файлов — {}", thousands(st.scanned.load(Ordering::Relaxed))),
                ));
            });
        }
        let current = st.current.lock().unwrap().clone();
        if !current.is_empty() {
            ui.label(theme::caption(pal, shorten(&current, 80)));
        }
        if theme::button(ui, pal, "Остановить индексацию и слежение").clicked() {
            actions.push(Action::StopIndexing);
        }
    } else {
        ui.horizontal_wrapped(|ui| {
            let has_roots = !app.settings.roots.is_empty();
            if ui.add_enabled_ui(has_roots, |ui| theme::primary_button(ui, pal, "Обновить сейчас")).inner.clicked()
            {
                actions.push(Action::ApplySettings { force: false });
            }
            if ui
                .add_enabled_ui(has_roots, |ui| theme::button(ui, pal, "Прочитать все файлы заново"))
                .inner
                .on_hover_text("Заново прочитать все файлы, даже не изменившиеся")
                .clicked()
            {
                actions.push(Action::ApplySettings { force: true });
            }
        });
    }
    if let Some(error) = st.error.lock().unwrap().as_ref() {
        ui.add(egui::Label::new(RichText::new(error).size(theme::FONT_SMALL).color(pal.warn)).wrap());
    }
    let summary = st.summary.lock().unwrap().clone();
    if let Some(s) = summary {
        row_line(ui, pal);
        let head = if s.cancelled { "Прервано" } else { "Последний проход" };
        ui.label(theme::body(pal, format!("{head} · {:.1} с", s.elapsed.as_secs_f64())));
        egui::Grid::new("index_summary").num_columns(2).min_row_height(20.0).spacing([SPACE_LG, SPACE_XS]).show(
            ui,
            |ui| {
                for (label, value) in [
                    ("Файлов найдено", thousands(s.scanned)),
                    ("Добавлено или обновлено", thousands(s.indexed)),
                    ("Без изменений", thousands(s.unchanged)),
                    ("Удалено", thousands(s.removed)),
                    ("Объём текста", format_size(s.text_bytes)),
                ] {
                    ui.label(theme::caption(pal, label));
                    ui.label(theme::small(pal, value));
                    ui.end_row();
                }
            },
        );
        let notes = [
            (s.encrypted, "Защищены паролем (ищутся только по имени)"),
            (s.recognized, "Текст распознан на сканах (возможны ошибки)"),
            (s.scans, "Сканы без распознавания (ищутся только по имени)"),
            (s.deferred, "Не читались в прошлый раз, повторная попытка позже"),
            (s.truncated, "Прочитаны не полностью (лимит текста)"),
        ];
        for (n, text) in notes {
            if n > 0 {
                ui.label(theme::caption(pal, format!("{text}: {n}")));
            }
        }
        if s.failed_total > 0 {
            egui::CollapsingHeader::new(
                RichText::new(format!("Не удалось прочитать файлов: {}", s.failed_total))
                    .size(FONT_BODY)
                    .color(pal.warn),
            )
            .id_salt("failed")
            .show(ui, |ui| {
                ui.label(theme::caption(pal, "По имени эти файлы всё равно находятся."));
                for (path, why) in s.failed.iter().take(100) {
                    ui.label(theme::caption(pal, format!("{}\n    {}", path.display(), why)));
                }
            });
        }
        if s.walk_errors_total > 0 {
            egui::CollapsingHeader::new(theme::body(pal, format!("Нет доступа к {} объектам", s.walk_errors_total)))
                .id_salt("walk_errors")
                .show(ui, |ui| {
                    for e in s.walk_errors.iter().take(50) {
                        ui.label(theme::caption(pal, e.as_str()));
                    }
                });
        }
    }
    let log: Vec<String> = st.log.lock().unwrap().iter().rev().take(30).cloned().collect();
    if !log.is_empty() {
        egui::CollapsingHeader::new(theme::body(pal, "Журнал")).id_salt("log").show(ui, |ui| {
            for line in log {
                ui.label(theme::caption(pal, line));
            }
        });
    }
}

// ------------------------------------------------------------------ обновления

fn updates_tab(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    tab_title(ui, pal, "Обновления", "");
    card(ui, pal, |ui| {
        ui.horizontal(|ui| {
            ui.label(theme::body(pal, "Установлена версия"));
            ui.label(RichText::new(env!("CARGO_PKG_VERSION")).size(FONT_BODY).color(pal.accent));
        });
        row_line(ui, pal);
        theme::setting_row(
            ui,
            pal,
            "Проверять обновления автоматически",
            "Раз в сутки. Программа только спрашивает у GitHub номер последней версии; ничего не ставится без вашей кнопки.",
            |ui| {
                if theme::toggle(ui, pal, &mut app.settings.check_updates).changed() {
                    actions.push(Action::SaveSettings);
                }
            },
        );
    });
    section(ui, pal, "Состояние");
    card(ui, pal, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled_ui(!app.updater.busy(), |ui| theme::button(ui, pal, "Проверить сейчас")).inner.clicked()
            {
                actions.push(Action::CheckUpdates);
            }
            match &app.updater.phase {
                Phase::Idle => {
                    if app.settings.last_update_check > 0 {
                        ui.label(theme::small(
                            pal,
                            "Последняя проверка: ".to_string() + &ago(app.settings.last_update_check),
                        ));
                    }
                }
                Phase::Checking => {
                    ui.spinner();
                    ui.label(theme::small(pal, "Проверяю…"));
                }
                Phase::UpToDate => {
                    ui.label(RichText::new("У вас последняя версия").size(FONT_BODY).color(pal.ok));
                }
                Phase::Available(release) => {
                    ui.label(
                        RichText::new(format!("Доступна версия {} от {}", release.version, release.published))
                            .size(FONT_BODY)
                            .color(pal.accent),
                    );
                    let label =
                        if release.installable_asset().is_some() { "Обновить" } else { "Скачать" };
                    if theme::primary_button(ui, pal, label).clicked() {
                        actions.push(Action::DownloadUpdate);
                    }
                    if theme::button(ui, pal, "Что нового").clicked() {
                        actions.push(Action::OpenUrl(release.url.clone()));
                    }
                }
                Phase::Downloading { done, total, .. } => {
                    let frac = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
                    theme::progress(ui, pal, frac, 200.0);
                    ui.label(theme::small(pal, "Загружаю…"));
                }
                Phase::Ready { release, .. } => {
                    ui.label(
                        RichText::new(format!("Версия {} загружена", release.version)).size(FONT_BODY).color(pal.ok),
                    );
                    if theme::primary_button(ui, pal, "Перезапустить и обновить").clicked() {
                        actions.push(Action::InstallUpdate);
                    }
                }
                Phase::Failed { message, .. } => {
                    ui.label(
                        RichText::new(format!("Не удалось: {}", shorten(message, 80))).size(FONT_BODY).color(pal.warn),
                    )
                    .on_hover_text(message.as_str());
                }
            }
        });
        if let Some(version) = app.settings.skipped_version.clone() {
            row_line(ui, pal);
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::small(pal, format!("Версия {version} пропущена")));
                if theme::button(ui, pal, "Снова предлагать").clicked() {
                    app.settings.skipped_version = None;
                    actions.push(Action::CheckUpdates);
                }
            });
        }
    });
    ui.add_space(SPACE_SM);
    if theme::ghost_button(ui, pal, "Все выпуски на GitHub").clicked() {
        actions.push(Action::OpenUrl(file_search::update::RELEASES_URL.to_string()));
    }
}

/// «только что», «2 ч назад», «3 дн. назад» для отметки времени в секундах с 1970 года.
fn ago(unix_secs: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let d = now.saturating_sub(unix_secs);
    if d < 90 {
        "только что".into()
    } else if d < 3600 {
        format!("{} мин назад", d / 60)
    } else if d < 86_400 {
        format!("{} ч назад", d / 3600)
    } else {
        format!("{} дн. назад", d / 86_400)
    }
}

// ------------------------------------------------------------------ о программе

fn about_tab(ui: &mut Ui, pal: &Palette, actions: &mut Vec<Action>) {
    ui.label(theme::title(pal, "Поиск файлов"));
    ui.label(theme::small(pal, format!("Версия {}", env!("CARGO_PKG_VERSION"))));
    ui.add_space(SPACE_SM);
    ui.add(
        egui::Label::new(theme::body(
            pal,
            "Находит документы по названию и по тексту внутри: Word, Excel, PowerPoint, PDF, сканы и обычные текстовые файлы. Все данные остаются на этом компьютере.",
        ))
        .wrap(),
    );

    section(ui, pal, "Если что-то работает не так");
    card(ui, pal, |ui| {
        ui.add(
            egui::Label::new(theme::small(
                pal,
                "Опишите, что вы делали, и отправьте тому, кто вам помогает, файл gui-crash.log из этой папки. В нём нет ваших документов — только записи о сбоях.",
            ))
            .wrap(),
        );
        let dir = crate::config_dir();
        let log = dir.join("gui-crash.log");
        ui.add_space(SPACE_XS);
        ui.add(
            egui::Label::new(RichText::new(log.display().to_string()).monospace().size(FONT_CAPTION).color(pal.muted))
                .wrap(),
        );
        ui.add_space(SPACE_XS);
        ui.horizontal_wrapped(|ui| {
            if theme::button(ui, pal, "Открыть папку с журналом").clicked() {
                actions.push(Action::OpenLogFolder);
            }
            if theme::button(ui, pal, "Копировать путь").clicked() {
                actions.push(Action::CopyText(log.display().to_string(), "Путь к журналу скопирован"));
            }
        });
    });

    section(ui, pal, "Ссылки");
    card(ui, pal, |ui| {
        ui.horizontal_wrapped(|ui| {
            if theme::button(ui, pal, "Страница программы и выпуски").clicked() {
                actions.push(Action::OpenUrl(file_search::update::RELEASES_URL.to_string()));
            }
        });
        row_line(ui, pal);
        ui.label(theme::caption(pal, "Файл настроек"));
        ui.add(
            egui::Label::new(
                RichText::new(crate::settings::settings_path().display().to_string())
                    .monospace()
                    .size(FONT_CAPTION)
                    .color(pal.muted),
            )
            .wrap(),
        );
    });
    ui.add_space(SPACE_XL);
}
