//! Слежение за папками: изменения на диске попадают в индекс сами.
//!
//! События файловой системы копятся и обрабатываются пачками, когда файл «успокоился»
//! (редакторы и Office пишут файлы в несколько приёмов). Раз в несколько минут
//! выполняется полная инкрементальная проверка — на случай пропущенных событий
//! (сетевые диски, переполнение буфера, лимит inotify).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::event::ModifyKind;
use notify::{EventKind, RecursiveMode, Watcher};
use tantivy::Index;
use tantivy::indexer::IndexWriterOptions;

use crate::indexer::{self, Excluder, Options, Progress, delete_path, index_tree, upsert_file, usable_roots};
use crate::schema::Fields;

pub struct WatchOptions {
    pub index: Options,
    /// Сколько файл должен «молчать», прежде чем его читать.
    pub debounce: Duration,
    /// Не переиндексировать один и тот же файл чаще, чем раз в этот срок (файлы, в которые постоянно пишут).
    pub min_reindex_interval: Duration,
    /// Период полной проверки.
    pub rescan_every: Duration,
}

impl WatchOptions {
    pub fn new(index: Options) -> Self {
        WatchOptions {
            index,
            debounce: Duration::from_millis(1500),
            min_reindex_interval: Duration::from_secs(30),
            rescan_every: Duration::from_secs(30 * 60),
        }
    }
}

/// Корни, заданные через символические ссылки: (реальный путь, путь как задан).
/// На macOS `/var` — ссылка на `/private/var`, и FSEvents сообщает реальные пути.
fn root_aliases(roots: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    roots
        .iter()
        .filter_map(|r| {
            let real = fs::canonicalize(r).ok()?;
            (real != *r).then(|| (real, r.clone()))
        })
        .collect()
}

/// Приводит путь из события к виду, в котором корень задан пользователем (иначе ключи в индексе разъедутся).
fn rebase(path: PathBuf, aliases: &[(PathBuf, PathBuf)]) -> PathBuf {
    for (real, given) in aliases {
        if let Ok(rest) = path.strip_prefix(real) {
            return given.join(rest);
        }
    }
    path
}

/// Событие говорит о появлении пути (создан, переименован, перемещён) — тогда новую папку нужно обойти целиком.
/// Обычное «изменено» у папки (на Windows оно приходит при любой записи внутри) обходить не нужно:
/// новые файлы в ней сообщат о себе сами.
fn introduces_path(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_)) | EventKind::Any | EventKind::Other)
}

struct Pending {
    seen: Instant,
    /// Если путь окажется папкой — обойти её целиком.
    tree: bool,
}

fn stamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Сначала догоняет текущее состояние диска, затем следит за изменениями, пока не взведён `cancel`.
pub fn watch(
    index: &Index,
    fields: &Fields,
    opts: &WatchOptions,
    progress: &dyn Progress,
    log: &dyn Fn(&str),
    cancel: &AtomicBool,
) -> Result<()> {
    let (roots, problems) = usable_roots(&opts.index.roots)?;
    for problem in problems {
        log(&format!("Пропущено: {problem}"));
    }
    let excluder = Excluder::new(&opts.index.excludes, opts.index.default_excludes, &opts.index.skip_dirs)?;
    let aliases = root_aliases(&roots);

    log("Первичная проверка индекса…");
    let s = indexer::run(index, fields, &opts.index, progress, cancel)?;
    // «Переиндексировать всё» относится только к первому проходу.
    let mut rescan_opts = opts.index.clone();
    rescan_opts.force = false;
    log(&format!(
        "Проверено файлов: {}, добавлено или обновлено: {}, удалено: {}. Слежу за изменениями…",
        s.scanned, s.indexed, s.removed
    ));

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    for root in &roots {
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            log(&format!(
                "Не удалось следить за {}: {e}. Изменения там будут подхвачены при периодической проверке.",
                root.display()
            ));
        }
    }

    let mut pending: HashMap<PathBuf, Pending> = HashMap::new();
    let mut last_indexed: HashMap<PathBuf, Instant> = HashMap::new();
    let mut rescan_needed = false;
    let mut last_scan = Instant::now();
    let mut last_event = Instant::now();

    while !cancel.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(Ok(event)) => {
                last_event = Instant::now();
                if event.need_rescan() {
                    rescan_needed = true;
                } else if !matches!(event.kind, EventKind::Access(_)) {
                    let tree = introduces_path(&event.kind);
                    for p in event.paths {
                        let p = rebase(p, &aliases);
                        // Сразу отбрасываем служебное: файлы самого индекса, блокировки Office, исключённые папки.
                        if excluder.excluded_in_roots(&p, false, &roots) {
                            continue;
                        }
                        let entry = pending.entry(p).or_insert(Pending { seen: Instant::now(), tree });
                        entry.seen = Instant::now();
                        entry.tree |= tree;
                    }
                }
            }
            Ok(Err(e)) => {
                log(&format!("Ошибка слежения: {e}. Будет выполнена полная проверка."));
                rescan_needed = true;
                last_event = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let now = Instant::now();
        // Файлы и папки, которые постоянно меняются, не переразбираем чаще min_reindex_interval.
        // Удаления применяются сразу: они дёшевы.
        let ready: Vec<(PathBuf, bool)> = pending
            .iter()
            .filter(|(_, p)| now.duration_since(p.seen) >= opts.debounce)
            .filter(|(path, _)| match last_indexed.get(*path) {
                Some(t) if now.duration_since(*t) < opts.min_reindex_interval => fs::symlink_metadata(path).is_err(),
                _ => true,
            })
            .map(|(path, p)| (path.clone(), p.tree))
            .collect();
        if !ready.is_empty() {
            for (path, _) in &ready {
                pending.remove(path);
                last_indexed.insert(path.clone(), now);
            }
            match apply(index, fields, opts, &excluder, &roots, &ready, cancel) {
                Ok((updated, removed)) if updated + removed > 0 => {
                    log(&format!("Обновлено: {updated}, удалено: {removed}"))
                }
                Ok(_) => {}
                Err(e) => log(&format!("Не удалось обновить индекс: {e:#}")),
            }
            if last_indexed.len() > 50_000 {
                last_indexed.retain(|_, t| now.duration_since(*t) < opts.min_reindex_interval);
            }
        }

        let quiet = last_event.elapsed() >= opts.debounce;
        if (rescan_needed && quiet) || last_scan.elapsed() >= opts.rescan_every {
            match indexer::run(index, fields, &rescan_opts, progress, cancel) {
                Ok(s) if s.indexed + s.removed > 0 => {
                    log(&format!("Полная проверка: обновлено {}, удалено {}", s.indexed, s.removed))
                }
                Ok(_) => {}
                Err(e) => log(&format!("Полная проверка не удалась: {e:#}")),
            }
            rescan_needed = false;
            last_scan = Instant::now();
        }
    }
    Ok(())
}

/// Применяет пачку изменённых путей. Возвращает (обновлено, удалено).
fn apply(
    index: &Index,
    fields: &Fields,
    opts: &WatchOptions,
    excluder: &Excluder,
    roots: &[PathBuf],
    items: &[(PathBuf, bool)],
    cancel: &AtomicBool,
) -> Result<(u64, u64)> {
    let writer_opts = IndexWriterOptions::builder().num_worker_threads(1).memory_budget_per_thread(48 << 20).build();
    let mut writer = index.writer_with_options::<tantivy::TantivyDocument>(writer_opts)?;
    let (mut updated, mut removed) = (0u64, 0u64);
    for (path, tree) in items {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => {
                if excluder.excluded_in_roots(path, false, roots) {
                    continue;
                }
                upsert_file(&writer, fields, path, &meta, &opts.index.limits)?;
                updated += 1;
            }
            // Папка: целиком обходим только появившуюся или переименованную, простое «изменена» игнорируем.
            Ok(meta) if meta.is_dir() => {
                if *tree && !excluder.excluded_in_roots(path, true, roots) {
                    updated += index_tree(&writer, fields, path, excluder, &opts.index.limits, cancel)?;
                }
            }
            Ok(_) => {}
            Err(_) => {
                delete_path(&writer, fields, path)?;
                removed += 1;
            }
        }
    }
    writer.commit()?;
    // Слияние мелких сегментов, чтобы поиск не замедлялся за долгую работу.
    if !cancel.load(Ordering::Relaxed) {
        writer.wait_merging_threads()?;
    }
    Ok((updated, removed))
}

pub fn log_line(text: &str) {
    eprintln!("[{}] {text}", stamp());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_paths_are_mapped_back_to_the_given_root() {
        let aliases = vec![(PathBuf::from("/private/var/docs"), PathBuf::from("/var/docs"))];
        assert_eq!(rebase(PathBuf::from("/private/var/docs/a/b.txt"), &aliases), PathBuf::from("/var/docs/a/b.txt"));
        assert_eq!(rebase(PathBuf::from("/private/var/docs"), &aliases), PathBuf::from("/var/docs"));
        assert_eq!(rebase(PathBuf::from("/other/x.txt"), &aliases), PathBuf::from("/other/x.txt"));
        assert_eq!(
            rebase(PathBuf::from("/private/var/docs2/x.txt"), &aliases),
            PathBuf::from("/private/var/docs2/x.txt")
        );
    }

    #[test]
    fn only_new_paths_trigger_a_tree_walk() {
        use notify::event::{CreateKind, DataChange, MetadataKind, RenameMode};
        assert!(introduces_path(&EventKind::Create(CreateKind::Any)));
        assert!(introduces_path(&EventKind::Modify(ModifyKind::Name(RenameMode::To))));
        // На Windows «папка изменена» приходит при любой записи внутри неё — обходить её заново нельзя.
        assert!(!introduces_path(&EventKind::Modify(ModifyKind::Any)));
        assert!(!introduces_path(&EventKind::Modify(ModifyKind::Data(DataChange::Any))));
        assert!(!introduces_path(&EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any))));
        assert!(!introduces_path(&EventKind::Remove(notify::event::RemoveKind::Any)));
    }

    #[test]
    fn no_aliases_for_plain_roots() {
        let dir = std::env::temp_dir();
        let real = fs::canonicalize(&dir).unwrap();
        assert!(root_aliases(&[real]).is_empty());
    }
}
