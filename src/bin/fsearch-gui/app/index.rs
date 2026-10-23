//! Индексация и папки: запуск, применение настроек, пересоздание индекса, отслеживание прогресса.

use super::*;
use crate::backend::Backend;
use crate::theme::{self};
use egui::Context;
use file_search::platform;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// Каждый диск проверяется в своём потоке: отключённый сетевой диск отвечает секундами и не должен
/// задерживать остальные кнопки.
pub(super) fn discover_places(tx: Sender<(String, PathBuf)>, ctx: Context) {
    let send = move |tx: &Sender<(String, PathBuf)>, ctx: &Context, item: (String, PathBuf)| {
        let _ = tx.send(item);
        ctx.request_repaint();
    };
    if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let root = PathBuf::from(format!("{}:\\", letter as char));
                if root.exists() {
                    send(&tx, &ctx, (format!("Диск {}:", letter as char), root));
                }
            });
        }
    } else {
        send(&tx, &ctx, ("Корень /".to_string(), PathBuf::from("/")));
    }
    std::thread::spawn(move || {
        for (name, dir) in [
            ("Документы", dirs::document_dir()),
            ("Рабочий стол", dirs::desktop_dir()),
            ("Загрузки", dirs::download_dir()),
            ("Домашняя папка", dirs::home_dir()),
        ] {
            if let Some(dir) = dir.filter(|d| d.is_dir()) {
                send(&tx, &ctx, (name.to_string(), dir));
            }
        }
    });
}

/// Индекс: подключённый бэкенд и сведения о ходе индексации.
pub struct IndexStatus {
    pub backend: Option<Backend>,
    pub backend_error: Option<String>,
    pub num_docs: u64,
    pub index_was_running: bool,
    pub index_mark: Option<(Instant, u64)>,
    pub index_eta_secs: Option<u64>,
}

impl App {
    pub(super) fn start_indexing(&mut self, force: bool) {
        if let Some(backend) = &mut self.index.backend {
            backend.start_indexing(&self.settings, force);
        }
    }

    pub(super) fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.settings_path) {
            self.toast_err(format!("Не удалось сохранить настройки: {e}"));
        }
    }

    /// Применяет настройки: сохраняет и перезапускает индексацию.
    pub(super) fn apply_settings(&mut self, ctx: &Context, force: bool) {
        self.settings.excludes = self.parsed_excludes();
        self.baseline = self.settings.index_params(self.settings.excludes.clone());
        theme::apply(ctx, self.settings.theme);
        self.applied_theme = self.settings.theme;
        platform::set_low_priority(self.settings.load.background());
        self.save_settings();
        self.start_indexing(force);
        self.mark_dirty(true);
    }

    pub(super) fn parsed_excludes(&self) -> Vec<String> {
        self.excludes_text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
    }

    /// Есть изменения, которые вступят в силу только после «Применить» (перезапуск индексации).
    pub fn pending_changes(&self) -> bool {
        self.settings.index_params(self.parsed_excludes()) != self.baseline
    }

    /// Возвращает параметры индексации к тем, что сейчас применены.
    pub fn revert_pending(&mut self) {
        self.settings.set_index_params(&self.baseline);
        self.excludes_text = self.baseline.excludes.join("\n");
    }

    /// Закрывает окно настроек; несохранённые изменения применяются, чтобы ничего не потерялось молча.
    pub(super) fn close_settings(&mut self, ctx: &Context) {
        self.show_settings = false;
        self.confirm_reset = false;
        self.confirm_remove = None;
        self.focus_search = true;
        if self.pending_changes() {
            self.apply_settings(ctx, false);
            self.toast_ok("Настройки применены, индекс обновляется");
        }
    }

    pub(super) fn recreate_index(&mut self, ctx: &Context) {
        // Удаляем только каталог, который действительно является индексом программы: путь берётся из настроек,
        // и ошибка в нём не должна стоить пользователю случайной папки.
        let is_index = self.index_dir.join("fsearch.version").exists() || self.index_dir.join("meta.json").exists();
        if !is_index {
            self.toast_err(format!("{} не похож на индекс программы — удалять не буду", self.index_dir.display()));
            return;
        }
        if let Some(backend) = &mut self.index.backend
            && !backend.shutdown()
        {
            self.toast_err("Индексация ещё не остановилась — подождите несколько секунд и повторите");
            return;
        }
        self.index.backend = None;
        if let Err(e) = std::fs::remove_dir_all(&self.index_dir)
            && self.index_dir.exists()
        {
            self.index.backend_error = Some(format!("Не удалось удалить индекс: {e}"));
            return;
        }
        match Backend::new(ctx.clone(), self.index_dir.clone()) {
            Ok(b) => {
                self.index.backend = Some(b);
                self.index.backend_error = None;
                self.search.results.clear();
                self.search.total = 0;
                self.index.num_docs = 0;
                self.search.last_generation = 0;
                self.start_indexing(true);
                self.toast_ok("Индекс пересоздан");
            }
            Err(e) => self.index.backend_error = Some(format!("{e:#}")),
        }
    }

    // ------------------------------------------------------------ поиск

    /// Идёт проход по файлам (первичная индексация, обновление или полная проверка).
    pub fn indexing(&self) -> bool {
        self.index.backend.as_ref().is_some_and(|b| b.state.running.load(Ordering::Relaxed))
    }

    /// Следит за ходом индексации: оценивает оставшееся время и сообщает об окончании прохода.
    pub(super) fn track_indexing(&mut self) {
        let Some(backend) = &self.index.backend else { return };
        let st = &backend.state;
        let running = st.running.load(Ordering::Relaxed);
        if running {
            let (done, total) = (st.done.load(Ordering::Relaxed), st.total.load(Ordering::Relaxed));
            match self.index.index_mark {
                Some((at, start)) if total > 0 && done > start && at.elapsed() >= Duration::from_secs(5) => {
                    let rate = (done - start) as f64 / at.elapsed().as_secs_f64();
                    self.index.index_eta_secs =
                        Some(((total.saturating_sub(done)) as f64 / rate.max(1e-6)).ceil() as u64);
                }
                Some(_) => {}
                None if total > 0 => self.index.index_mark = Some((Instant::now(), done)),
                None => {}
            }
        } else {
            self.index.index_mark = None;
            self.index.index_eta_secs = None;
        }
        if self.index.index_was_running && !running {
            let summary = st.summary.lock().unwrap().clone();
            if let Some(s) = summary
                && !s.cancelled
                && s.indexed + s.removed > 0
            {
                let mut text = format!("Индексация завершена: обновлено файлов — {}", s.indexed);
                if s.failed_total > 0 {
                    text.push_str(&format!(", не удалось прочитать — {} (см. настройки)", s.failed_total));
                }
                self.toast_ok(text);
            }
        }
        self.index.index_was_running = running;
    }
}
