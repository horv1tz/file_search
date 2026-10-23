//! Поиск: отправка запроса, подгрузка страниц, связь выбора с предпросмотром.

use super::*;
use crate::backend::SearchRequest;
use egui::Context;
use file_search::query::Mode;
use std::path::PathBuf;
use std::time::Instant;

/// Поиск: запрос, найденные файлы и то, что выбрано в списке.
pub struct SearchState {
    pub form: Form,
    pub next_id: u64,
    pub current_id: u64,
    pub results: Vec<Hit>,
    pub total: usize,
    pub took_ms: f64,
    pub searching: bool,
    pub loading_more: bool,
    pub search_error: Option<String>,
    pub dirty_at: Option<Instant>,
    pub silent_refresh: bool,
    pub selected: Option<usize>,
    pub facets: Option<Vec<usize>>,
    pub scroll_to_selected: bool,
    pub scroll_to_top: bool,
    pub last_generation: u64,
    pub history_pos: Option<usize>,
}

impl App {
    /// Новый запрос набран, но ответа на него ещё нет: список на экране относится к прежнему запросу.
    pub fn pending(&self) -> bool {
        self.search.searching || self.search.dirty_at.is_some()
    }

    /// Запросить поиск после короткой паузы в наборе.
    pub fn mark_dirty(&mut self, silent: bool) {
        self.search.dirty_at = Some(Instant::now());
        self.search.silent_refresh = silent && !self.search.results.is_empty();
    }

    pub(super) fn dispatch_search(&mut self, append: bool) {
        let Some(backend) = &self.index.backend else { return };
        if self.search.form.is_empty() {
            self.search.results.clear();
            self.search.total = 0;
            self.search.searching = false;
            self.search.search_error = None;
            self.search.selected = None;
            self.search.facets = None;
            self.search.current_id = self.search.next_id + 1;
            self.search.next_id += 1;
            return;
        }
        if append {
            if self.search.loading_more {
                return;
            }
            self.search.loading_more = true;
        } else {
            self.search.next_id += 1;
            self.search.current_id = self.search.next_id;
            self.search.loading_more = false;
            self.search.searching = true;
        }
        let offset = if append { self.search.results.len() } else { 0 };
        // Фоновое обновление не должно сворачивать уже подгруженный список обратно до первой страницы.
        let limit =
            if !append && self.search.silent_refresh { self.search.results.len().clamp(PAGE, 500) } else { PAGE };
        backend.search(SearchRequest {
            id: self.search.current_id,
            opts: self.search.form.options(offset, limit),
            append,
        });
    }

    /// Запрос, который привёл к результатам, попадает в историю (запоминается при действиях над файлом и при выходе).
    pub(super) fn remember_query(&mut self) {
        if !self.search.results.is_empty() && !self.pending() {
            self.settings.remember_query(&self.search.form.query);
        }
    }

    /// Какие фильтры сейчас отличаются от «искать везде» (для кнопки «Сбросить фильтры»).
    pub fn filters_active(&self) -> bool {
        self.search.form.groups.iter().any(|g| *g)
            || self.search.form.mode != Mode::All
            || self.search.form.dir.is_some()
    }

    pub(super) fn hit_path(&self, index: usize) -> Option<PathBuf> {
        self.search.results.get(index).map(|h| PathBuf::from(&h.path))
    }

    /// Выбранный результат можно показать как текст (обычный текстовый файл).
    /// У выбранного файла есть предпросмотр (текст, документ, картинка или PDF).
    pub fn selected_has_preview(&self) -> bool {
        self.search
            .selected
            .and_then(|i| self.search.results.get(i))
            .is_some_and(|hit| crate::preview::Kind::from_ext(&hit.ext) != crate::preview::Kind::Other)
    }

    /// Держит просмотр в соответствии с выбранным файлом и текущим запросом.
    pub fn sync_preview(&mut self, ctx: &Context) {
        let wanted = if self.selected_has_preview() {
            self.search.selected.and_then(|i| self.search.results.get(i)).map(|hit| PathBuf::from(&hit.path))
        } else {
            None
        };
        match (wanted, &mut self.preview) {
            (None, _) => self.preview = None,
            (Some(path), Some(p)) if p.path() == path => p.set_query(&self.search.form.query),
            (Some(path), _) => self.preview = Some(crate::preview::Preview::open(ctx, path, &self.search.form.query)),
        }
    }

    /// Подгрузить следующую страницу, когда пользователь докрутил до конца списка.
    pub fn load_more(&mut self) {
        // Пока идёт новый запрос, подгружать к старому списку нечего: ответы перепутались бы.
        if self.search.results.len() < self.search.total && !self.pending() {
            self.dispatch_search(true);
        }
    }
}
