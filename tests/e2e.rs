//! Сквозные тесты: индексация папки и поиск через библиотечный API.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use file_search::indexer::{self, Options, Silent, Summary};
use file_search::query::Mode;
use file_search::schema::open_or_create_index;
use file_search::search::{SearchOptions, SearchResult, Searcher, Sort};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

struct Env {
    docs: TempDir,
    index: TempDir,
}

impl Env {
    /// Папка с документами разных форматов в подпапках, как на реальном диске.
    fn new() -> Env {
        let docs = TempDir::new().unwrap();
        for (sub, files) in [
            ("Договоры", &["contract.docx", "contract.doc", "contract.pdf"][..]),
            ("Отчёты", &["sales.xlsx", "sales.xls"][..]),
            ("Презентации", &["strategy.pptx", "strategy.odp"][..]),
        ] {
            fs::create_dir_all(docs.path().join(sub)).unwrap();
            for f in files {
                fs::copy(fixture(f), docs.path().join(sub).join(f)).unwrap();
            }
        }
        Env { docs, index: TempDir::new().unwrap() }
    }

    fn root(&self) -> &Path {
        self.docs.path()
    }

    fn index_with(&self, opts: Options) -> Summary {
        let (index, fields) = open_or_create_index(self.index.path()).unwrap();
        indexer::run(&index, &fields, &opts, &Silent, &AtomicBool::new(false)).unwrap()
    }

    fn index(&self) -> Summary {
        self.index_with(Options::new(vec![self.root().to_path_buf()]))
    }

    fn search(&self, query: &str) -> SearchResult {
        self.search_with(SearchOptions { query: query.into(), ..SearchOptions::default() })
    }

    fn search_with(&self, opts: SearchOptions) -> SearchResult {
        Searcher::open(self.index.path(), false).unwrap().search(&opts).unwrap()
    }

    /// Имена найденных файлов, отсортированные.
    fn names(&self, query: &str) -> Vec<String> {
        let mut v: Vec<String> = self.search(query).hits.into_iter().map(|h| h.name).collect();
        v.sort();
        v
    }
}

#[test]
fn indexes_everything_and_finds_content_in_every_format() {
    let env = Env::new();
    let s = env.index();
    assert_eq!((s.scanned, s.indexed, s.unchanged, s.removed), (7, 7, 0, 0));
    assert!(s.failed.is_empty(), "{:?}", s.failed);

    assert_eq!(env.names("Кракозябровая"), ["contract.doc", "contract.docx", "contract.pdf"]);
    assert_eq!(env.names("Тюльпаны"), ["sales.xls", "sales.xlsx"]);
    assert_eq!(env.names("Ландыш"), ["strategy.odp", "strategy.pptx"]);
    assert_eq!(env.names("Нептуновой"), ["sales.xlsx"]);
    assert_eq!(env.names("Стругацкого"), ["strategy.odp", "strategy.pptx"]);
}

#[test]
fn russian_and_english_word_forms_are_matched() {
    let env = Env::new();
    env.index();
    // «Арендатору» в тексте, ищем другой падеж
    assert_eq!(env.names("арендатором"), ["contract.doc", "contract.docx", "contract.pdf"]);
    // ё = е
    assert_eq!(env.names("Пётр"), env.names("Петр"));
    // английская морфология: в тексте «quarterly revenue»
    assert!(!env.names("QUARTER revenues").is_empty());
}

#[test]
fn phrases_prefixes_fuzzy_negation_and_or() {
    let env = Env::new();
    env.index();
    assert_eq!(env.names("\"срок действия\""), ["contract.doc", "contract.docx", "contract.pdf"]);
    assert!(env.names("\"действия срок\"").is_empty(), "порядок слов во фразе важен");
    assert_eq!(env.names("кракоз*"), ["contract.doc", "contract.docx", "contract.pdf"]);
    assert_eq!(env.names("Кракозябровая~"), env.names("Кракозябровая"));
    assert_eq!(env.names("Кракозябровя~").len(), 3, "одна опечатка допускается");
    assert!(env.names("Кракозябровая -Казань").is_empty());
    assert_eq!(env.names("Кракозябровая -Зазеркальск").len(), 3);
    let both = env.names("Тюльпаны OR Ландыш");
    assert_eq!(both, ["sales.xls", "sales.xlsx", "strategy.odp", "strategy.pptx"]);
    // все слова должны присутствовать
    assert!(env.names("Тюльпаны Ландыш").is_empty());
}

#[test]
fn file_names_are_searched_by_word_and_by_substring() {
    let env = Env::new();
    env.index();
    assert_eq!(env.names("name:contract").len(), 3);
    assert_eq!(env.names("name:trat").len(), 2, "подстрока внутри слова");
    let r = env.search_with(SearchOptions { query: "sales".into(), mode: Mode::Name, ..Default::default() });
    assert_eq!(r.total, 2);
    let r = env.search_with(SearchOptions { query: "sales".into(), mode: Mode::Content, ..Default::default() });
    assert_eq!(r.total, 0, "в содержимом слова sales нет");
    // слова из названия папки
    assert_eq!(env.names("path:договоры").len(), 3);
}

#[test]
fn name_matches_outrank_content_matches() {
    let env = Env::new();
    fs::write(env.root().join("budget.txt"), "нечто совсем другое").unwrap();
    fs::write(
        env.root().join("notes.txt"),
        "в этой заметке упомянут budget один раз среди прочего текста про другое дело",
    )
    .unwrap();
    env.index();
    let r = env.search("budget");
    assert_eq!(r.hits[0].name, "budget.txt");
    assert_eq!(r.hits[1].name, "notes.txt");
}

#[test]
fn filters_by_type_folder_size_and_date() {
    let env = Env::new();
    env.index();
    let base = SearchOptions { query: "Ромашка".into(), ..Default::default() };

    let only_docx = env.search_with(SearchOptions { exts: vec!["docx".into()], ..base.clone() });
    assert_eq!(only_docx.hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["contract.docx"]);
    assert_eq!(env.search("ext:xls,xlsx Ромашка").total, 2);

    let in_reports = env.search_with(SearchOptions {
        dirs: vec![env.root().join("Отчёты").to_string_lossy().into_owned()],
        ..base.clone()
    });
    assert_eq!(in_reports.total, 2);
    let with_slash = env
        .search_with(SearchOptions {
            dirs: vec![format!("{}/", env.root().join("Отчёты").display())], ..base.clone()
        });
    assert_eq!(with_slash.total, 2);

    let big = env.search_with(SearchOptions { min_size: Some(30_000), ..base.clone() });
    assert!(big.hits.iter().all(|h| h.size >= 30_000) && big.total > 0);
    let small = env.search_with(SearchOptions { max_size: Some(10_000), ..base.clone() });
    assert!(small.hits.iter().all(|h| h.size <= 10_000) && small.total > 0);

    let future = env.search_with(SearchOptions { modified_after: Some(u64::MAX / 4), ..base.clone() });
    assert_eq!(future.total, 0);
    let past = env.search_with(SearchOptions { modified_before: Some(u64::MAX / 4), ..base });
    assert!(past.total > 0);
}

#[test]
fn filters_alone_list_files_and_sorting_works() {
    let env = Env::new();
    env.index();
    let r = env
        .search_with(SearchOptions { exts: vec!["docx".into(), "xlsx".into(), "pptx".into()], ..Default::default() });
    assert_eq!(r.total, 3);
    let largest = env.search_with(SearchOptions { sort: Sort::Largest, limit: 100, ..Default::default() });
    let sizes: Vec<u64> = largest.hits.iter().map(|h| h.size).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
    let smallest = env.search_with(SearchOptions { sort: Sort::Smallest, limit: 100, ..Default::default() });
    let sizes: Vec<u64> = smallest.hits.iter().map(|h| h.size).collect();
    assert!(sizes.windows(2).all(|w| w[0] <= w[1]), "{sizes:?}");
}

#[test]
fn pagination_covers_all_results_without_overlap() {
    let env = Env::new();
    env.index();
    let all: Vec<String> = env
        .search_with(SearchOptions { exts: vec![], limit: 100, ..Default::default() })
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect();
    assert_eq!(all.len(), 7);
    let mut paged = Vec::new();
    for offset in (0..7).step_by(3) {
        let page = env.search_with(SearchOptions { limit: 3, offset, sort: Sort::Newest, ..Default::default() });
        assert_eq!(page.total, 7);
        paged.extend(page.hits.into_iter().map(|h| h.path));
    }
    paged.sort();
    let mut all_sorted = all;
    all_sorted.sort();
    assert_eq!(paged, all_sorted);
}

#[test]
fn snippets_are_highlighted_and_know_their_sheet_or_slide() {
    let env = Env::new();
    env.index();
    let hit = |q: &str, name: &str| {
        env.search(q).hits.into_iter().find(|h| h.name == name).unwrap_or_else(|| panic!("{name} не найден по {q}"))
    };

    let h = hit("Quarterly", "sales.xlsx");
    assert_eq!(h.location.as_deref(), Some("Лист «Sheet2»"));
    let snippet = h.snippet.unwrap();
    assert!(snippet.parts.iter().any(|p| p.hit && p.text.eq_ignore_ascii_case("quarterly")), "{snippet:?}");
    assert!(!snippet.parts.iter().any(|p| p.text.contains("Ромашка")), "сниппет не должен захватывать соседний лист");

    let h = hit("Ландыш", "strategy.pptx");
    assert_eq!(h.location.as_deref(), Some("Слайд 2"));
    let joined: String = h.snippet.unwrap().parts.iter().map(|p| p.text.as_str()).collect();
    assert!(joined.contains("Ландыш"));

    let h = hit("Хабибуллин", "strategy.odp");
    assert_eq!(h.location.as_deref(), Some("Слайд 3"));
}

#[test]
fn prefix_queries_are_highlighted_too() {
    let env = Env::new();
    env.index();
    let h = env.search("кракоз*").hits.into_iter().find(|h| h.name == "contract.docx").unwrap();
    let hits: Vec<_> = h.snippet.unwrap().parts.into_iter().filter(|p| p.hit).collect();
    assert!(hits.iter().any(|p| p.text.to_lowercase().starts_with("кракоз")), "{hits:?}");
}

#[test]
fn incremental_update_handles_changes_removals_and_additions() {
    let env = Env::new();
    env.index();

    let again = env.index();
    assert_eq!((again.indexed, again.unchanged, again.removed), (0, 7, 0));

    fs::write(env.root().join("Отчёты/new.txt"), "свежая заметка про Единорога").unwrap();
    fs::remove_file(env.root().join("Договоры/contract.pdf")).unwrap();
    fs::write(env.root().join("Договоры/contract.docx.txt"), "копия").unwrap();
    // «изменение»: файл заменяется другим содержимым (размер отличается)
    fs::copy(fixture("sales.ods"), env.root().join("Отчёты/sales.xls")).unwrap();

    let s = env.index();
    assert_eq!((s.indexed, s.removed), (3, 1), "{s:?}");
    assert_eq!(env.names("Единорога"), ["new.txt"]);
    assert_eq!(env.names("Кракозябровая"), ["contract.doc", "contract.docx"]);
    assert_eq!(env.search("Тюльпаны").total, 2, "измененный sales.xls читается заново");
}

#[test]
fn deleted_text_disappears_from_results() {
    let env = Env::new();
    fs::write(env.root().join("a.txt"), "старый текст про Мамонта").unwrap();
    env.index();
    assert_eq!(env.names("Мамонта"), ["a.txt"]);
    fs::write(env.root().join("a.txt"), "совсем новый текст про Пингвина, длиннее прежнего").unwrap();
    env.index();
    assert!(env.names("Мамонта").is_empty());
    assert_eq!(env.names("Пингвина"), ["a.txt"]);
}

#[test]
fn periodic_commits_keep_the_index_consistent() {
    let env = Env::new();
    for i in 0..60 {
        fs::write(env.root().join(format!("заметка{i}.txt")), format!("текст заметки номер {i} про Лемура")).unwrap();
    }
    let mut opts = Options::new(vec![env.root().to_path_buf()]);
    opts.commit_every = Some(std::time::Duration::from_millis(1));
    let s = env.index_with(opts);
    assert_eq!(s.indexed, 67);
    assert_eq!(env.search("Лемура").total, 60);
    assert_eq!(env.search("Тюльпаны").total, 2, "документы на месте, дублей нет");
}

#[test]
fn force_reindexes_everything() {
    let env = Env::new();
    env.index();
    let mut opts = Options::new(vec![env.root().to_path_buf()]);
    opts.force = true;
    let s = env.index_with(opts);
    assert_eq!((s.indexed, s.unchanged), (7, 0));
    assert_eq!(env.search("Тюльпаны").total, 2, "после переиндексации дублей нет");
}

#[test]
fn separate_roots_do_not_remove_each_other() {
    let a = Env::new();
    let b_docs = TempDir::new().unwrap();
    fs::write(b_docs.path().join("b.txt"), "второй корень про Кенгуру").unwrap();

    a.index();
    let s = a.index_with(Options::new(vec![b_docs.path().to_path_buf()]));
    assert_eq!((s.indexed, s.removed), (1, 0));
    assert_eq!(a.names("Кенгуру"), ["b.txt"]);
    assert_eq!(a.names("Тюльпаны").len(), 2, "документы первого корня остались");

    // удаление файла во втором корне не задевает первый
    fs::remove_file(b_docs.path().join("b.txt")).unwrap();
    let s = a.index_with(Options::new(vec![b_docs.path().to_path_buf()]));
    assert_eq!(s.removed, 1);
    assert_eq!(a.names("Тюльпаны").len(), 2);
}

#[test]
fn excludes_lock_files_and_service_folders_are_skipped() {
    let env = Env::new();
    fs::write(env.root().join("~$contract.docx"), "владелец").unwrap();
    fs::create_dir_all(env.root().join("node_modules/pkg")).unwrap();
    fs::write(env.root().join("node_modules/pkg/readme.txt"), "служебный текст").unwrap();
    fs::create_dir_all(env.root().join("$RECYCLE.BIN")).unwrap();
    fs::write(env.root().join("$RECYCLE.BIN/x.txt"), "корзина").unwrap();
    fs::write(env.root().join("temp.tmp"), "временный").unwrap();
    fs::create_dir_all(env.root().join("backup")).unwrap();
    fs::write(env.root().join("backup/old.txt"), "резервная копия").unwrap();

    let mut opts = Options::new(vec![env.root().to_path_buf()]);
    opts.excludes = vec!["*.tmp".into(), "**/backup/**".into()];
    let s = env.index_with(opts);
    assert_eq!(s.scanned, 7, "только исходные файлы: {s:?}");
    assert!(env.names("служебный").is_empty());
    assert!(env.names("корзина").is_empty());
    assert!(env.names("резервная").is_empty());
    assert!(env.names("name:temp").is_empty());
    assert!(env.names("name:owner").is_empty());
}

#[test]
fn disabling_default_excludes_indexes_service_folders() {
    let env = Env::new();
    fs::create_dir_all(env.root().join("node_modules")).unwrap();
    fs::write(env.root().join("node_modules/readme.txt"), "служебный текст").unwrap();
    let mut opts = Options::new(vec![env.root().to_path_buf()]);
    opts.default_excludes = false;
    env.index_with(opts);
    assert_eq!(env.names("служебный"), ["readme.txt"]);
}

#[test]
fn unreadable_content_still_leaves_the_file_findable_by_name() {
    let env = Env::new();
    fs::write(env.root().join("сломанный отчёт.docx"), b"PK\x03\x04broken").unwrap();
    fs::write(env.root().join("картинка.png"), [0x89, b'P', b'N', b'G', 0, 0, 0]).unwrap();
    let s = env.index();
    assert_eq!(s.failed.len(), 1, "{s:?}");
    assert_eq!(env.names("name:сломанный"), ["сломанный отчёт.docx"]);
    assert_eq!(env.names("name:картинка"), ["картинка.png"]);
    assert_eq!(env.search("name:сломанный").hits[0].status, "error");
}

#[test]
fn cancelled_run_does_not_remove_files_from_index() {
    let env = Env::new();
    env.index();
    fs::remove_file(env.root().join("Договоры/contract.docx")).unwrap();
    let (index, fields) = open_or_create_index(env.index.path()).unwrap();
    let cancel = AtomicBool::new(true);
    let s = indexer::run(&index, &fields, &Options::new(vec![env.root().to_path_buf()]), &Silent, &cancel).unwrap();
    assert!(s.cancelled);
    assert_eq!(s.removed, 0);
    assert_eq!(env.names("Кракозябровая").len(), 3, "запись об удалённом файле пока сохранена");

    let s = env.index();
    assert_eq!(s.removed, 1);
    assert_eq!(env.names("Кракозябровая").len(), 2);
}

#[test]
fn missing_root_is_an_error_and_does_not_touch_the_index() {
    let env = Env::new();
    env.index();
    let (index, fields) = open_or_create_index(env.index.path()).unwrap();
    let opts = Options::new(vec![env.root().join("нет такой папки")]);
    assert!(indexer::run(&index, &fields, &opts, &Silent, &AtomicBool::new(false)).is_err());
    assert_eq!(env.names("Кракозябровая").len(), 3);
}

#[test]
fn stats_describe_the_index() {
    let env = Env::new();
    fs::write(env.root().join("blob.exe"), b"MZ\x90\x00").unwrap();
    env.index();
    let stats = Searcher::open(env.index.path(), false).unwrap().stats(Some(env.index.path())).unwrap();
    assert_eq!(stats.documents, 8);
    assert!(stats.size_on_disk > 0);
    let count = |ext: &str| stats.by_ext.iter().find(|(e, _)| e == ext).map(|(_, n)| *n);
    assert_eq!(count("docx"), Some(1));
    assert_eq!(count("exe"), Some(1));
    let status = |s: &str| stats.by_status.iter().find(|(e, _)| e == s).map(|(_, n)| *n);
    assert_eq!(status("ok"), Some(7));
    assert_eq!(status("skipped"), Some(1));
}

#[test]
fn query_with_only_filters_or_only_negatives_does_not_fail() {
    let env = Env::new();
    env.index();
    assert_eq!(env.search("-Ландыш").total, 5);
    assert_eq!(env.search("").total, 7);
    assert_eq!(env.search("   ").total, 7);
    assert_eq!(env.search("!!!").total, 0, "запрос из знаков препинания не падает (ищется как подстрока имени)");
}

#[test]
fn contains_path_only_knows_indexed_files() {
    let env = Env::new();
    env.index();
    let s = Searcher::open(env.index.path(), false).unwrap();
    let indexed = env.root().join("Договоры/contract.docx");
    assert!(s.contains_path(&indexed.to_string_lossy()));
    assert!(!s.contains_path("/etc/passwd"));
    assert!(!s.contains_path(&env.root().join("Договоры").to_string_lossy()));
}
