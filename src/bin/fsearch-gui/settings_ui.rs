//! Окно «Индексация и настройки».

use std::sync::atomic::Ordering;

use egui::{Align2, Color32, ComboBox, Context, ProgressBar, RichText, TextEdit, Ui};
use file_search::platform::format_size;

use crate::app::{Action, App};
use crate::settings::Load;
use crate::settings::Theme;
use crate::theme::{self, Palette};
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
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Поиск файлов · версия {}", env!("CARGO_PKG_VERSION")))
                        .size(12.5)
                        .color(pal.muted),
                );
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
            if ui.small_button("×").on_hover_text("Убрать из поиска и удалить из индекса").clicked()
            {
                actions.push(Action::RemoveRoot(i));
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
            Load::Minimal => "Минимальная (1 поток)",
            Load::Moderate => "Умеренная (половина ядер)",
            Load::Maximum => "Максимальная (быстрее всего)",
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

fn advanced_section(ui: &mut Ui, app: &mut App, actions: &mut Vec<Action>) {
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
            ui.horizontal(|ui| {
                ui.label("Максимум текста из одного файла, МБ:");
                ui.add(egui::DragValue::new(&mut app.settings.max_text_mb).range(1..=512));
            });
            ui.horizontal(|ui| {
                ui.label("Полная проверка раз в, минут:");
                ui.add(egui::DragValue::new(&mut app.settings.rescan_minutes).range(1..=1440));
            });
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

fn index_section(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Индекс").strong().size(16.0));
    ui.label(RichText::new(app.index_dir.display().to_string()).size(12.5).color(pal.muted));
    ui.horizontal(|ui| {
        if ui.button("Открыть папку индекса").clicked() {
            actions.push(Action::OpenIndexFolder);
        }
        if app.confirm_reset {
            ui.label("Удалить индекс и построить заново?");
            if ui.button(RichText::new("Да, пересоздать").color(Color32::WHITE)).clicked() {
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
