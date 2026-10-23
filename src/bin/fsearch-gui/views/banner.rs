//! Полоса «есть новая версия» над панелями.

use super::*;
use crate::app::{Action, App};
use crate::theme::{self, FONT_BODY, Palette};
use crate::updater::Phase;
use egui::{RichText, Ui};
use file_search::platform::format_size;

pub(super) fn update_banner_visible(app: &App) -> bool {
    !app.updater.dismissed
        && match &app.updater.phase {
            Phase::Available(_) | Phase::Downloading { .. } | Phase::Ready { .. } => true,
            // Ошибка показывается в полосе, только если обновление уже пытались поставить.
            Phase::Failed { release, .. } => release.is_some(),
            Phase::Idle | Phase::Checking | Phase::UpToDate => false,
        }
}

/// Полоса под шапкой: «Доступна новая версия», ход загрузки, предложение перезапуститься.
pub(super) fn update_banner(ui: &mut Ui, app: &mut App, pal: &Palette, actions: &mut Vec<Action>) {
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
