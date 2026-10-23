//! Стартовый экран и ввод сетевой папки.

use super::*;
use crate::app::{Action, App};
use crate::theme::{self, CONTROL_H, FONT_BODY, FONT_CAPTION, Icon, Palette, RADIUS_SM, SPACE_LG, SPACE_SM, SPACE_XS};
use egui::{Align, Layout, Margin, RichText, TextEdit, Ui, vec2};

pub(super) fn onboarding(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
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

pub(super) const EXAMPLES: [(&str, &str); 8] = [
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
pub(super) const SHORTCUTS: [(&str, &str); 10] = [
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

pub(super) fn welcome(ui: &mut Ui, app: &App, pal: &Palette, actions: &mut Vec<Action>) {
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
