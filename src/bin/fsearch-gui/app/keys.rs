//! Клавиатура и служебный режим снимка окна для проверки.

use super::*;
use egui::{Context, Id, Key, Modifiers};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(super) struct ScreenshotPlan {
    pub(super) path: PathBuf,
    pub(super) requested: bool,
    pub(super) started: Instant,
}

impl App {
    pub(super) fn handle_keys(&mut self, ctx: &Context) -> Vec<Action> {
        let mut actions = Vec::new();
        let search_id = Id::new(SEARCH_ID);
        let typing_elsewhere = ctx.memory(|m| m.focused()).is_some_and(|id| id != search_id);

        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F) || i.consume_key(Modifiers::COMMAND, Key::L)) {
            self.focus_search = true;
        }
        if self.show_settings && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.close_settings(ctx);
            return actions;
        }
        if typing_elsewhere || self.show_settings || egui::Popup::is_any_open(ctx) {
            return actions;
        }

        if self.search.history_pos.is_some_and(|p| self.settings.history.get(p) != Some(&self.search.form.query)) {
            self.search.history_pos = None;
        }
        // Ctrl+1…5 — фильтры по типам, Ctrl+0 — сбросить фильтры.
        const TYPE_KEYS: [Key; 5] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5];
        for (i, key) in TYPE_KEYS.iter().enumerate() {
            if ctx.input_mut(|inp| inp.consume_key(Modifiers::COMMAND, *key)) {
                self.search.form.groups[i] = !self.search.form.groups[i];
                self.mark_dirty(false);
            }
        }
        if ctx.input_mut(|inp| inp.consume_key(Modifiers::COMMAND, Key::Num0)) {
            actions.push(Action::ResetFilters);
        }
        // Стрелка вверх в пустой строке поиска — прошлые запросы (вниз — обратно).
        let search_focused = ctx.memory(|m| m.has_focus(search_id));
        let history_len = self.settings.history.len();
        let browsing = self.search.history_pos.is_some();
        if search_focused
            && self.search.results.is_empty()
            && history_len > 0
            && (self.search.form.query.is_empty() || browsing)
        {
            let (up, down) = ctx.input_mut(|inp| {
                (
                    inp.consume_key(Modifiers::NONE, Key::ArrowUp),
                    browsing && inp.consume_key(Modifiers::NONE, Key::ArrowDown),
                )
            });
            let next = match (self.search.history_pos, up, down) {
                (None, true, _) => Some(Some(0)),
                (Some(p), true, _) => Some(Some((p + 1).min(history_len - 1))),
                (Some(0), _, true) => Some(None),
                (Some(p), _, true) => Some(Some(p - 1)),
                _ => None,
            };
            if let Some(pos) = next {
                self.search.history_pos = pos;
                self.search.form.query = pos.map(|p| self.settings.history[p].clone()).unwrap_or_default();
                self.focus_search = true;
                self.mark_dirty(false);
            }
        }

        // Ctrl+C копирует путь выбранного файла, если в строке поиска ничего не выделено (иначе — выделенный текст).
        let selection_in_search = egui::TextEdit::load_state(ctx, search_id)
            .and_then(|s| s.cursor.char_range())
            .is_some_and(|r| r.primary != r.secondary);
        if !selection_in_search && let Some(i) = self.search.selected.filter(|i| *i < self.search.results.len()) {
            let (copy_name, copy_path) = ctx.input_mut(|inp| {
                (
                    inp.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::C),
                    inp.consume_key(Modifiers::COMMAND, Key::C),
                )
            });
            if copy_name {
                actions.push(Action::CopyName(i));
            } else if copy_path {
                actions.push(Action::CopyPath(i));
            }
        }
        // F3 / Shift+F3 — по совпадениям в открытом тексте.
        let (next, prev) =
            ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::F3), i.consume_key(Modifiers::SHIFT, Key::F3)));
        if (next || prev)
            && let Some(preview) = &mut self.preview
        {
            preview.step_match(next);
        }

        // Стрелки, Enter и Esc обрабатываем до того, как поле ввода успеет забрать их себе.
        let (mut delta, mut reveal, mut open, mut escape) = (0isize, false, false, false);
        let (home, end) = ctx
            .input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::Home), i.consume_key(Modifiers::COMMAND, Key::End)));
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                delta += 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                delta -= 1;
            }
            if i.consume_key(Modifiers::NONE, Key::PageDown) {
                delta += 8;
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp) {
                delta -= 8;
            }
            reveal = i.consume_key(Modifiers::COMMAND, Key::Enter) || i.consume_key(Modifiers::SHIFT, Key::Enter);
            open = i.consume_key(Modifiers::NONE, Key::Enter);
            escape = i.consume_key(Modifiers::NONE, Key::Escape);
        });

        if (home || end) && !self.search.results.is_empty() {
            self.search.selected = Some(if home { 0 } else { self.search.results.len() - 1 });
            self.search.scroll_to_selected = true;
            if end {
                self.load_more();
            }
        }
        if delta != 0 && !self.search.results.is_empty() {
            let last = self.search.results.len() as isize - 1;
            let next = match self.search.selected {
                None => 0,
                Some(i) => (i as isize + delta).clamp(0, last),
            };
            self.search.selected = Some(next as usize);
            self.search.scroll_to_selected = true;
            // Дошли до конца загруженного — подгружаем следующую страницу.
            if next == last {
                self.load_more();
            }
        }
        if !self.search.results.is_empty() && !self.pending() {
            let target = self.search.selected.unwrap_or(0);
            if reveal {
                actions.push(Action::Reveal(target));
            } else if open {
                actions.push(Action::Open(target));
            }
        }
        if escape && self.narrow_details {
            self.narrow_details = false;
        } else if escape && !self.search.form.query.is_empty() {
            self.remember_query();
            self.search.form.query.clear();
            self.mark_dirty(false);
        }
        actions
    }

    // ------------------------------------------------------------ снимок экрана (для проверки)

    pub(super) fn drive_screenshot(&mut self, ctx: &Context) {
        let Some(plan) = &mut self.screenshot else { return };
        // Снимок делаем, когда индексация закончилась и выдача обновилась.
        let settled = self.index.backend.as_ref().is_none_or(|b| {
            !b.state.running.load(Ordering::Relaxed)
                && b.state.generation.load(Ordering::SeqCst) == self.search.last_generation
        });
        let ready = plan.started.elapsed() > Duration::from_millis(2500)
            && settled
            && !self.search.searching
            && self.search.dirty_at.is_none();
        if !plan.requested && (ready || plan.started.elapsed() > Duration::from_secs(10)) {
            plan.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let path = plan.path.clone();
            let _ = image::save_buffer(
                &path,
                image.as_raw(),
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}
