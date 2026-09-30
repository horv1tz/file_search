//! Индексация: обход папок, инкрементальное обновление, параллельное извлечение текста.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use tantivy::indexer::IndexWriterOptions;
use tantivy::schema::Term;
use tantivy::{Index, IndexWriter, ReloadPolicy, TantivyDocument};
use walkdir::WalkDir;

use crate::extract::{EXTRACTOR_VERSION, Extracted, Limits, Status, UNIT_SEP, extract_file};
use crate::schema::{Fields, path_key};

/// Папки, которые почти никогда не нужны в поиске.
const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "__pycache__",
];
const DEFAULT_EXCLUDED_FILES: &[&str] = &["thumbs.db", "desktop.ini", ".ds_store"];

/// Сколько текста накапливаем в памяти между коммитами.
const COMMIT_TEXT_BUDGET: usize = 128 << 20;
const COMMIT_DOC_BUDGET: usize = 50_000;
const MAX_REPORTED_FAILURES: usize = 1000;

pub struct Options {
    pub roots: Vec<PathBuf>,
    /// Glob-шаблоны исключений; применяются к полному пути и к имени файла/папки.
    pub excludes: Vec<String>,
    pub default_excludes: bool,
    pub limits: Limits,
    pub threads: usize,
    /// Переиндексировать всё, даже неизменённые файлы.
    pub force: bool,
}

impl Options {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Options {
            roots,
            excludes: Vec::new(),
            default_excludes: true,
            limits: Limits::default(),
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
            force: false,
        }
    }
}

/// Обратная связь для интерфейса (индикатор прогресса).
pub trait Progress: Sync {
    fn scanning(&self, _files_seen: u64) {}
    fn indexing_started(&self, _total: u64) {}
    fn file_done(&self, _path: &Path, _status: &Status) {}
    fn message(&self, _text: &str) {}
}

pub struct Silent;
impl Progress for Silent {}

#[derive(Debug, Default, Clone)]
pub struct Summary {
    /// Файлов найдено при обходе.
    pub scanned: u64,
    pub unchanged: u64,
    /// Добавлено или обновлено в индексе.
    pub indexed: u64,
    pub removed: u64,
    pub encrypted: u64,
    pub skipped: u64,
    pub empty: u64,
    pub truncated: u64,
    pub failed: Vec<(PathBuf, String)>,
    pub failed_total: u64,
    /// Ошибки обхода (нет доступа к папке и т. п.).
    pub walk_errors: Vec<String>,
    pub walk_errors_total: u64,
    pub text_bytes: u64,
    pub cancelled: bool,
    pub elapsed: Duration,
}

struct Candidate {
    path: PathBuf,
    key: String,
    size: u64,
    mtime: u64,
}

struct Existing {
    mtime: u64,
    size: u64,
    ver: u64,
}

/// Приводит корень к абсолютному виду: `D:` → `D:\`, без `..`, без завершающего разделителя.
pub fn normalize_root(root: &Path) -> Result<PathBuf> {
    let s = root.to_string_lossy();
    let fixed: PathBuf = if cfg!(windows) && s.len() == 2 && s.ends_with(':') {
        PathBuf::from(format!("{s}\\"))
    } else {
        root.to_path_buf()
    };
    let abs = std::path::absolute(&fixed).with_context(|| format!("некорректный путь {}", root.display()))?;
    if !abs.is_dir() {
        bail!("{} не существует или не является папкой", abs.display());
    }
    Ok(abs)
}

fn root_prefix(root: &Path) -> String {
    let mut key = path_key(&root.to_string_lossy());
    if !key.ends_with('/') {
        key.push('/');
    }
    key
}

fn build_globs(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(&p.replace('\\', "/")).with_context(|| format!("некорректный шаблон исключения: {p}"))?);
    }
    Ok(b.build()?)
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn load_existing(index: &Index, fields: &Fields, prefixes: &[String]) -> Result<HashMap<String, Existing>> {
    let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
    let searcher = reader.searcher();
    let key_name = index.schema().get_field_name(fields.key).to_string();
    let mut map = HashMap::new();
    for seg in searcher.segment_readers() {
        let ff = seg.fast_fields();
        let Some(keys) = ff.str(&key_name)? else { continue };
        let mtime = ff.u64("mtime")?;
        let size = ff.u64("size")?;
        let ver = ff.u64("ver")?;
        let mut buf = String::new();
        for doc in 0..seg.max_doc() {
            if seg.is_deleted(doc) {
                continue;
            }
            let Some(ord) = keys.term_ords(doc).next() else { continue };
            buf.clear();
            keys.ord_to_str(ord, &mut buf)?;
            if !prefixes.iter().any(|p| buf.starts_with(p.as_str())) {
                continue;
            }
            map.insert(
                buf.clone(),
                Existing {
                    mtime: mtime.first(doc).unwrap_or(0),
                    size: size.first(doc).unwrap_or(0),
                    ver: ver.first(doc).unwrap_or(0),
                },
            );
        }
    }
    Ok(map)
}

fn build_doc(f: &Fields, c: &Candidate, ex: &Extracted) -> TantivyDocument {
    let mut doc = TantivyDocument::default();
    let path_str = c.path.to_string_lossy();
    let file_name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), e.to_lowercase()),
        _ => (file_name.clone(), String::new()),
    };
    doc.add_text(f.path, &path_str);
    doc.add_text(f.key, &c.key);
    doc.add_text(f.name, &stem);
    doc.add_text(f.name_lc, file_name.to_lowercase());
    if !ext.is_empty() {
        doc.add_text(f.ext, &ext);
    }
    if let Some(parent) = c.path.parent() {
        doc.add_text(f.folder, parent.to_string_lossy());
    }
    if !ex.meta.is_empty() {
        doc.add_text(f.meta, &ex.meta);
    }
    if !ex.text.is_empty() {
        doc.add_text(f.content, &ex.text);
    }
    if !ex.units.is_empty() {
        let labels: Vec<String> = ex.units.iter().map(|u| u.replace(['\n', UNIT_SEP], " ")).collect();
        doc.add_text(f.units, labels.join("\n"));
    }
    doc.add_u64(f.size, c.size);
    doc.add_u64(f.mtime, c.mtime);
    doc.add_text(f.status, ex.status.as_string().split(':').next().unwrap_or("ok"));
    doc.add_u64(f.ver, EXTRACTOR_VERSION);
    doc
}

struct Excluder {
    globs: GlobSet,
    defaults: bool,
}

impl Excluder {
    fn excluded(&self, path: &Path, is_dir: bool) -> bool {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if self.defaults {
            if is_dir && DEFAULT_EXCLUDED_DIRS.contains(&name.as_str()) {
                return true;
            }
            if !is_dir
                && (DEFAULT_EXCLUDED_FILES.contains(&name.as_str())
                    // блокировки Word/Excel/PowerPoint и LibreOffice
                    || name.starts_with("~$")
                    || name.starts_with(".~lock."))
            {
                return true;
            }
        }
        if self.globs.is_empty() {
            return false;
        }
        let full = path.to_string_lossy().replace('\\', "/");
        self.globs.is_match(&full) || self.globs.is_match(name.as_str())
    }
}

pub fn run(
    index: &Index,
    fields: &Fields,
    opts: &Options,
    progress: &dyn Progress,
    cancel: &AtomicBool,
) -> Result<Summary> {
    let started = Instant::now();
    let mut summary = Summary::default();

    let roots: Vec<PathBuf> = opts.roots.iter().map(|r| normalize_root(r)).collect::<Result<_>>()?;
    let excluder = Excluder { globs: build_globs(&opts.excludes)?, defaults: opts.default_excludes };
    let prefixes: Vec<String> = roots.iter().map(|r| root_prefix(r)).collect();

    progress.message("Чтение текущего индекса…");
    let mut existing = load_existing(index, fields, &prefixes)?;

    // ---- обход
    let mut todo: Vec<Candidate> = Vec::new();
    'roots: for root in &roots {
        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !excluder.excluded(e.path(), e.file_type().is_dir()));
        for entry in walker {
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break 'roots;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    summary.walk_errors_total += 1;
                    if summary.walk_errors.len() < 200 {
                        summary.walk_errors.push(err.to_string());
                    }
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(err) => {
                    summary.walk_errors_total += 1;
                    if summary.walk_errors.len() < 200 {
                        summary.walk_errors.push(format!("{}: {err}", entry.path().display()));
                    }
                    continue;
                }
            };
            summary.scanned += 1;
            if summary.scanned % 1000 == 0 {
                progress.scanning(summary.scanned);
            }
            let path = entry.into_path();
            let key = path_key(&path.to_string_lossy());
            let (size, mtime) = (meta.len(), mtime_ms(&meta));
            match existing.remove(&key) {
                Some(e) if !opts.force && e.mtime == mtime && e.size == size && e.ver == EXTRACTOR_VERSION => {
                    summary.unchanged += 1;
                }
                _ => todo.push(Candidate { path, key, size, mtime }),
            }
        }
    }
    progress.scanning(summary.scanned);

    // Всё, что осталось в `existing`, из индекса пропало с диска. При прерванном обходе судить нельзя.
    let removed: Vec<String> = if summary.cancelled { Vec::new() } else { existing.into_keys().collect() };
    summary.removed = removed.len() as u64;

    if todo.is_empty() && removed.is_empty() {
        summary.elapsed = started.elapsed();
        return Ok(summary);
    }

    // ---- индексация
    let threads = opts.threads.max(1);
    let writer_opts = IndexWriterOptions::builder()
        .num_worker_threads(threads.clamp(1, 4))
        .memory_budget_per_thread(96 << 20)
        .build();
    let writer: RwLock<IndexWriter> = RwLock::new(index.writer_with_options(writer_opts)?);

    {
        let w = writer.read().unwrap();
        for key in &removed {
            w.delete_term(Term::from_field_text(fields.key, key));
        }
    }

    progress.indexing_started(todo.len() as u64);
    let pending_bytes = AtomicUsize::new(0);
    let pending_docs = AtomicUsize::new(0);
    let indexed = AtomicU64::new(0);
    let counters = Counters::default();
    let failures = std::sync::Mutex::new(Vec::<(PathBuf, String)>::new());
    let commit_error = std::sync::Mutex::new(None::<anyhow::Error>);

    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    pool.install(|| {
        todo.par_iter().for_each(|c| {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let ex = extract_file(&c.path, c.size, &opts.limits);
            let text_len = ex.text.len() + ex.meta.len();
            counters.record(&ex);
            let doc = build_doc(fields, c, &ex);
            {
                let w = writer.read().unwrap();
                w.delete_term(Term::from_field_text(fields.key, &c.key));
                if let Err(e) = w.add_document(doc) {
                    let mut f = failures.lock().unwrap();
                    f.push((c.path.clone(), format!("индекс: {e}")));
                    return;
                }
            }
            indexed.fetch_add(1, Ordering::Relaxed);
            if let Status::Failed(msg) = &ex.status {
                counters.failed_total.fetch_add(1, Ordering::Relaxed);
                let mut f = failures.lock().unwrap();
                if f.len() < MAX_REPORTED_FAILURES {
                    f.push((c.path.clone(), msg.clone()));
                }
            }
            progress.file_done(&c.path, &ex.status);

            let bytes = pending_bytes.fetch_add(text_len + 512, Ordering::Relaxed) + text_len + 512;
            let docs = pending_docs.fetch_add(1, Ordering::Relaxed) + 1;
            if bytes > COMMIT_TEXT_BUDGET || docs > COMMIT_DOC_BUDGET {
                // Коммит нужен, чтобы очередь tantivy не накопила гигабайты текста.
                let mut w = writer.write().unwrap();
                if pending_bytes.load(Ordering::Relaxed) > 0 {
                    if let Err(e) = w.commit() {
                        *commit_error.lock().unwrap() = Some(e.into());
                    }
                    pending_bytes.store(0, Ordering::Relaxed);
                    pending_docs.store(0, Ordering::Relaxed);
                }
            }
        });
    });

    let mut writer = writer.into_inner().unwrap();
    writer.commit().context("не удалось сохранить индекс")?;
    if let Some(e) = commit_error.into_inner().unwrap() {
        return Err(e.context("не удалось сохранить индекс"));
    }
    if !cancel.load(Ordering::Relaxed) {
        progress.message("Оптимизация индекса…");
        writer.wait_merging_threads().context("ошибка при слиянии сегментов индекса")?;
    }

    summary.indexed = indexed.load(Ordering::Relaxed);
    summary.encrypted = counters.encrypted.load(Ordering::Relaxed);
    summary.skipped = counters.skipped.load(Ordering::Relaxed);
    summary.empty = counters.empty.load(Ordering::Relaxed);
    summary.truncated = counters.truncated.load(Ordering::Relaxed);
    summary.text_bytes = counters.text_bytes.load(Ordering::Relaxed);
    summary.failed_total = counters.failed_total.load(Ordering::Relaxed);
    summary.failed = failures.into_inner().unwrap();
    summary.cancelled |= cancel.load(Ordering::Relaxed);
    summary.elapsed = started.elapsed();
    Ok(summary)
}

#[derive(Default)]
struct Counters {
    encrypted: AtomicU64,
    skipped: AtomicU64,
    empty: AtomicU64,
    truncated: AtomicU64,
    text_bytes: AtomicU64,
    failed_total: AtomicU64,
}

impl Counters {
    fn record(&self, ex: &Extracted) {
        match &ex.status {
            Status::Encrypted => self.encrypted.fetch_add(1, Ordering::Relaxed),
            Status::Skipped(_) => self.skipped.fetch_add(1, Ordering::Relaxed),
            Status::Empty => self.empty.fetch_add(1, Ordering::Relaxed),
            Status::Ok | Status::Failed(_) => 0,
        };
        if ex.truncated {
            self.truncated.fetch_add(1, Ordering::Relaxed);
        }
        self.text_bytes.fetch_add(ex.text.len() as u64, Ordering::Relaxed);
    }
}
