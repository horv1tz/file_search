//! Окно «Индексация и настройки».

use std::sync::atomic::Ordering;

use egui::{Align2, Color32, ComboBox, Context, ProgressBar, RichText, TextEdit, Ui};
use file_search::platform::format_size;

use crate::app::{Action, App};
use crate::settings::Load;
use crate::settings::Theme;
use crate::theme::{self, Palette};
use crate::updater::Phase;
use crate::views::shorten;

pub fn show(ctx: &Context, app: &mut App, actions: &mut Vec<Action>) {
    if !app.show_settings {
        return;
    }
    let pal = theme::palette(ctx.style().visuals.dark_mode);
    let mut open = true;
    egui::Window::new("Индексация и настройки")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([640.0, 600.0])
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                roots_section(ui, app, &pal, actions);
                ui.separator();
                status_section(ui, app, &pal, actions);
                ui.separator();
                advanced_section(ui, app, actions);
                ui.separator();
                index_section(ui, app, &pal, actions);
                ui.separator();
                updates_section(ui, app, &pal, actions);
            });
        });
    if !open {
        actions.push(Action::CloseSettings);
    }
}

fn roots_section(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Где искать").strong().size(16.0));
    if app.settings.roots.is_empty() {
        ui.label(RichText::new("Папки не выбраны — добавьте хотя бы одну.").color(pal.warn));
    }
    for (i, root) in app.settings.roots.iter().enumerate() {
        ui.horizontal(|ui| {
            if app.confirm_remove == Some(i) {
                ui.label(RichText::new("Убрать из поиска?").color(pal.warn));
                if ui.button("Убрать").clicked() {
                    actions.push(Action::RemoveRoot(i));
                }
                if ui.button("Отмена").clicked() {
                    actions.push(Action::CancelRemoveRoot);
                }
            } else if ui
                .add(egui::Button::new(RichText::new("×").size(15.0)).min_size(egui::vec2(26.0, 24.0)))
                .on_hover_text("Убрать из поиска и удалить из индекса")
                .clicked()
            {
                actions.push(Action::ConfirmRemoveRoot(i));
            }
            ui.label(root.display().to_string());
            if file_search::netpath::is_network_path(root) {
                ui.label(RichText::new("сеть").size(12.0).color(pal.accent))
                    .on_hover_text("Папка на другом компьютере: проверка изменений идёт каждые 10 минут");
            }
        });
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button("Добавить папку…").clicked() {
            actions.push(Action::PickRoot);
        }
        for (name, path) in &app.places {
            if !app.settings.roots.iter().any(|r| file_search::schema::path_covers(r, path))
                && ui.button(name).on_hover_text(path.display().to_string()).clicked()
            {
                actions.push(Action::AddRoot(path.clone()));
            }
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Нагрузка на компьютер:");
        let label = |l: Load| match l {
            Load::Minimal => "Минимальная — медленно, почти незаметно",
            Load::Moderate => "Умеренная — обычно хватает",
            Load::Maximum => "Максимальная — быстро, компьютер может притормаживать",
        };
        let before = app.settings.load;
        ComboBox::from_id_salt("load").selected_text(label(app.settings.load)).show_ui(ui, |ui| {
            for l in [Load::Minimal, Load::Moderate, Load::Maximum] {
                ui.selectable_value(&mut app.settings.load, l, label(l));
            }
        });
        if app.settings.load != before {
            actions.push(Action::ApplySettings { force: false });
        }
    })
    .response
    .on_hover_text(
        "Индексация работает в фоне с пониженным приоритетом; чем ниже нагрузка, тем дольше первая индексация",
    );
    ui.add_space(4.0);
    ui.label(RichText::new("Папка на другом компьютере").size(13.5).strong());
    crate::views::network_input(ui, app, pal, actions);
    ui.add_space(4.0);
    if ui
        .checkbox(&mut app.settings.watch, "Следить за изменениями и обновлять индекс автоматически")
        .on_hover_text("Новые и изменённые файлы попадают в поиск сразу, без повторной индексации")
        .changed()
    {
        actions.push(Action::ApplySettings { force: false });
    }
}

fn status_section(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Состояние индекса").strong().size(16.0));
    let Some(backend) = &app.backend else {
        ui.label("Индекс недоступен.");
        return;
    };
    let st = backend.state.clone();
    ui.label(format!("Файлов в индексе: {}", app.num_docs));

    if st.running.load(Ordering::Relaxed) {
        let total = st.total.load(Ordering::Relaxed);
        let done = st.done.load(Ordering::Relaxed);
        if total > 0 {
            ui.add(ProgressBar::new((done as f32 / total as f32).clamp(0.0, 1.0)).text(format!("{done} из {total}")));
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Обход папок: найдено файлов — {}", st.scanned.load(Ordering::Relaxed)));
            });
        }
        let current = st.current.lock().unwrap().clone();
        if !current.is_empty() {
            ui.label(RichText::new(shorten(&current, 70)).size(12.5).color(pal.muted));
        }
        if ui.button("Остановить индексацию и слежение").clicked() {
            actions.push(Action::StopIndexing);
        }
    } else {
        ui.horizontal(|ui| {
            if ui.add_enabled(!app.settings.roots.is_empty(), egui::Button::new("Обновить сейчас")).clicked()
            {
                actions.push(Action::ApplySettings { force: false });
            }
            if ui
                .add_enabled(!app.settings.roots.is_empty(), egui::Button::new("Переиндексировать всё"))
                .on_hover_text("Заново прочитать все файлы, даже не изменившиеся")
                .clicked()
            {
                actions.push(Action::ApplySettings { force: true });
            }
        });
    }

    if let Some(error) = st.error.lock().unwrap().as_ref() {
        ui.label(RichText::new(error).color(pal.warn));
    }
    if let Some(s) = st.summary.lock().unwrap().as_ref() {
        ui.add_space(4.0);
        let head = if s.cancelled { "Прервано" } else { "Последний проход" };
        ui.label(
            RichText::new(format!(
                "{head}: файлов найдено {}, добавлено или обновлено {}, без изменений {}, удалено {} · {:.1} с · текста {}",
                s.scanned,
                s.indexed,
                s.unchanged,
                s.removed,
                s.elapsed.as_secs_f64(),
                format_size(s.text_bytes)
            ))
            .size(12.5)
            .color(pal.muted),
        );
        if s.encrypted > 0 {
            ui.label(
                RichText::new(format!("Защищены паролем (ищутся только по имени): {}", s.encrypted))
                    .size(12.5)
                    .color(pal.muted),
            );
        }
        if s.recognized > 0 {
            ui.label(
                RichText::new(format!("Текст распознан на сканах: {} (возможны ошибки)", s.recognized))
                    .size(12.5)
                    .color(pal.muted),
            );
        }
        if s.scans > 0 {
            ui.label(
                RichText::new(format!("Сканы без распознавания (ищутся только по имени): {}", s.scans))
                    .size(12.5)
                    .color(pal.muted),
            );
        }
        if s.deferred > 0 {
            ui.label(
                RichText::new(format!("Не читались в прошлый раз, повторная попытка позже: {}", s.deferred))
                    .size(12.5)
                    .color(pal.muted),
            )
            .on_hover_text("Файлы с ошибкой чтения пробуются снова через несколько часов, а не при каждом проходе");
        }
        if s.truncated > 0 {
            ui.label(
                RichText::new(format!("Прочитаны не полностью (лимит текста): {}", s.truncated))
                    .size(12.5)
                    .color(pal.muted),
            );
        }
        if s.failed_total > 0 {
            egui::CollapsingHeader::new(
                RichText::new(format!("Не удалось прочитать файлов: {}", s.failed_total)).color(pal.warn),
            )
            .id_salt("failed")
            .show(ui, |ui| {
                ui.label(RichText::new("По имени эти файлы всё равно находятся.").size(12.5).color(pal.muted));
                for (path, why) in s.failed.iter().take(100) {
                    ui.label(RichText::new(format!("{}\n    {}", path.display(), why)).size(12.5));
                }
            });
        }
        if s.walk_errors_total > 0 {
            egui::CollapsingHeader::new(format!("Нет доступа к {} объектам", s.walk_errors_total))
                .id_salt("walk_errors")
                .show(ui, |ui| {
                    for e in s.walk_errors.iter().take(50) {
                        ui.label(RichText::new(e).size(12.5));
                    }
                });
        }
    }
    let log: Vec<String> = st.log.lock().unwrap().iter().rev().take(30).cloned().collect();
    if !log.is_empty() {
        egui::CollapsingHeader::new("Журнал").id_salt("log").show(ui, |ui| {
            for line in log {
                ui.label(RichText::new(line).size(12.5).color(pal.muted));
            }
        });
    }
}

/// Распознавание текста на сканах: переключатель и пояснение, что есть в системе.
fn ocr_setting(ui: &mut Ui, app: &mut App, pal: &Palette) {
    let info = file_search::extract::ocr::engine_info();
    ui.add_enabled_ui(info.available, |ui| {
        ui.checkbox(&mut app.settings.ocr, "Распознавать текст на сканах PDF (картинках вместо текста)")
            .on_hover_text("Медленно: страница занимает около секунды, поэтому первая индексация сканов дольше. Файлы со сканами находятся и без этого, но только по имени.");
    });
    if info.available && info.russian {
        ui.label(
            RichText::new(format!(
                "Распознавание доступно, язык: {}. Применится при следующем обновлении индекса.",
                info.language
            ))
            .size(12.5)
            .color(pal.muted),
        );
    } else {
        // Нет языка или он не русский: в обоих случаях пользователю нужно знать, что делать.
        ui.label(RichText::new(&info.note).size(12.5).color(pal.warn));
    }
}

fn advanced_section(ui: &mut Ui, app: &mut App, actions: &mut Vec<Action>) {
    let pal = theme::palette(ui.visuals().dark_mode);
    egui::CollapsingHeader::new(RichText::new("Дополнительно").strong().size(16.0)).id_salt("advanced").show(
        ui,
        |ui| {
            ui.label("Не индексировать (шаблоны, по одному в строке), например: *.tmp или **/Резерв/**");
            ui.add(
                TextEdit::multiline(&mut app.excludes_text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("*.tmp"),
            );
            ui.checkbox(
                &mut app.settings.default_excludes,
                "Пропускать служебные папки ($RECYCLE.BIN, .git, node_modules…)",
            );
            ocr_setting(ui, app, &pal);
            ui.horizontal(|ui| {
                ui.label("Максимум текста из одного файла, МБ:");
                ui.add(egui::DragValue::new(&mut app.settings.max_text_mb).range(1..=512));
            });
            ui.horizontal(|ui| {
                ui.label("Полная проверка папок раз в, минут:");
                ui.add(egui::DragValue::new(&mut app.settings.rescan_minutes).range(5..=10080));
            })
            .response
            .on_hover_text(
                "Обычно изменения подхватываются сразу по уведомлениям системы; полная проверка — страховка на случай \
                 пропущенных уведомлений. Для сетевых папок она идёт каждые 10 минут независимо от этого значения.",
            );
            ui.horizontal(|ui| {
                ui.label("Тема:");
                ComboBox::from_id_salt("theme")
                    .selected_text(match app.settings.theme {
                        Theme::Auto => "Как в системе",
                        Theme::Light => "Светлая",
                        Theme::Dark => "Тёмная",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.settings.theme, Theme::Auto, "Как в системе");
                        ui.selectable_value(&mut app.settings.theme, Theme::Light, "Светлая");
                        ui.selectable_value(&mut app.settings.theme, Theme::Dark, "Тёмная");
                    });
            });
            ui.add_space(4.0);
            if ui
                .add_enabled(true, egui::Button::new("Применить"))
                .on_hover_text("Сохранить настройки и обновить индекс")
                .clicked()
            {
                actions.push(Action::ApplySettings { force: false });
            }
        },
    );
}

fn updates_section(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Обновления").strong().size(16.0));
    ui.label(RichText::new(format!("Поиск файлов · версия {}", env!("CARGO_PKG_VERSION"))).size(12.5).color(pal.muted));
    if ui
        .checkbox(&mut app.settings.check_updates, "Проверять обновления автоматически (раз в сутки)")
        .on_hover_text(
            "Программа только спрашивает у GitHub номер последней версии; ничего не ставится без вашей кнопки",
        )
        .changed()
    {
        actions.push(Action::SaveSettings);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!app.updater.busy(), egui::Button::new("Проверить сейчас")).clicked() {
            actions.push(Action::CheckUpdates);
        }
        match &app.updater.phase {
            Phase::Idle => {
                if app.settings.last_update_check > 0 {
                    ui.label(
                        RichText::new("Последняя проверка: ".to_string() + &ago(app.settings.last_update_check))
                            .size(12.5)
                            .color(pal.muted),
                    );
                }
            }
            Phase::Checking => {
                ui.spinner();
                ui.label(RichText::new("Проверяю…").size(12.5).color(pal.muted));
            }
            Phase::UpToDate => {
                ui.label(RichText::new("У вас последняя версия").size(12.5).color(pal.ok));
            }
            Phase::Available(release) => {
                ui.label(
                    RichText::new(format!("Доступна версия {} от {}", release.version, release.published))
                        .size(12.5)
                        .color(pal.accent),
                );
                if ui.button(if release.installable_asset().is_some() { "Обновить" } else { "Скачать" }).clicked()
                {
                    actions.push(Action::DownloadUpdate);
                }
                if ui.button("Что нового").clicked() {
                    actions.push(Action::OpenUrl(release.url.clone()));
                }
            }
            Phase::Downloading { done, total, .. } => {
                let frac = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
                ui.add(ProgressBar::new(frac).desired_width(180.0).text("Загрузка…"));
            }
            Phase::Ready { release, .. } => {
                ui.label(RichText::new(format!("Версия {} загружена", release.version)).size(12.5).color(pal.ok));
                if ui.button("Перезапустить и обновить").clicked() {
                    actions.push(Action::InstallUpdate);
                }
            }
            Phase::Failed { message, .. } => {
                ui.label(RichText::new(format!("Не удалось: {}", shorten(message, 80))).size(12.5).color(pal.warn))
                    .on_hover_text(message.as_str());
            }
        }
    });
    if app.settings.skipped_version.is_some() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Версия {} пропущена", app.settings.skipped_version.as_deref().unwrap_or("")))
                    .size(12.5)
                    .color(pal.muted),
            );
            if ui.small_button("Снова предлагать").clicked() {
                app.settings.skipped_version = None;
                actions.push(Action::CheckUpdates);
            }
        });
    }
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new(RichText::new("Все выпуски на GitHub").size(12.5)).frame(false)).clicked()
        {
            actions.push(Action::OpenUrl(file_search::update::RELEASES_URL.to_string()));
        }
    });
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

fn index_section(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Индекс").strong().size(16.0));
    ui.label(RichText::new(app.index_dir.display().to_string()).size(12.5).color(pal.muted));
    ui.horizontal(|ui| {
        if ui.button("Открыть папку индекса").clicked() {
            actions.push(Action::OpenIndexFolder);
        }
        if app.confirm_reset {
            ui.label("Удалить индекс и построить заново?");
            if ui
                .add(egui::Button::new(RichText::new("Да, пересоздать").color(Color32::WHITE)).fill(pal.danger))
                .clicked()
            {
                actions.push(Action::RecreateIndex);
            }
            if ui.button("Отмена").clicked() {
                app.confirm_reset = false;
            }
        } else if ui.button("Пересоздать индекс…").clicked() {
            app.confirm_reset = true;
        }
    });
}
