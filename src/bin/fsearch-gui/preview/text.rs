//! Текст целиком: код и текстовые файлы — как в редакторе (номера строк, моноширинный шрифт), документы — абзацами
//! с переносом по ширине панели. Подсветка слов запроса и переход между совпадениями в обоих видах одинаковые.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, FontId, Layout, RichText, ScrollArea, Ui, vec2};

use egui::Rect;

use crate::theme::{self, CONTROL_H, FONT_BODY, FONT_CAPTION, FONT_MONO, Palette, RADIUS_SM, SPACE_XS};

/// Очень длинные строки (минифицированный код) обрезаем при показе: иначе окно зависает.
const MAX_LINE_CHARS: usize = 2000;
/// Сколько мест с совпадениями запоминаем для кнопок «вперёд / назад».
const MAX_MATCH_LINES: usize = 5000;
/// Расстояние между абзацами документа.
const PARA_GAP: f32 = 6.0;
/// Поля вокруг текста документа.
const PROSE_PAD: f32 = 8.0;

pub struct TextDoc {
    lines: Vec<String>,
    /// Длина самой длинной строки в символах: от неё зависит ширина области прокрутки.
    longest: usize,
    /// Текст документа (абзацы с переносом), а не код.
    prose: bool,
    /// Текст подрезан при чтении: показываем об этом пометку.
    truncated: bool,
    /// Положение каждого абзаца по высоте при ширине `laid_width` (с последним элементом — общая высота).
    starts: Vec<f32>,
    laid_width: f32,
    terms: Vec<String>,
    /// Номера строк (с нуля), где есть совпадения.
    matches: Vec<usize>,
    /// Какое по счёту совпадение сейчас выбрано.
    current: usize,
    /// Нужно прокрутить к выбранному совпадению.
    scroll_to_match: bool,
}

impl TextDoc {
    pub fn new(text: &str, truncated: bool, terms: Vec<String>, prose: bool) -> TextDoc {
        // Документ переносим по ширине, поэтому абзацы не обрезаем; у кода обрезаем очень длинные строки.
        let lines: Vec<String> = text.split('\n').map(|l| if prose { l.to_string() } else { clip_line(l) }).collect();
        let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let matches = match_lines(&lines, &terms);
        TextDoc {
            lines,
            longest,
            prose,
            truncated,
            starts: Vec::new(),
            laid_width: 0.0,
            terms,
            matches,
            current: 0,
            scroll_to_match: true,
        }
    }

    /// «12 строк» для кода, «12 абзацев» для документа — с правильным окончанием.
    pub fn count_text(&self) -> String {
        let n = self.lines.len();
        let (one, few, many) = if self.prose {
            ("абзац", "абзаца", "абзацев")
        } else {
            ("строка", "строки", "строк")
        };
        let word = match (n % 10, n % 100) {
            (1, m) if m != 11 => one,
            (2..=4, m) if !(12..=14).contains(&m) => few,
            _ => many,
        };
        format!("{n} {word}")
    }

    /// Запрос изменился: подсветку и список совпадений пересчитываем.
    pub fn set_terms(&mut self, terms: Vec<String>) {
        self.matches = match_lines(&self.lines, &terms);
        self.terms = terms;
        // От подсветки зависит раскладка абзацев: пересчитаем её при следующем показе.
        self.starts.clear();
        self.current = 0;
        self.scroll_to_match = true;
    }

    /// Переход к следующему (или предыдущему) совпадению.
    pub fn step(&mut self, forward: bool) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len();
        self.current = if forward { (self.current + 1) % n } else { (self.current + n - 1) % n };
        self.scroll_to_match = true;
    }

    pub fn show(&mut self, ui: &mut Ui, pal: &Palette) {
        self.show_toolbar(ui, pal);
        if self.truncated {
            ui.label(
                RichText::new("Показана только начальная часть текста: файл большой")
                    .size(FONT_CAPTION)
                    .color(pal.warn),
            );
        }
        ui.add_space(SPACE_XS);
        if self.prose {
            self.show_prose(ui, pal);
        } else {
            self.show_lines(ui, pal);
        }
    }

    /// Документ: абзацы с переносом. Рисуем только видимые абзацы, поэтому длинный документ не тормозит.
    fn show_prose(&mut self, ui: &mut Ui, pal: &Palette) {
        let width = (ui.available_width() - 2.0 * PROSE_PAD).max(40.0);
        if self.starts.is_empty() || (self.laid_width - width).abs() > 0.5 {
            self.relayout(ui, width, pal);
        }
        let total = self.starts.last().copied().unwrap_or(0.0);
        let text_color = ui.visuals().text_color();
        let current_line = self.matches.get(self.current).copied();
        let mut scroll = ScrollArea::vertical().auto_shrink([false, false]).id_salt("preview-prose");
        if std::mem::take(&mut self.scroll_to_match)
            && let Some(line) = current_line
        {
            scroll = scroll.vertical_scroll_offset((self.starts[line] - 2.0 * PARA_GAP).max(0.0));
        }
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(egui::Stroke::new(theme::LINE_W, pal.line))
            .corner_radius(theme::radius(RADIUS_SM))
            .show(ui, |ui| {
                scroll.show_viewport(ui, |ui, viewport| {
                    ui.set_height(total + 2.0 * PROSE_PAD);
                    let origin = ui.min_rect().min + vec2(PROSE_PAD, PROSE_PAD);
                    // Первый видимый абзац ищем по высотам, дальше идём, пока не выйдем за нижний край.
                    let first = self.starts.partition_point(|&y| y <= viewport.min.y).saturating_sub(1);
                    for i in first..self.lines.len() {
                        let y = self.starts[i];
                        if y > viewport.max.y {
                            break;
                        }
                        let galley = ui.fonts_mut(|f| f.layout_job(self.paragraph(i, width, pal)));
                        if current_line == Some(i) {
                            let rect = Rect::from_min_size(
                                origin + vec2(-PROSE_PAD, y),
                                vec2(width + 2.0 * PROSE_PAD, galley.size().y),
                            );
                            ui.painter().rect_filled(rect, RADIUS_SM, pal.line_hl);
                        }
                        ui.painter().galley(origin + vec2(0.0, y), galley, text_color);
                    }
                });
            });
    }

    /// Пересчитывает высоты всех абзацев для ширины `width`.
    fn relayout(&mut self, ui: &mut Ui, width: f32, pal: &Palette) {
        let mut starts = Vec::with_capacity(self.lines.len() + 1);
        let mut y = 0.0;
        for i in 0..self.lines.len() {
            starts.push(y);
            let galley = ui.fonts_mut(|f| f.layout_job(self.paragraph(i, width, pal)));
            y += galley.size().y + PARA_GAP;
        }
        starts.push(y);
        self.starts = starts;
        self.laid_width = width;
    }

    /// Абзац `i` с подсветкой совпадений, перенесённый по `width`.
    fn paragraph(&self, i: usize, width: f32, pal: &Palette) -> LayoutJob {
        let font = theme::font(FONT_BODY);
        let normal = TextFormat { font_id: font.clone(), color: pal.text, ..Default::default() };
        let marked = TextFormat { font_id: font, color: pal.mark_fg, background: pal.mark_bg, ..Default::default() };
        let line = &self.lines[i];
        let mut job = LayoutJob::default();
        job.wrap.max_width = width;
        if line.is_empty() {
            // Пустой абзац всё равно должен занимать строку.
            job.append(" ", 0.0, normal);
            return job;
        }
        let mut pos = 0;
        for (s, e) in find_marks(line, &self.terms) {
            job.append(&line[pos..s], 0.0, normal.clone());
            job.append(&line[s..e], 0.0, marked.clone());
            pos = e;
        }
        job.append(&line[pos..], 0.0, normal);
        job
    }

    fn show_toolbar(&mut self, ui: &mut Ui, pal: &Palette) {
        ui.horizontal(|ui| {
            let info = if self.terms.is_empty() {
                self.count_text()
            } else if self.matches.is_empty() {
                "Совпадений в тексте нет".to_string()
            } else {
                format!(
                    "Совпадение {} из {}{}",
                    self.current + 1,
                    self.matches.len(),
                    if self.matches.len() >= MAX_MATCH_LINES { "+" } else { "" }
                )
            };
            ui.label(theme::small(pal, info));
            if self.matches.is_empty() {
                return;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let arrow = |t: &str| {
                    egui::Button::new(RichText::new(t).size(theme::FONT_BODY).color(pal.text))
                        .min_size(vec2(CONTROL_H, CONTROL_H))
                };
                if ui.add(arrow("▶")).on_hover_text("Следующее совпадение (F3)").clicked() {
                    self.step(true);
                }
                if ui.add(arrow("◀")).on_hover_text("Предыдущее совпадение (Shift+F3)").clicked()
                {
                    self.step(false);
                }
            });
        });
    }

    fn show_lines(&mut self, ui: &mut Ui, pal: &Palette) {
        let font = FontId::monospace(FONT_MONO);
        let (row_height, char_width) = ui.fonts_mut(|f| (f.row_height(&font) + 2.0, f.glyph_width(&font, '0')));
        let digits = self.lines.len().to_string().len().max(3);
        let content_width = (digits + 2 + self.longest) as f32 * char_width + 16.0;
        let text_color = ui.visuals().text_color();
        let current_line = self.matches.get(self.current).copied();
        let editor_bg = ui.visuals().extreme_bg_color;
        let normal = TextFormat { font_id: font.clone(), color: text_color, ..Default::default() };
        let marked =
            TextFormat { font_id: font.clone(), color: pal.mark_fg, background: pal.mark_bg, ..Default::default() };
        let gutter = TextFormat { font_id: font.clone(), color: pal.gutter, ..Default::default() };

        let mut scroll = ScrollArea::both().auto_shrink([false, false]).id_salt("preview-text");
        if std::mem::take(&mut self.scroll_to_match)
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
                scroll.show_rows(ui, row_height, self.lines.len(), |ui, rows| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for i in rows {
                        let line = &self.lines[i];
                        let mut job = LayoutJob::default();
                        job.wrap.max_width = f32::INFINITY;
                        job.append(&format!("{:>digits$}  ", i + 1), 0.0, gutter.clone());
                        let mut pos = 0;
                        for (s, e) in find_marks(line, &self.terms) {
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
                            ui.painter().rect_filled(rect, 0.0, pal.line_hl);
                        }
                        let galley = ui.fonts_mut(|f| f.layout_job(job));
                        ui.painter().galley(rect.min + vec2(6.0, 1.0), galley, text_color);
                    }
                });
            });
    }
}

fn clip_line(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((cut, _)) => format!("{} …", &line[..cut]),
        None => line.to_string(),
    }
}

/// Байтовые диапазоны совпадений в строке (регистр не важен).
pub fn find_marks(line: &str, terms: &[String]) -> Vec<(usize, usize)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_agree_with_number() {
        let lines = |n: usize| ["x"; 12][..n].join("\n");
        assert_eq!(TextDoc::new("a", false, vec![], false).count_text(), "1 строка");
        assert_eq!(TextDoc::new("a\nb\nc", false, vec![], false).count_text(), "3 строки");
        assert_eq!(TextDoc::new("a", false, vec![], true).count_text(), "1 абзац");
        assert_eq!(TextDoc::new(&lines(12), false, vec![], true).count_text(), "12 абзацев");
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

    #[test]
    fn stepping_wraps_around_matches() {
        let mut doc = TextDoc::new("срок\nпусто\nсрок", false, vec!["срок".to_string()], false);
        assert_eq!(doc.matches, [0, 2]);
        doc.step(false);
        assert_eq!(doc.current, 1);
        doc.step(true);
        assert_eq!(doc.current, 0);
    }
}
