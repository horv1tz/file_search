//! Режим слежения: изменения на диске попадают в индекс без ручной переиндексации.
//!
//! Тест опирается на события файловой системы, поэтому ждёт результат с запасом по времени.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use file_search::indexer::Options;
use file_search::schema::open_or_create_index;
use file_search::search::{SearchOptions, Searcher};
use file_search::watcher::{self, WatchOptions};
use tempfile::TempDir;

fn names(index_dir: &Path, query: &str) -> Vec<String> {
    let searcher = Searcher::open(index_dir, false).unwrap();
    let opts = SearchOptions { query: query.into(), ..SearchOptions::default() };
    let mut v: Vec<String> = searcher.search(&opts).unwrap().hits.into_iter().map(|h| h.path).collect();
    v.sort();
    v
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
    panic!("за 40 секунд не дождались: {what}");
}

#[test]
fn watch_applies_creates_changes_moves_and_deletes() {
    let docs = TempDir::new().unwrap();
    let idx = TempDir::new().unwrap();
    fs::write(docs.path().join("a.txt"), "исходный текст про Жирафа").unwrap();

    let (index, fields) = open_or_create_index(idx.path()).unwrap();
    let mut opts = WatchOptions::new(Options::new(vec![docs.path().to_path_buf()]));
    opts.debounce = Duration::from_millis(200);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = {
        let cancel = cancel.clone();
        thread::spawn(move || watcher::watch(&index, &fields, &opts, &file_search::indexer::Silent, &|_| {}, &cancel))
    };
    let q = |query: &str| names(idx.path(), query);

    wait_until("первичная индексация", || q("Жирафа").len() == 1);

    // новый файл и новая папка с вложенным файлом
    fs::write(docs.path().join("b.txt"), "новый файл про Верблюда").unwrap();
    fs::create_dir_all(docs.path().join("dir/deep")).unwrap();
    fs::write(docs.path().join("dir/deep/c.txt"), "вложенный файл про Ленивца").unwrap();
    wait_until("новый файл", || q("Верблюда").len() == 1);
    wait_until("файл в новой папке", || q("Ленивца").len() == 1);

    // изменение: старый текст пропадает, новый появляется
    fs::write(docs.path().join("a.txt"), "переписанный текст про Тапира и ничего больше").unwrap();
    wait_until("новый текст файла", || q("Тапира").len() == 1);
    assert!(q("Жирафа").is_empty(), "старый текст должен исчезнуть из индекса");

    // удаление сразу после создания не должно ждать «антидребезга»
    fs::remove_file(docs.path().join("b.txt")).unwrap();
    wait_until("удаление файла", || q("Верблюда").is_empty());

    // переименование папки: записи переезжают
    fs::rename(docs.path().join("dir"), docs.path().join("moved")).unwrap();
    wait_until("переименование папки", || {
        let found = q("Ленивца");
        found.len() == 1 && found[0].contains("moved")
    });

    // удаление папки
    fs::remove_dir_all(docs.path().join("moved")).unwrap();
    wait_until("удаление папки", || q("Ленивца").is_empty());

    // блокировки Office не попадают в индекс
    fs::write(docs.path().join("~$a.docx"), "владелец").unwrap();
    fs::write(docs.path().join("marker.txt"), "маркер завершения про Окапи").unwrap();
    wait_until("маркерный файл", || q("Окапи").len() == 1);
    assert!(q("name:owner").is_empty());
    assert!(q("name:a.docx").is_empty());

    cancel.store(true, Ordering::SeqCst);
    worker.join().unwrap().unwrap();
}

#[test]
fn watcher_does_not_index_its_own_index_folder() {
    let docs = TempDir::new().unwrap();
    fs::write(docs.path().join("a.txt"), "начальный документ про Рысь").unwrap();
    // Индекс лежит внутри наблюдаемой папки — худший случай (на Windows так бывает, если индексировать весь диск C:).
    let index_dir = docs.path().join(".index");
    let (index, fields) = open_or_create_index(&index_dir).unwrap();

    let mut options = Options::new(vec![docs.path().to_path_buf()]);
    options.skip_dirs = vec![index_dir.clone()];
    let mut opts = WatchOptions::new(options);
    opts.debounce = Duration::from_millis(200);

    let updates = Arc::new(std::sync::Mutex::new(0usize));
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = {
        let (cancel, updates) = (cancel.clone(), updates.clone());
        thread::spawn(move || {
            let log = move |line: &str| {
                if line.starts_with("Обновлено") {
                    *updates.lock().unwrap() += 1;
                }
            };
            watcher::watch(&index, &fields, &opts, &file_search::indexer::Silent, &log, &cancel)
        })
    };
    let q = |query: &str| names(&index_dir, query);
    wait_until("первичная индексация", || q("Рысь").len() == 1);

    fs::write(docs.path().join("b.txt"), "второй документ про Куницу").unwrap();
    wait_until("новый файл", || q("Куницу").len() == 1);
    // Если бы файлы индекса попадали в очередь, коммиты порождали бы всё новые обновления.
    thread::sleep(Duration::from_secs(4));
    let count = *updates.lock().unwrap();
    assert_eq!(count, 1, "ровно одно обновление (новый файл), без самоподпитки; было {count}");
    assert_eq!(q("Рысь").len(), 1);

    cancel.store(true, Ordering::SeqCst);
    worker.join().unwrap().unwrap();
}
