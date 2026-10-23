//! Главное окно: раскладка панелей и всплывающие сообщения.

use crate::app::{Action, App, Toast};
use crate::theme::{self, Kind, LINE_W, RADIUS_MD, SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XS};
use crate::updater::Phase;
use egui::{Align2, Context, Frame, Id, Margin, Order, Sense, Stroke, vec2};
use std::time::Duration;

/// Ширина окна, начиная с которой справа показывается панель подробностей о выбранном файле.
mod banner;
mod common;
mod details;
mod header;
mod results;
mod status;
mod welcome;

use banner::*;
use common::*;
use details::*;
use header::*;
use results::*;
use status::*;
use welcome::*;

pub use common::{shorten, thousands};
pub use welcome::network_input;

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
    let can_show = app.index.backend_error.is_none()
        && app.search.search_error.is_none()
        && !app.search.form.is_empty()
        && app.search.selected.is_some_and(|i| i < app.search.results.len());
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
                    if app.search.total > 0 {
                        ui.label(theme::caption(
                            &pal,
                            format!("Найдено файлов: {}", thousands(app.search.total as u64)),
                        ));
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
