//! Просмотр текстового файла целиком, как в редакторе: номера строк, моноширинный шрифт, подсветка найденного.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Context, FontId, Layout, RichText, ScrollArea, Ui, vec2};
use file_search::extract::text::read_for_view;
use file_search::query::{self, Scope, Text};

use crate::theme::{self, CONTROL_H, FONT_CAPTION, FONT_MONO, Palette, RADIUS_SM, SPACE_XS};

/// Сколько байт файла показываем. Больше — только начало, с пометкой.
const MAX_VIEW_BYTES: usize = 4 << 20;
/// Очень длинные строки (минифицированный код) обрезаем при показе: иначе окно зависает.
const MAX_LINE_CHARS: usize = 2000;
/// Сколько мест с совпадениями запоминаем для кнопок «вперёд / назад».
const MAX_MATCH_LINES: usize = 5000;

enum State {
    Loading,
    Ready(Doc),
    Failed(String),
}

struct Doc {
    lines: Vec<String>,
    /// Длина самой длинной строки в символах: от неё зависит ширина области прокрутки.
    longest: usize,
    truncated: bool,
    /// Номера строк (с нуля), где есть совпадения.
    matches: Vec<usize>,
    /// Какое по счёту совпадение сейчас выбрано.
    current: usize,
    /// Нужно прокрутить к выбранному совпадению.
    scroll_to_match: bool,
}

pub struct Preview {
    path: PathBuf,
    terms: Vec<String>,
    state: State,
    rx: Receiver<Result<(String, bool), String>>,
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

/// Байтовые диапазоны совпадений в строке (регистр не важен).
fn find_marks(line: &str, terms: &[String]) -> Vec<(usize, usize)> {
    let lower = line.to_lowercase();
    // Если смена регистра сдвинула символы, границы нельзя перенести на исходную строку.
    if terms.is_empty() || lower.len() != line.len() {
        return Vec::new();
    }
    let mut marks: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(term.as_str()) {
            let start = from + pos;
            let end = start + term.len();
            if line.is_char_boundary(start) && line.is_char_boundary(end) {
                marks.push((start, end));
            }
            from = end;
        }
    }
    marks.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in marks {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

impl Preview {
    /// Начинает читать файл в фоновом потоке.
    pub fn open(ctx: &Context, path: PathBuf, query_text: &str) -> Preview {
        let (tx, rx) = channel();
        let (file, ctx) = (path.clone(), ctx.clone());
        thread::spawn(move || {
            let result = read_for_view(&file, MAX_VIEW_BYTES).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => "Файл не найден — возможно, его удалили или переместили".to_string(),
                std::io::ErrorKind::PermissionDenied => "Нет доступа к файлу".to_string(),
                _ => format!("Не удалось прочитать файл: {e}"),
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        Preview { path, terms: highlight_terms(query_text), state: State::Loading, rx }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Запрос изменился: подсветку нужно пересчитать.
    pub fn set_query(&mut self, query_text: &str) {
        let terms = highlight_terms(query_text);
        if terms == self.terms {
            return;
        }
        self.terms = terms;
        if let State::Ready(doc) = &mut self.state {
            doc.matches = match_lines(&doc.lines, &self.terms);
            doc.current = 0;
            doc.scroll_to_match = true;
        }
    }

    /// Переход к следующему (или предыдущему) совпадению — для F3 / Shift+F3.
    pub fn step_match(&mut self, forward: bool) {
        if let State::Ready(doc) = &mut self.state
            && !doc.matches.is_empty()
        {
            let n = doc.matches.len();
            doc.current = if forward { (doc.current + 1) % n } else { (doc.current + n - 1) % n };
            doc.scroll_to_match = true;
        }
    }

    fn poll(&mut self) {
        if let (State::Loading, Ok(result)) = (&self.state, self.rx.try_recv()) {
            self.state = match result {
                Ok((text, truncated)) => {
                    let lines: Vec<String> = text.split('\n').map(clip_line).collect();
                    let matches = match_lines(&lines, &self.terms);
                    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
                    State::Ready(Doc { lines, longest, truncated, matches, current: 0, scroll_to_match: true })
                }
                Err(e) => State::Failed(e),
            };
        }
    }

    /// Рисует просмотр в оставшемся месте панели.
    pub fn show(&mut self, ui: &mut Ui, pal: &Palette) {
        self.poll();
        let terms = &self.terms;
        match &mut self.state {
            State::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::small(pal, "Открываю файл…"));
                });
            }
            State::Failed(error) => {
                ui.label(RichText::new(error.as_str()).size(theme::FONT_SMALL).color(pal.warn));
            }
            State::Ready(doc) => doc_view(ui, doc, terms, pal),
        }
    }
}

fn clip_line(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((cut, _)) => format!("{} …", &line[..cut]),
        None => line.to_string(),
    }
}

/// Сколько разных слов запроса есть в строке (регистр не важен).
fn terms_in_line(line: &str, terms: &[String]) -> usize {
    let lower = line.to_lowercase();
    terms.iter().filter(|t| lower.contains(t.as_str())).count()
}

/// Строки, к которым ведут кнопки «вперёд / назад»: сначала те, где есть все слова запроса, а если таких нет —
/// где есть хотя бы одно: так открытие файла сразу показывает самое подходящее место.
fn match_lines(lines: &[String], terms: &[String]) -> Vec<usize> {
    if terms.is_empty() {
        return Vec::new();
    }
    let counts: Vec<(usize, usize)> =
        lines.iter().enumerate().map(|(i, l)| (i, terms_in_line(l, terms))).filter(|(_, n)| *n > 0).collect();
    let best = counts.iter().map(|(_, n)| *n).max().unwrap_or(0);
    counts.into_iter().filter(|(_, n)| *n == best).map(|(i, _)| i).take(MAX_MATCH_LINES).collect()
}

fn doc_view(ui: &mut Ui, doc: &mut Doc, terms: &[String], pal: &Palette) {
    let font = FontId::monospace(FONT_MONO);
    let (row_height, char_width) = ui.fonts_mut(|f| (f.row_height(&font) + 2.0, f.glyph_width(&font, '0')));
    let digits = doc.lines.len().to_string().len().max(3);

    ui.horizontal(|ui| {
        let info = if terms.is_empty() {
            format!("{} стр.", doc.lines.len())
        } else if doc.matches.is_empty() {
            "Совпадений в тексте нет".to_string()
        } else {
            format!(
                "Совпадение {} из {}{}",
                doc.current + 1,
                doc.matches.len(),
                if doc.matches.len() >= MAX_MATCH_LINES { "+" } else { "" }
            )
        };
        ui.label(theme::small(pal, info));
        if !doc.matches.is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let n = doc.matches.len();
                let arrow = |t: &str| {
                    egui::Button::new(RichText::new(t).size(theme::FONT_BODY).color(pal.text))
                        .min_size(vec2(CONTROL_H, CONTROL_H))
                };
                if ui.add(arrow("▶")).on_hover_text("Следующее совпадение (F3)").clicked() {
                    doc.current = (doc.current + 1) % n;
                    doc.scroll_to_match = true;
                }
                if ui.add(arrow("◀")).on_hover_text("Предыдущее совпадение (Shift+F3)").clicked()
                {
                    doc.current = (doc.current + n - 1) % n;
                    doc.scroll_to_match = true;
                }
            });
        }
    });
    if doc.truncated {
        ui.label(
            RichText::new(format!("Файл большой: показаны первые {} МБ", MAX_VIEW_BYTES >> 20))
                .size(FONT_CAPTION)
                .color(pal.warn),
        );
    }
    ui.add_space(SPACE_XS);

    let content_width = (digits + 2 + doc.longest) as f32 * char_width + 16.0;
    let text_color = ui.visuals().text_color();
    let current_line = doc.matches.get(doc.current).copied();
    let editor_bg = ui.visuals().extreme_bg_color;
    let gutter_color = pal.gutter;
    let line_hl = pal.line_hl;
    let normal = TextFormat { font_id: font.clone(), color: text_color, ..Default::default() };
    let marked =
        TextFormat { font_id: font.clone(), color: pal.mark_fg, background: pal.mark_bg, ..Default::default() };
    let gutter = TextFormat { font_id: font.clone(), color: gutter_color, ..Default::default() };

    let mut scroll = ScrollArea::both().auto_shrink([false, false]).id_salt("preview");
    if std::mem::take(&mut doc.scroll_to_match)
        && let Some(line) = current_line
    {
        // Совпадение показываем чуть ниже верхнего края, чтобы были видны строки перед ним.
        scroll = scroll.vertical_scroll_offset((line as f32 - 3.0).max(0.0) * row_height);
    }
    egui::Frame::new()
        .fill(editor_bg)
        .stroke(egui::Stroke::new(theme::LINE_W, pal.line))
        .corner_radius(theme::radius(RADIUS_SM))
        .show(ui, |ui| {
            scroll.show_rows(ui, row_height, doc.lines.len(), |ui, rows| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in rows {
                    let line = &doc.lines[i];
                    let mut job = LayoutJob::default();
                    job.wrap.max_width = f32::INFINITY;
                    job.append(&format!("{:>digits$}  ", i + 1), 0.0, gutter.clone());
                    let mut pos = 0;
                    for (s, e) in find_marks(line, terms) {
                        job.append(&line[pos..s], 0.0, normal.clone());
                        job.append(&line[s..e], 0.0, marked.clone());
                        pos = e;
                    }
                    job.append(&line[pos..], 0.0, normal.clone());
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(content_width.max(ui.available_width()), row_height),
                        egui::Sense::hover(),
                    );
                    if current_line == Some(i) {
                        ui.painter().rect_filled(rect, 0.0, line_hl);
                    }
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    ui.painter().galley(rect.min + vec2(6.0, 1.0), galley, text_color);
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_come_from_content_clauses_only() {
        assert_eq!(highlight_terms("Срок \"Действия договора\" -черновик name:отчёт"), ["срок", "действия договора"]);
        assert!(highlight_terms("").is_empty());
    }

    #[test]
    fn marks_are_case_insensitive_and_merged() {
        let terms = vec!["срок".to_string(), "ок д".to_string()];
        assert_eq!(find_marks("Срок действия", &terms), [(0, 11)]);
        assert!(find_marks("ничего", &terms).is_empty());
    }

    #[test]
    fn navigation_prefers_lines_with_all_terms() {
        let lines: Vec<String> = ["срок", "аренда", "срок и аренда", "пусто"].map(String::from).to_vec();
        let terms = vec!["срок".to_string(), "аренда".to_string()];
        assert_eq!(match_lines(&lines, &terms), [2]);
        assert_eq!(match_lines(&lines, &terms[..1]), [0, 2]);
        assert!(match_lines(&lines, &[]).is_empty());
    }
}
