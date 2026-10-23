//! Общие элементы отрисовки: пустые состояния, обрезка текста, форматирование чисел.

use super::*;
use crate::theme::{self, Icon, Palette, SPACE_XL, SPACE_XS};
use egui::{Align, Color32, FontId, Layout, Rect, Ui, UiBuilder, pos2, vec2};

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
pub(super) fn fit_path(ui: &Ui, path: &str, width: f32, font: FontId, color: Color32) -> String {
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

/// Колонка по центру окна для пустых состояний.
pub(super) fn centered_column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let avail = ui.available_rect_before_wrap();
    let width = avail.width().min(COLUMN_WIDTH);
    let rect = Rect::from_min_size(pos2(avail.center().x - width / 2.0, avail.top()), vec2(width, avail.height()));
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center)), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// Единое пустое состояние: значок, заголовок, пояснение и (по желанию) кнопки или подсказки.
pub(super) fn empty_state(ui: &mut Ui, pal: &Palette, icon: Icon, title: &str, body: &str, add: impl FnOnce(&mut Ui)) {
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

/// Папка, в которой лежит файл (без имени самого файла).
pub(super) fn parent_of(path: &str) -> String {
    std::path::Path::new(path).parent().map(|p| p.display().to_string()).unwrap_or_else(|| path.to_string())
}

/// Пояснение к файлам, у которых текста нет: пароль или скан.
pub(super) fn status_note(status: &str) -> Option<&'static str> {
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
