//! Предпросмотр файла под результатом поиска: текст, документы (как текст), картинки и страницы PDF.
//! Файл читает фоновый поток (`load`); здесь — состояние, подсветка слов запроса и показ по типу файла.

mod load;
mod media;
mod text;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Context, Ui};
use file_search::platform::format_size;
use file_search::query::{self, Scope, Text};

use crate::theme::{self, Palette, SPACE_XS};
pub use load::Kind;
use load::Msg;
use media::{PdfView, Picture};
use text::TextDoc;

enum State {
    Loading,
    Text(TextDoc),
    Picture(Picture),
    Pdf(PdfView),
    /// Показать нечего: причина (ошибка или тип без предпросмотра).
    Message(String),
}

pub struct Preview {
    path: PathBuf,
    kind: Kind,
    size: u64,
    /// Слова запроса для подсветки в тексте.
    terms: Vec<String>,
    state: State,
    rx: Receiver<Msg>,
    /// Запросы страниц PDF фоновому потоку.
    pages: Sender<usize>,
}

/// Слова запроса, которые нужно подсветить в тексте: без исключённых и без поиска по имени или пути.
pub fn highlight_terms(query_text: &str) -> Vec<String> {
    let parsed = query::parse(query_text);
    let mut terms: Vec<String> = Vec::new();
    for clause in parsed.groups.iter().flatten() {
        if clause.negate || !matches!(clause.scope, Scope::Any | Scope::Content) {
            continue;
        }
        let (Text::Word(t) | Text::Phrase(t) | Text::Prefix(t) | Text::Fuzzy(t, _)) = &clause.text;
        let t = t.trim().to_lowercase();
        if !t.is_empty() && !terms.contains(&t) {
            terms.push(t);
        }
    }
    terms
}

impl Preview {
    /// Начинает читать файл в фоновом потоке.
    pub fn open(ctx: &Context, path: PathBuf, query_text: &str) -> Preview {
        let kind = Kind::of(&path);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let (pages, requests) = channel();
        let rx = load::spawn(ctx.clone(), path.clone(), kind, size, requests);
        Preview { path, kind, size, terms: highlight_terms(query_text), state: State::Loading, rx, pages }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Запрос изменился: подсветку нужно пересчитать.
    pub fn set_query(&mut self, query_text: &str) {
        let terms = highlight_terms(query_text);
        if terms == self.terms {
            return;
        }
        if let State::Text(doc) = &mut self.state {
            doc.set_terms(terms.clone());
        }
        self.terms = terms;
    }

    /// Переход к следующему (или предыдущему) совпадению — для F3 / Shift+F3.
    pub fn step_match(&mut self, forward: bool) {
        if let State::Text(doc) = &mut self.state {
            doc.step(forward);
        }
    }

    /// Забирает всё, что успел прочитать фоновый поток.
    fn poll(&mut self, ctx: &Context) {
        while let Ok(msg) = self.rx.try_recv() {
            let state = match msg {
                Msg::Page { index, image } => {
                    if let State::Pdf(view) = &mut self.state {
                        view.receive(ctx, index, image);
                    }
                    continue;
                }
                Msg::Text { text, truncated } => State::Text(TextDoc::new(&text, truncated, self.terms.clone())),
                Msg::Image(image) => State::Picture(Picture::new(ctx, image)),
                Msg::Pdf { pages } => State::Pdf(PdfView::new(pages)),
                Msg::Failed(error) => State::Message(error),
                Msg::Unsupported => State::Message(
                    "Для этого типа файла предпросмотра нет. Откройте файл кнопкой «Открыть».".to_string(),
                ),
            };
            self.state = state;
        }
    }

    /// Сведения о файле в одну строку: тип, размер, при необходимости число страниц.
    fn meta(&self) -> String {
        let mut parts = vec![self.kind.label().to_string(), format_size(self.size)];
        if let State::Pdf(view) = &self.state {
            parts.push(view.pages_text());
        }
        parts.join(" · ")
    }

    /// Рисует просмотр в оставшемся месте панели.
    pub fn show(&mut self, ui: &mut Ui, pal: &Palette) {
        self.poll(ui.ctx());
        ui.label(theme::small(pal, self.meta()));
        ui.add_space(SPACE_XS);
        match &mut self.state {
            State::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::small(pal, "Открываю файл…"));
                });
            }
            State::Text(doc) => doc.show(ui, pal),
            State::Picture(picture) => picture.show(ui, pal),
            State::Pdf(view) => view.show(ui, pal, &self.pages),
            State::Message(text) => {
                ui.label(egui::RichText::new(text.as_str()).size(theme::FONT_SMALL).color(pal.warn));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_come_from_content_clauses_only() {
        assert_eq!(highlight_terms("Срок \"Действия договора\" -черновик name:отчёт"), ["срок", "действия договора"]);
        assert!(highlight_terms("").is_empty());
    }
}
