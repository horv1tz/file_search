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
use notify::{EventKind, RecursiveMode, Watcher};
use tantivy::Index;
use tantivy::indexer::IndexWriterOptions;

use crate::indexer::{self, Excluder, Options, Silent, delete_path, index_tree, normalize_root, upsert_file};
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

fn stamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Сначала догоняет текущее состояние диска, затем следит за изменениями, пока не взведён `cancel`.
pub fn watch(
    index: &Index,
    fields: &Fields,
    opts: &WatchOptions,
    log: &dyn Fn(&str),
    cancel: &AtomicBool,
) -> Result<()> {
    let roots: Vec<PathBuf> = opts.index.roots.iter().map(|r| normalize_root(r)).collect::<Result<_>>()?;
    let excluder = Excluder::new(&opts.index.excludes, opts.index.default_excludes)?;
    let aliases = root_aliases(&roots);

    log("Первичная проверка индекса…");
    let s = indexer::run(index, fields, &opts.index, &Silent, cancel)?;
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

    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
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
                    for p in event.paths {
                        pending.insert(rebase(p, &aliases), Instant::now());
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
        // Файлы, в которые постоянно пишут, не переразбираем чаще min_reindex_interval.
        // Удаления и папки применяются сразу: они дёшевы.
        let ready: Vec<PathBuf> = pending
            .iter()
            .filter(|(_, seen)| now.duration_since(**seen) >= opts.debounce)
            .filter(|(path, _)| match last_indexed.get(*path) {
                Some(t) if now.duration_since(*t) < opts.min_reindex_interval => {
                    !fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
                }
                _ => true,
            })
            .map(|(p, _)| p.clone())
            .collect();
        if !ready.is_empty() {
            for p in &ready {
                pending.remove(p);
                last_indexed.insert(p.clone(), now);
            }
            match apply(index, fields, opts, &excluder, &roots, &ready) {
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
            match indexer::run(index, fields, &opts.index, &Silent, cancel) {
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
    paths: &[PathBuf],
) -> Result<(u64, u64)> {
    let writer_opts = IndexWriterOptions::builder().num_worker_threads(1).memory_budget_per_thread(48 << 20).build();
    let mut writer = index.writer_with_options::<tantivy::TantivyDocument>(writer_opts)?;
    let (mut updated, mut removed) = (0u64, 0u64);
    for path in paths {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => {
                if excluder.excluded_in_roots(path, false, roots) {
                    continue;
                }
                upsert_file(&writer, fields, path, &meta, &opts.index.limits)?;
                updated += 1;
            }
            Ok(meta) if meta.is_dir() => {
                if !excluder.excluded_in_roots(path, true, roots) {
                    updated += index_tree(&writer, fields, path, excluder, &opts.index.limits)?;
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
    writer.wait_merging_threads()?;
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
    fn no_aliases_for_plain_roots() {
        let dir = std::env::temp_dir();
        let real = fs::canonicalize(&dir).unwrap();
        assert!(root_aliases(&[real]).is_empty());
    }
}
