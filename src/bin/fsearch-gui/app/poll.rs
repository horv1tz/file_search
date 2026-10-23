//! Приём ответов фоновых потоков (поиск, индексация, обновления) и всплывающие сообщения.

use super::*;
use crate::backend::Reply;
use crate::theme::Kind;
use crate::updater::{self};
use egui::Context;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

impl App {
    pub(super) fn poll(&mut self, ctx: &Context) {
        while let Ok(place) = self.places_rx.try_recv() {
            if !self.places.iter().any(|(_, p)| *p == place.1) {
                self.places.push(place);
                // Сначала папки пользователя (их индексировать быстро и полезно), затем целые диски.
                self.places.sort_by_key(|(name, _)| (name.starts_with("Диск "), name.clone()));
            }
        }
        self.track_indexing();
        while let Ok((kind, text)) = self.toast_rx.try_recv() {
            self.toast = Some(Toast { text, kind, since: Instant::now() });
        }
        while let Ok(done) = self.dialog_rx.try_recv() {
            match done {
                DialogResult::Root(path) => self.handle_action(ctx, Action::AddRoot(path)),
                DialogResult::FilterFolder(path) => {
                    self.search.form.dir = Some(path.to_string_lossy().into_owned());
                    self.mark_dirty(false);
                }
                DialogResult::Closed => {
                    self.dialog_open = false;
                    self.focus_search = true;
                }
                DialogResult::NetRoot(path) => {
                    self.net_checking = false;
                    self.net_error = None;
                    self.net_input.clear();
                    self.handle_action(ctx, Action::AddRoot(path));
                }
                DialogResult::NetFailed(text) => {
                    self.net_checking = false;
                    self.net_error = Some(text);
                }
            }
        }
        for event in self.updater.poll(self.settings.skipped_version.as_deref()) {
            match event {
                updater::Event::Checked => {
                    self.settings.last_update_check = unix_now();
                    self.save_settings();
                }
            }
        }
        // Папки, записи которых уже удалены из индекса, больше не нужно помнить.
        let removed_done: Vec<PathBuf> = self
            .index
            .backend
            .as_ref()
            .map(|b| std::mem::take(&mut *b.state.removed_done.lock().unwrap()))
            .unwrap_or_default();
        if !removed_done.is_empty() {
            self.settings.pending_removals.retain(|r| !removed_done.contains(r));
            self.save_settings();
        }
        let Some(backend) = &self.index.backend else { return };
        while let Ok(reply) = backend.replies.try_recv() {
            match reply {
                Reply::Count(n) => self.index.num_docs = n,
                Reply::Facets { id, counts } => {
                    if id == self.search.current_id {
                        self.search.facets = Some(counts);
                    }
                }
                Reply::Search(resp) => {
                    if resp.id != self.search.current_id {
                        continue;
                    }
                    if resp.append {
                        self.search.loading_more = false;
                    } else {
                        self.search.searching = false;
                    }
                    match resp.result {
                        Ok(result) => {
                            self.search.search_error = None;
                            self.search.total = result.total;
                            self.search.took_ms = result.took.as_secs_f64() * 1000.0;
                            if resp.append {
                                self.search.results.extend(result.hits);
                            } else {
                                self.search.results = result.hits;
                                if self.search.silent_refresh {
                                    self.search.selected =
                                        self.search.selected.filter(|i| *i < self.search.results.len());
                                } else {
                                    // Первый результат выбран сразу: Enter открывает его, справа видны подробности.
                                    self.search.selected = if self.search.results.is_empty() { None } else { Some(0) };
                                    self.search.scroll_to_top = true;
                                    self.narrow_details = false;
                                }
                                self.search.silent_refresh = false;
                            }
                        }
                        Err(message) => {
                            self.search.search_error = Some(message);
                            self.search.results.clear();
                            self.search.total = 0;
                        }
                    }
                }
            }
        }
        // Индекс обновился (закончилась индексация, пришли изменения с диска) — обновим выдачу.
        let generation = backend.state.generation.load(Ordering::SeqCst);
        if generation != self.search.last_generation {
            self.search.last_generation = generation;
            backend.request_count();
            if !self.search.form.is_empty() {
                self.mark_dirty(true);
            }
        }
    }

    // ------------------------------------------------------------ действия

    pub(super) fn show_toast(&mut self, kind: Kind, text: impl Into<String>) {
        self.toast = Some(Toast { text: text.into(), kind, since: Instant::now() });
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.show_toast(Kind::Info, text);
    }

    pub fn toast_ok(&mut self, text: impl Into<String>) {
        self.show_toast(Kind::Success, text);
    }

    pub fn toast_err(&mut self, text: impl Into<String>) {
        self.show_toast(Kind::Error, text);
    }
}
