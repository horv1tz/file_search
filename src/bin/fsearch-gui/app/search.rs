//! Поиск: отправка запроса, подгрузка страниц, связь выбора с предпросмотром.

use super::*;
use crate::backend::SearchRequest;
use egui::Context;
use file_search::query::Mode;
use std::path::PathBuf;
use std::time::Instant;

impl App {
    /// Новый запрос набран, но ответа на него ещё нет: список на экране относится к прежнему запросу.
    pub fn pending(&self) -> bool {
        self.searching || self.dirty_at.is_some()
    }

    /// Запросить поиск после короткой паузы в наборе.
    pub fn mark_dirty(&mut self, silent: bool) {
        self.dirty_at = Some(Instant::now());
        self.silent_refresh = silent && !self.results.is_empty();
    }

    pub(super) fn dispatch_search(&mut self, append: bool) {
        let Some(backend) = &self.backend else { return };
        if self.form.is_empty() {
            self.results.clear();
            self.total = 0;
            self.searching = false;
            self.search_error = None;
            self.selected = None;
            self.facets = None;
            self.current_id = self.next_id + 1;
            self.next_id += 1;
            return;
        }
        if append {
            if self.loading_more {
                return;
            }
            self.loading_more = true;
        } else {
            self.next_id += 1;
            self.current_id = self.next_id;
            self.loading_more = false;
            self.searching = true;
        }
        let offset = if append { self.results.len() } else { 0 };
        // Фоновое обновление не должно сворачивать уже подгруженный список обратно до первой страницы.
        let limit = if !append && self.silent_refresh { self.results.len().clamp(PAGE, 500) } else { PAGE };
        backend.search(SearchRequest { id: self.current_id, opts: self.form.options(offset, limit), append });
    }

    /// Запрос, который привёл к результатам, попадает в историю (запоминается при действиях над файлом и при выходе).
    pub(super) fn remember_query(&mut self) {
        if !self.results.is_empty() && !self.pending() {
            self.settings.remember_query(&self.form.query);
        }
    }

    /// Какие фильтры сейчас отличаются от «искать везде» (для кнопки «Сбросить фильтры»).
    pub fn filters_active(&self) -> bool {
        self.form.groups.iter().any(|g| *g) || self.form.mode != Mode::All || self.form.dir.is_some()
    }

    pub(super) fn hit_path(&self, index: usize) -> Option<PathBuf> {
        self.results.get(index).map(|h| PathBuf::from(&h.path))
    }

    /// Выбранный результат можно показать как текст (обычный текстовый файл).
    /// У выбранного файла есть предпросмотр (текст, документ, картинка или PDF).
    pub fn selected_has_preview(&self) -> bool {
        self.selected
            .and_then(|i| self.results.get(i))
            .is_some_and(|hit| crate::preview::Kind::from_ext(&hit.ext) != crate::preview::Kind::Other)
    }

    /// Держит просмотр в соответствии с выбранным файлом и текущим запросом.
    pub fn sync_preview(&mut self, ctx: &Context) {
        let wanted = if self.selected_has_preview() {
            self.selected.and_then(|i| self.results.get(i)).map(|hit| PathBuf::from(&hit.path))
        } else {
            None
        };
        match (wanted, &mut self.preview) {
            (None, _) => self.preview = None,
            (Some(path), Some(p)) if p.path() == path => p.set_query(&self.form.query),
            (Some(path), _) => self.preview = Some(crate::preview::Preview::open(ctx, path, &self.form.query)),
        }
    }

    /// Подгрузить следующую страницу, когда пользователь докрутил до конца списка.
    pub fn load_more(&mut self) {
        // Пока идёт новый запрос, подгружать к старому списку нечего: ответы перепутались бы.
        if self.results.len() < self.total && !self.pending() {
            self.dispatch_search(true);
        }
    }
}
