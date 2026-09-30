//! Фоновая работа приложения: поиск и индексация идут в отдельных потоках,
//! окно только показывает их состояние и никогда не ждёт диск.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;
use file_search::extract::{Limits, Status};
use file_search::indexer::{self, Options, Progress, Summary};
use file_search::schema::open_or_create_index;
use file_search::search::{SearchOptions, SearchResult, Searcher};
use file_search::watcher::{self, WatchOptions};

use crate::settings::Settings;

const LOG_LIMIT: usize = 200;

// ------------------------------------------------------------------ состояние индексации

/// Состояние индексации, общее для потока-исполнителя и окна.
#[derive(Default)]
pub struct IndexState {
    /// Идёт проход по файлам (первичная индексация, дообновление, полная проверка).
    pub running: AtomicBool,
    /// Поток индексации жив (проход или слежение).
    pub alive: AtomicBool,
    /// Включено слежение за изменениями.
    pub watching: AtomicBool,
    pub scanned: AtomicU64,
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// Растёт каждый раз, когда содержимое индекса могло измениться.
    pub generation: AtomicU64,
    pub phase: Mutex<String>,
    pub current: Mutex<String>,
    pub summary: Mutex<Option<Summary>>,
    pub log: Mutex<VecDeque<String>>,
    pub error: Mutex<Option<String>>,
}

impl IndexState {
    pub fn push_log(&self, line: impl Into<String>) {
        let mut log = self.log.lock().unwrap();
        log.push_back(line.into());
        while log.len() > LOG_LIMIT {
            log.pop_front();
        }
    }

    pub fn phase(&self) -> String {
        self.phase.lock().unwrap().clone()
    }
}

struct GuiProgress {
    state: Arc<IndexState>,
    ctx: egui::Context,
}

impl Progress for GuiProgress {
    fn started(&self) {
        self.state.running.store(true, Ordering::SeqCst);
        self.state.scanned.store(0, Ordering::Relaxed);
        self.state.done.store(0, Ordering::Relaxed);
        self.state.total.store(0, Ordering::Relaxed);
        *self.state.phase.lock().unwrap() = "Обход папок…".into();
        self.ctx.request_repaint();
    }

    fn finished(&self, summary: &Summary) {
        self.state.running.store(false, Ordering::SeqCst);
        *self.state.summary.lock().unwrap() = Some(summary.clone());
        self.state.current.lock().unwrap().clear();
        self.state.phase.lock().unwrap().clear();
        self.state.generation.fetch_add(1, Ordering::SeqCst);
        self.ctx.request_repaint();
    }

    fn scanning(&self, files_seen: u64) {
        self.state.scanned.store(files_seen, Ordering::Relaxed);
    }

    fn indexing_started(&self, total: u64) {
        self.state.total.store(total, Ordering::Relaxed);
        self.state.done.store(0, Ordering::Relaxed);
        *self.state.phase.lock().unwrap() = "Индексация".into();
    }

    fn file_done(&self, path: &Path, _status: &Status) {
        self.state.done.fetch_add(1, Ordering::Relaxed);
        // Имя текущего файла — только для красоты: при конкуренции потоков пропускаем обновление.
        if let Ok(mut current) = self.state.current.try_lock() {
            *current = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
    }

    fn message(&self, text: &str) {
        *self.state.phase.lock().unwrap() = text.to_string();
    }
}

// ------------------------------------------------------------------ поиск

pub struct SearchRequest {
    /// Номер запроса: ответы на устаревшие запросы отбрасываются.
    pub id: u64,
    pub opts: SearchOptions,
    /// Догрузка следующей страницы к уже показанным результатам.
    pub append: bool,
}

pub struct SearchResponse {
    pub id: u64,
    pub append: bool,
    pub result: Result<SearchResult, String>,
}

enum Msg {
    Search(SearchRequest),
    Count,
}

pub enum Reply {
    Search(SearchResponse),
    Count(u64),
}

fn search_worker(dir: PathBuf, rx: Receiver<Msg>, tx: Sender<Reply>, ctx: egui::Context) {
    let mut searcher: Option<Searcher> = None;
    let mut open_error = String::new();
    while let Ok(first) = rx.recv() {
        // Пока набирают запрос, копятся устаревшие: выполняем только последний поиск.
        let mut latest: Option<SearchRequest> = None;
        let mut want_count = false;
        let mut take = |msg: Msg| match msg {
            Msg::Search(req) => latest = Some(req),
            Msg::Count => want_count = true,
        };
        take(first);
        while let Ok(next) = rx.try_recv() {
            take(next);
        }
        if searcher.is_none() {
            match Searcher::open(&dir, true) {
                Ok(s) => searcher = Some(s),
                Err(e) => open_error = format!("{e:#}"),
            }
        }
        if want_count {
            let _ = tx.send(Reply::Count(searcher.as_ref().map_or(0, Searcher::num_docs)));
        }
        if let Some(req) = latest {
            let result = match &searcher {
                Some(s) => s.search(&req.opts).map_err(|e| format!("{e:#}")),
                None => Err(format!("Индекс недоступен: {open_error}")),
            };
            let _ = tx.send(Reply::Search(SearchResponse { id: req.id, append: req.append, result }));
        }
        ctx.request_repaint();
    }
}

// ------------------------------------------------------------------ движок

pub struct Backend {
    pub state: Arc<IndexState>,
    pub replies: Receiver<Reply>,
    search_tx: Sender<Msg>,
    ctx: egui::Context,
    index_dir: PathBuf,
    cancel: Arc<AtomicBool>,
    indexer: Option<JoinHandle<()>>,
}

fn scan_options(settings: &Settings, roots: Vec<PathBuf>) -> Options {
    let mut opts = Options::new(roots);
    opts.excludes = settings.excludes.clone();
    opts.default_excludes = settings.default_excludes;
    opts.limits = Limits { max_text_bytes: settings.max_text_mb.max(1) << 20, ..Limits::default() };
    // Одно ядро оставляем окну и остальным программам; найденное сохраняем каждые несколько секунд.
    opts.threads = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).max(1));
    opts.commit_every = Some(Duration::from_secs(5));
    opts
}

impl Backend {
    /// Открывает (при необходимости создаёт) индекс и запускает поток поиска.
    pub fn new(ctx: egui::Context, index_dir: PathBuf) -> Result<Backend> {
        open_or_create_index(&index_dir)?;
        let (search_tx, search_rx) = channel();
        let (reply_tx, replies) = channel();
        {
            let (dir, ctx) = (index_dir.clone(), ctx.clone());
            thread::Builder::new().name("поиск".into()).spawn(move || search_worker(dir, search_rx, reply_tx, ctx))?;
        }
        let backend = Backend {
            state: Arc::new(IndexState::default()),
            replies,
            search_tx,
            ctx,
            index_dir,
            cancel: Arc::new(AtomicBool::new(false)),
            indexer: None,
        };
        backend.request_count();
        Ok(backend)
    }

    pub fn search(&self, req: SearchRequest) {
        let _ = self.search_tx.send(Msg::Search(req));
    }

    pub fn request_count(&self) {
        let _ = self.search_tx.send(Msg::Count);
    }

    /// Запускает индексацию (и слежение, если оно включено). Прежняя индексация останавливается.
    /// `removed` — папки, которые убрали из списка: их записи удаляются из индекса.
    pub fn start_indexing(&mut self, settings: &Settings, removed: Vec<PathBuf>, force: bool) {
        self.cancel.store(true, Ordering::SeqCst);
        let previous = self.indexer.take();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();

        let state = self.state.clone();
        let ctx = self.ctx.clone();
        let dir = self.index_dir.clone();
        let settings = settings.clone();
        state.alive.store(true, Ordering::SeqCst);

        let spawned = thread::Builder::new().name("индексация".into()).spawn(move || {
            // Индекс открывает один писатель за раз: дожидаемся остановки прежнего потока.
            if let Some(h) = previous {
                let _ = h.join();
            }
            *state.error.lock().unwrap() = None;
            let result = run_indexing(&dir, &settings, removed, force, &state, &ctx, &cancel);
            state.watching.store(false, Ordering::SeqCst);
            state.running.store(false, Ordering::SeqCst);
            state.alive.store(false, Ordering::SeqCst);
            state.generation.fetch_add(1, Ordering::SeqCst);
            if let Err(e) = result {
                *state.error.lock().unwrap() = Some(format!("{e:#}"));
            }
            ctx.request_repaint();
        });
        match spawned {
            Ok(handle) => self.indexer = Some(handle),
            Err(e) => {
                self.state.alive.store(false, Ordering::SeqCst);
                *self.state.error.lock().unwrap() = Some(format!("не удалось запустить индексацию: {e}"));
            }
        }
    }

    /// Останавливает индексацию и слежение (сделанное сохраняется).
    pub fn stop_indexing(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Останавливает фоновую работу и ждёт её недолго, чтобы индекс закрылся аккуратно.
    pub fn shutdown(&mut self) {
        self.stop_indexing();
        for _ in 0..40 {
            if !self.state.alive.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn run_indexing(
    dir: &Path,
    settings: &Settings,
    removed: Vec<PathBuf>,
    force: bool,
    state: &Arc<IndexState>,
    ctx: &egui::Context,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let (index, fields) = open_or_create_index(dir)?;
    for root in &removed {
        indexer::remove_root(&index, &fields, root)?;
        state.generation.fetch_add(1, Ordering::SeqCst);
    }

    // Недоступные папки (например, отключённая флешка) пропускаем, не трогая их записи в индексе.
    let (available, missing): (Vec<PathBuf>, Vec<PathBuf>) = settings.roots.iter().cloned().partition(|r| r.is_dir());
    if !missing.is_empty() {
        let list: Vec<String> = missing.iter().map(|p| p.display().to_string()).collect();
        state.push_log(format!("Недоступны, пропущены: {}", list.join(", ")));
        *state.error.lock().unwrap() = Some(format!("Недоступны и пропущены: {}", list.join(", ")));
    }
    if available.is_empty() {
        return Ok(());
    }

    let mut opts = scan_options(settings, available);
    opts.force = force;
    let progress = GuiProgress { state: state.clone(), ctx: ctx.clone() };

    if settings.watch {
        let mut watch = WatchOptions::new(opts);
        watch.rescan_every = Duration::from_secs(settings.rescan_minutes.max(1) * 60);
        state.watching.store(true, Ordering::SeqCst);
        let log = |line: &str| {
            state.push_log(line);
            state.generation.fetch_add(1, Ordering::SeqCst);
            ctx.request_repaint();
        };
        watcher::watch(&index, &fields, &watch, &progress, &log, cancel)
    } else {
        indexer::run(&index, &fields, &opts, &progress, cancel).map(|_| ())
    }
}
