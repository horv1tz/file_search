//! Индексация: обход папок, инкрементальное обновление, параллельное извлечение текста.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use tantivy::indexer::IndexWriterOptions;
use tantivy::schema::Term;
use tantivy::{Index, IndexWriter, ReloadPolicy, TantivyDocument};
use walkdir::WalkDir;

use crate::extract::{EXTRACTOR_VERSION, Extracted, Limits, Status, UNIT_SEP, extract_file};
use crate::schema::{Fields, path_key};

/// Папки, которые почти никогда не нужны в поиске.
const DEFAULT_EXCLUDED_DIRS: &[&str] =
    &["$recycle.bin", "system volume information", ".git", ".svn", ".hg", "node_modules", "__pycache__"];
const DEFAULT_EXCLUDED_FILES: &[&str] = &["thumbs.db", "desktop.ini", ".ds_store"];

/// Сколько текста накапливаем в памяти между коммитами.
const COMMIT_TEXT_BUDGET: usize = 128 << 20;
const COMMIT_DOC_BUDGET: usize = 50_000;
const MAX_REPORTED_FAILURES: usize = 1000;

#[derive(Clone)]
pub struct Options {
    pub roots: Vec<PathBuf>,
    /// Glob-шаблоны исключений; применяются к полному пути и к имени файла/папки.
    pub excludes: Vec<String>,
    pub default_excludes: bool,
    pub limits: Limits,
    pub threads: usize,
    /// Переиндексировать всё, даже неизменённые файлы.
    pub force: bool,
    /// Сохранять индекс не реже чем раз в этот срок: так найденное появляется в поиске ещё во время индексации.
    pub commit_every: Option<Duration>,
    /// Папки, которые нельзя индексировать ни при каких условиях (прежде всего — каталог самого индекса:
    /// иначе его файлы попадали бы в индекс и порождали бы бесконечный цикл обновлений).
    pub skip_dirs: Vec<PathBuf>,
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
            commit_every: None,
            skip_dirs: Vec::new(),
        }
    }
}

/// Обратная связь для интерфейса (индикатор прогресса).
pub trait Progress: Sync {
    /// Начался проход (обход папок и индексация).
    fn started(&self) {}
    /// Проход закончен (в том числе прерванный и пустой). `None` — проход завершился ошибкой.
    fn finished(&self, _summary: Option<&Summary>) {}
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
    /// Файл лежит только в облаке (OneDrive и т. п.): чтение скачало бы его на компьютер.
    cloud: bool,
}

/// Файл-«заглушка» облачного хранилища: на диске только метаданные, содержимое скачивается при обращении.
#[cfg(windows)]
fn is_cloud_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const OFFLINE: u32 = 0x0000_1000;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    meta.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(not(windows))]
fn is_cloud_placeholder(_meta: &std::fs::Metadata) -> bool {
    false
}

fn extract_candidate(c: &Candidate, limits: &Limits) -> Extracted {
    if c.cloud {
        Extracted::without_content(Status::Skipped("облачный файл, не скачанный на компьютер".into()))
    } else {
        extract_file(&c.path, c.size, limits)
    }
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
        // На Windows и macOS регистр в путях не различается.
        let glob = GlobBuilder::new(&p.replace('\\', "/"))
            .case_insensitive(cfg!(any(windows, target_os = "macos")))
            .build()
            .with_context(|| format!("некорректный шаблон исключения: {p}"))?;
        b.add(glob);
    }
    Ok(b.build()?)
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0)
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
    // Ошибки чтения (файл заняла другая программа, сбой сети) и облачные заглушки пробуем снова при следующем проходе.
    let retry = c.cloud || matches!(ex.status, Status::Failed(_));
    doc.add_u64(f.ver, if retry { 0 } else { EXTRACTOR_VERSION });
    doc
}

/// Правила исключения файлов и папок из индекса.
pub struct Excluder {
    globs: GlobSet,
    defaults: bool,
    /// Ключи (см. [`path_key`]) папок, которые пропускаются целиком.
    skip_dirs: Vec<String>,
}

impl Excluder {
    pub fn new(patterns: &[String], defaults: bool, skip_dirs: &[PathBuf]) -> Result<Excluder> {
        let skip_dirs = skip_dirs
            .iter()
            .map(|d| path_key(&d.to_string_lossy()).trim_end_matches('/').to_string())
            .filter(|k| !k.is_empty())
            .collect();
        Ok(Excluder { globs: build_globs(patterns)?, defaults, skip_dirs })
    }

    pub fn excluded(&self, path: &Path, is_dir: bool) -> bool {
        if is_dir && !self.skip_dirs.is_empty() {
            let key = path_key(&path.to_string_lossy());
            let key = key.trim_end_matches('/');
            if self.skip_dirs.iter().any(|skip| key == skip) {
                return true;
            }
        }
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

    /// Исключён ли путь сам или любая из его папок внутри корней. Пути вне корней считаются исключёнными.
    pub fn excluded_in_roots(&self, path: &Path, is_dir: bool, roots: &[PathBuf]) -> bool {
        let Some(root) = roots.iter().find(|r| path.starts_with(r)) else { return true };
        for ancestor in path.ancestors() {
            if ancestor == root.as_path() {
                break;
            }
            if self.excluded(ancestor, ancestor != path || is_dir) {
                return true;
            }
        }
        false
    }
}

fn candidate_from(path: PathBuf, meta: &std::fs::Metadata) -> Candidate {
    let key = path_key(&path.to_string_lossy());
    Candidate { path, key, size: meta.len(), mtime: mtime_ms(meta), cloud: is_cloud_placeholder(meta) }
}

/// Заменяет запись о файле в индексе: старая удаляется, новая добавляется.
fn store(writer: &IndexWriter, fields: &Fields, c: &Candidate, ex: &Extracted) -> Result<()> {
    writer.delete_term(Term::from_field_text(fields.key, &c.key));
    writer.add_document(build_doc(fields, c, ex))?;
    Ok(())
}

/// Индексирует один файл (для режима слежения). Возвращает результат извлечения.
pub fn upsert_file(
    writer: &IndexWriter,
    fields: &Fields,
    path: &Path,
    meta: &std::fs::Metadata,
    limits: &Limits,
) -> Result<Extracted> {
    let c = candidate_from(path.to_path_buf(), meta);
    let ex = extract_candidate(&c, limits);
    store(writer, fields, &c, &ex)?;
    Ok(ex)
}

/// Удаляет из индекса файл или целую папку (всё, что лежит под этим путём).
pub fn delete_path(writer: &IndexWriter, fields: &Fields, path: &Path) -> Result<()> {
    let key = path_key(&path.to_string_lossy());
    writer.delete_term(Term::from_field_text(fields.key, &key));
    let prefix = format!("{}/", crate::query::regex_escape(key.trim_end_matches('/')));
    let children = tantivy::query::RegexQuery::from_pattern(&format!("{prefix}.*"), fields.key)?;
    writer.delete_query(Box::new(children))?;
    Ok(())
}

/// Убирает из индекса всё, что лежит под корнем (когда папку исключили из поиска).
pub fn remove_root(index: &Index, fields: &Fields, root: &Path) -> Result<()> {
    let mut writer = index.writer_with_options::<TantivyDocument>(
        IndexWriterOptions::builder().num_worker_threads(1).memory_budget_per_thread(32 << 20).build(),
    )?;
    delete_path(&writer, fields, root)?;
    writer.commit()?;
    writer.wait_merging_threads()?;
    Ok(())
}

/// Индексирует все файлы папки (например, появившейся или переименованной). Возвращает их число.
pub fn index_tree(
    writer: &IndexWriter,
    fields: &Fields,
    dir: &Path,
    excluder: &Excluder,
    limits: &Limits,
    cancel: &AtomicBool,
) -> Result<u64> {
    let mut count = 0;
    let walker = WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !excluder.excluded(e.path(), e.file_type().is_dir()));
    for entry in walker.flatten() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            upsert_file(writer, fields, entry.path(), &meta, limits)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Запись нельзя удалять из индекса, если файл или его папка не читались при обходе: они могли
/// просто оказаться недоступными (права, отключённый сетевой диск), а не исчезнуть.
fn is_protected(key: &str, unreadable_files: &[String], unreadable_prefixes: &[String]) -> bool {
    unreadable_files.iter().any(|f| f == key) || unreadable_prefixes.iter().any(|p| key.starts_with(p.as_str()))
}

/// Корни, с которыми можно работать: существующие папки без вложенных друг в друга.
/// Недоступный корень (отключённый диск) пропускается с пояснением; ошибка — только если нет ни одного.
pub fn usable_roots(roots: &[PathBuf]) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let mut good: Vec<PathBuf> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    for root in roots {
        match normalize_root(root) {
            Ok(r) => good.push(r),
            Err(e) => problems.push(format!("{e:#}")),
        }
    }
    if good.is_empty() {
        if let Some(first) = problems.first() {
            bail!("{first}");
        }
        bail!("не указано ни одной папки");
    }
    // Вложенный корень уже покрыт объемлющим; иначе его файлы переразбирались бы при каждом проходе.
    let keys: Vec<String> = good.iter().map(|r| root_prefix(r)).collect();
    let mut keep = Vec::new();
    for (i, root) in good.iter().enumerate() {
        let covered = keys
            .iter()
            .enumerate()
            .any(|(j, other)| j != i && keys[i].starts_with(other.as_str()) && (keys[i] != *other || j < i));
        if !covered {
            keep.push(root.clone());
        }
    }
    Ok((keep, problems))
}

pub fn run(
    index: &Index,
    fields: &Fields,
    opts: &Options,
    progress: &dyn Progress,
    cancel: &AtomicBool,
) -> Result<Summary> {
    progress.started();
    let result = run_inner(index, fields, opts, progress, cancel);
    progress.finished(result.as_ref().ok());
    result
}

fn run_inner(
    index: &Index,
    fields: &Fields,
    opts: &Options,
    progress: &dyn Progress,
    cancel: &AtomicBool,
) -> Result<Summary> {
    let started = Instant::now();
    let mut summary = Summary::default();

    let (roots, root_errors) = usable_roots(&opts.roots)?;
    summary.walk_errors_total += root_errors.len() as u64;
    summary.walk_errors.extend(root_errors);
    let excluder = Excluder::new(&opts.excludes, opts.default_excludes, &opts.skip_dirs)?;
    let prefixes: Vec<String> = roots.iter().map(|r| root_prefix(r)).collect();

    progress.message("Чтение текущего индекса…");
    let mut existing = load_existing(index, fields, &prefixes)?;

    // ---- обход
    let mut todo: Vec<Candidate> = Vec::new();
    // Что не удалось прочитать при обходе, то нельзя считать «пропавшим с диска»: папка может быть просто недоступна.
    let mut unreadable_prefixes: Vec<String> = Vec::new();
    let mut unreadable_files: Vec<String> = Vec::new();
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
                    if let Some(path) = err.path() {
                        let mut prefix = path_key(&path.to_string_lossy());
                        if !prefix.ends_with('/') {
                            prefix.push('/');
                        }
                        unreadable_prefixes.push(prefix);
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
                    unreadable_files.push(path_key(&entry.path().to_string_lossy()));
                    continue;
                }
            };
            summary.scanned += 1;
            if summary.scanned % 1000 == 0 {
                progress.scanning(summary.scanned);
            }
            let cand = candidate_from(entry.into_path(), &meta);
            match existing.remove(&cand.key) {
                Some(e)
                    if !opts.force && e.mtime == cand.mtime && e.size == cand.size && e.ver == EXTRACTOR_VERSION =>
                {
                    summary.unchanged += 1;
                }
                _ => todo.push(cand),
            }
        }
    }
    progress.scanning(summary.scanned);

    // Всё, что осталось в `existing`, из индекса пропало с диска. При прерванном обходе судить нельзя.
    let removed: Vec<String> = if summary.cancelled {
        Vec::new()
    } else {
        existing.into_keys().filter(|key| !is_protected(key, &unreadable_files, &unreadable_prefixes)).collect()
    };
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
    let last_commit = std::sync::Mutex::new(Instant::now());
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
            let ex = extract_candidate(c, &opts.limits);
            let text_len = ex.text.len() + ex.meta.len();
            counters.record(&ex);
            {
                let w = writer.read().unwrap();
                if let Err(e) = store(&w, fields, c, &ex) {
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
            let due = opts.commit_every.is_some_and(|every| last_commit.lock().unwrap().elapsed() >= every);
            if bytes > COMMIT_TEXT_BUDGET || docs > COMMIT_DOC_BUDGET || due {
                // Коммит нужен, чтобы очередь tantivy не накопила гигабайты текста.
                let mut w = writer.write().unwrap();
                if pending_bytes.load(Ordering::Relaxed) > 0 {
                    if let Err(e) = w.commit() {
                        *commit_error.lock().unwrap() = Some(e.into());
                    }
                    pending_bytes.store(0, Ordering::Relaxed);
                    pending_docs.store(0, Ordering::Relaxed);
                    *last_commit.lock().unwrap() = Instant::now();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_places_protect_their_documents() {
        let files = vec!["/d/a/locked.docx".to_string()];
        let dirs = vec!["/d/secret/".to_string()];
        assert!(is_protected("/d/a/locked.docx", &files, &dirs));
        assert!(is_protected("/d/secret/x/y.txt", &files, &dirs));
        assert!(!is_protected("/d/secret2/y.txt", &files, &dirs), "соседняя папка с похожим именем не защищена");
        assert!(!is_protected("/d/a/other.docx", &files, &dirs));
    }

    #[test]
    fn nested_duplicate_and_missing_roots_are_sorted_out() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let sub = a.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let gone = dir.path().join("gone");

        let (roots, problems) = usable_roots(&[a.clone(), sub.clone(), a.clone(), gone.clone()]).unwrap();
        assert_eq!(roots.len(), 1, "{roots:?}");
        assert!(roots[0].ends_with("a"));
        assert_eq!(problems.len(), 1, "недоступная папка пропущена с пояснением: {problems:?}");

        assert!(usable_roots(&[gone]).is_err(), "если не осталось ни одной папки — это ошибка");
        assert!(usable_roots(&[]).is_err());
    }

    #[test]
    fn skip_dirs_prune_only_the_named_directory() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("idx");
        let ex = Excluder::new(&[], true, std::slice::from_ref(&index)).unwrap();
        assert!(ex.excluded(&index, true));
        assert!(!ex.excluded(&dir.path().join("idx2"), true), "папка с похожим именем не исключается");
        assert!(ex.excluded_in_roots(&index.join("segment.idx"), false, &[dir.path().to_path_buf()]));
        assert!(!ex.excluded_in_roots(&dir.path().join("doc.txt"), false, &[dir.path().to_path_buf()]));
    }
}
