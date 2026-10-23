//! Картинки и страницы PDF: по ширине панели или с выбранным масштабом, листание страниц.

use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::Sender;

use egui::{ColorImage, Context, Image, ScrollArea, TextureHandle, TextureOptions, Ui, Vec2, vec2};

use crate::theme::{self, CONTROL_H, FONT_BODY, Palette, SPACE_SM};

/// Сколько отрисованных страниц держим в памяти: каждая — несколько мегабайт.
const MAX_CACHED_PAGES: usize = 4;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 8.0;

/// Масштаб картинки: `None` — вписать в ширину панели (но не увеличивать).
fn fit_scale(size: Vec2, available_width: f32) -> f32 {
    (available_width / size.x).min(1.0)
}

/// Кнопки масштаба и подпись к нему. Возвращает новый масштаб, если его выбрали.
fn zoom_bar(ui: &mut Ui, pal: &Palette, current: f32, fitted: bool) -> Option<Option<f32>> {
    let mut choice = None;
    ui.horizontal(|ui| {
        let button = |t: &str| {
            egui::Button::new(egui::RichText::new(t).size(FONT_BODY).color(pal.text))
                .min_size(vec2(CONTROL_H, CONTROL_H))
        };
        if ui.add(button("−")).on_hover_text("Уменьшить").clicked() {
            choice = Some(Some((current / 1.25).max(MIN_ZOOM)));
        }
        ui.label(theme::small(pal, format!("{:.0}%", current * 100.0)));
        if ui.add(button("+")).on_hover_text("Увеличить").clicked() {
            choice = Some(Some((current * 1.25).min(MAX_ZOOM)));
        }
        if ui
            .add_enabled(!fitted, egui::Button::new(theme::small(pal, "По ширине")))
            .on_hover_text("Вписать в ширину панели")
            .clicked()
        {
            choice = Some(None);
        }
        ui.add_space(SPACE_SM);
    });
    choice
}

/// Одна картинка (растровый файл).
pub struct Picture {
    texture: TextureHandle,
    size: Vec2,
    /// Выбранный масштаб; `None` — по ширине панели.
    zoom: Option<f32>,
}

impl Picture {
    pub fn new(ctx: &Context, image: ColorImage) -> Picture {
        let size = vec2(image.size[0] as f32, image.size[1] as f32);
        let texture = ctx.load_texture("preview-image", image, TextureOptions::LINEAR);
        Picture { texture, size, zoom: None }
    }

    pub fn show(&mut self, ui: &mut Ui, pal: &Palette) {
        let fitted = fit_scale(self.size, ui.available_width());
        let current = self.zoom.unwrap_or(fitted);
        ui.label(theme::small(pal, format!("{} × {} пикс.", self.size.x as u32, self.size.y as u32)));
        if let Some(zoom) = zoom_bar(ui, pal, current, self.zoom.is_none()) {
            self.zoom = zoom;
        }
        let shown = self.size * self.zoom.unwrap_or(fitted);
        ScrollArea::both().auto_shrink([false, false]).id_salt("preview-picture").show(ui, |ui| {
            ui.add(Image::new(&self.texture).fit_to_exact_size(shown));
        });
    }
}

/// Страницы PDF: листание и кэш отрисованных страниц. Сами страницы рисует фоновый поток.
pub struct PdfView {
    count: usize,
    current: usize,
    /// `None` в значении — страница не нарисовалась.
    pages: BTreeMap<usize, Option<TextureHandle>>,
    requested: HashSet<usize>,
}

impl PdfView {
    pub fn new(count: usize) -> PdfView {
        PdfView { count, current: 0, pages: BTreeMap::new(), requested: HashSet::new() }
    }

    pub fn pages_text(&self) -> String {
        format!("{} стр.", self.count)
    }

    /// Пришла отрисованная страница.
    pub fn receive(&mut self, ctx: &Context, index: usize, image: Option<ColorImage>) {
        self.requested.remove(&index);
        let texture =
            image.map(|image| ctx.load_texture(format!("preview-page-{index}"), image, TextureOptions::LINEAR));
        self.pages.insert(index, texture);
        while self.pages.len() > MAX_CACHED_PAGES {
            // Убираем страницу, которая дальше всего от текущей.
            let far = self.pages.keys().copied().max_by_key(|k| k.abs_diff(self.current));
            if let Some(far) = far {
                self.pages.remove(&far);
            }
        }
    }

    pub fn show(&mut self, ui: &mut Ui, pal: &Palette, requests: &Sender<usize>) {
        if self.count == 0 {
            ui.label(theme::small(pal, "В файле нет страниц"));
            return;
        }
        ui.horizontal(|ui| {
            let arrow = |t: &str| {
                egui::Button::new(egui::RichText::new(t).size(FONT_BODY).color(pal.text))
                    .min_size(vec2(CONTROL_H, CONTROL_H))
            };
            if ui.add_enabled(self.current > 0, arrow("◀")).on_hover_text("Предыдущая страница").clicked()
            {
                self.current -= 1;
            }
            ui.label(theme::small(pal, format!("Страница {} из {}", self.current + 1, self.count)));
            if ui.add_enabled(self.current + 1 < self.count, arrow("▶")).on_hover_text("Следующая страница").clicked()
            {
                self.current += 1;
            }
        });
        // Текущую страницу и следующую просим заранее: переход на соседнюю тогда почти мгновенный.
        for index in [self.current, self.current + 1] {
            if index < self.count && !self.pages.contains_key(&index) && self.requested.insert(index) {
                let _ = requests.send(index);
            }
        }
        match self.pages.get(&self.current) {
            Some(Some(texture)) => {
                let size = texture.size_vec2();
                let fitted = fit_scale(size, ui.available_width());
                ScrollArea::both().auto_shrink([false, false]).id_salt("preview-pdf").show(ui, |ui| {
                    ui.add(Image::new(texture).fit_to_exact_size(size * fitted));
                });
            }
            Some(None) => {
                ui.label(theme::small(pal, "Эту страницу не удалось нарисовать"));
            }
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::small(pal, "Рисую страницу…"));
                });
            }
        }
    }
}
