//! Сетевые папки (`\\сервер\ресурс`). Настоящий сервер в тесте не нужен: Windows отдаёт локальные диски
//! по сети под именем `\\localhost\C$`. Если такой общий ресурс недоступен (нет прав), тест молча пропускается.

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use file_search::indexer::{self, Options, Silent};
use file_search::netpath::{self, is_network_path};
use file_search::schema::open_or_create_index;
use file_search::search::{SearchOptions, Searcher};
use tempfile::TempDir;

/// `C:\a\b` → `\\localhost\C$\a\b`.
fn as_unc(local: &Path) -> Option<PathBuf> {
    let text = local.to_str()?;
    let bytes = text.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' || bytes[2] != b'\\' {
        return None;
    }
    Some(PathBuf::from(format!(r"\\localhost\{}${}", bytes[0] as char, &text[2..])))
}

fn search(index: &Path, query: &str) -> Vec<String> {
    let opts = SearchOptions { query: query.into(), ..SearchOptions::default() };
    Searcher::open(index, false).unwrap().search(&opts).unwrap().hits.into_iter().map(|h| h.path).collect()
}

#[test]
fn unc_folder_is_indexed_searched_and_survives_going_offline() {
    let docs = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    fs::create_dir_all(docs.path().join("Договоры")).unwrap();
    fs::write(docs.path().join("Договоры").join("аренда.txt"), "Договор аренды склада на Таганке").unwrap();
    fs::write(docs.path().join("notes.txt"), "Тюльпановый отчёт за квартал").unwrap();

    let Some(unc) = as_unc(docs.path()) else {
        eprintln!("пропуск: временная папка не на диске с буквой");
        return;
    };
    if fs::metadata(&unc).is_err() {
        eprintln!("пропуск: {} недоступна по сети на этом компьютере", unc.display());
        return;
    }
    assert!(is_network_path(&unc));

    let (index, fields) = open_or_create_index(index_dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let opts = Options::new(vec![unc.clone()]);

    // Первый проход: файлы найдены по сетевому пути.
    let s = indexer::run(&index, &fields, &opts, &Silent, &cancel).unwrap();
    assert_eq!((s.scanned, s.indexed), (2, 2), "{:?}", s.walk_errors);
    let hits = search(index_dir.path(), "аренды");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].to_lowercase().starts_with(r"\\localhost\"), "{}", hits[0]);

    // Повторный проход ничего не переделывает.
    let s = indexer::run(&index, &fields, &opts, &Silent, &cancel).unwrap();
    assert_eq!((s.indexed, s.unchanged, s.removed), (0, 2, 0));

    // Папка с другим регистром букв и косыми чертами — тот же корень, дубликатов не появляется.
    let alias = PathBuf::from(unc.to_string_lossy().replace('\\', "/").to_uppercase());
    let two = Options::new(vec![unc.clone(), alias]);
    let s = indexer::run(&index, &fields, &two, &Silent, &cancel).unwrap();
    assert_eq!((s.indexed, s.unchanged, s.removed), (0, 2, 0), "{:?}", s.walk_errors);

    // «Сервер вернул пустую папку»: записи не удаляются, в отчёте есть пояснение.
    fs::remove_file(docs.path().join("notes.txt")).unwrap();
    fs::remove_dir_all(docs.path().join("Договоры")).unwrap();
    let s = indexer::run(&index, &fields, &opts, &Silent, &cancel).unwrap();
    assert_eq!(s.removed, 0);
    assert!(s.walk_errors.iter().any(|e| e.contains("недоступна")), "{:?}", s.walk_errors);
    assert_eq!(search(index_dir.path(), "аренды").len(), 1, "документы остались в индексе");
}

#[test]
fn unreachable_server_is_reported_quickly() {
    // Зарезервированный для документации адрес: никто не отвечает.
    let path = PathBuf::from(r"\\192.0.2.1\docs");
    let started = std::time::Instant::now();
    let err = netpath::check_dir(&path, std::time::Duration::from_secs(3)).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(8), "ждали слишком долго: {err}");
    assert!(err.contains("192.0.2.1"), "{err}");
}
