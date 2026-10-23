//! Строка состояния внизу и экран ошибки индекса.

use super::*;
use crate::app::{Action, App};
use crate::theme::{self, FONT_CAPTION, Icon, Palette, SPACE_SM};
use egui::{Align, Color32, Layout, RichText, Sense, Ui, vec2};
use std::sync::atomic::Ordering;

pub(super) fn status_dot(ui: &mut Ui, color: Color32) {
    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(dot.center(), 4.0, color);
}

pub(super) fn status_bar(ui: &mut Ui, app: &App, pal: &Palette, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        let (left, left_color) = if app.search.search_error.is_some() && !app.search.form.is_empty() {
            ("Ошибка поиска".to_string(), pal.danger)
        } else if app.index.num_docs > 0 {
            (format!("Файлов в индексе: {}", thousands(app.index.num_docs)), pal.muted)
        } else {
            (String::new(), pal.muted)
        };
        ui.label(RichText::new(left).size(FONT_CAPTION).color(left_color));

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let Some(backend) = &app.index.backend else { return };
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
                        let eta = match app.index.index_eta_secs {
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

pub(super) fn index_error(ui: &mut Ui, confirm: bool, error: &str, pal: &Palette, actions: &mut Vec<Action>) {
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
