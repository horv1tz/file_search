use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use file_search::extract::{self, Limits, UNIT_SEP};
use file_search::indexer::{self, Progress, Summary};
use file_search::platform::{format_size, parse_date_ms, parse_size};
use file_search::query::Mode;
use file_search::schema::{default_index_dir, index_exists, open_or_create_index};
use file_search::search::{SearchOptions, Searcher, Sort};
use file_search::watcher::{self, WatchOptions};
use file_search::{interactive, render, web};

#[derive(Parser)]
#[command(
    name = "fsearch",
    version,
    about = "Полнотекстовый поиск по файлам: по именам и по содержимому (Word, Excel, PowerPoint, PDF, текст)",
    long_about = None
)]
struct Cli {
    /// Каталог, где хранится индекс
    #[arg(long, global = true, env = "FSEARCH_INDEX", value_name = "ПАПКА")]
    index_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Проиндексировать папки или диски (повторный запуск обновляет только изменившееся)
    Index(IndexArgs),
    /// Найти файлы
    Search(SearchArgs),
    /// Интерактивный поиск в терминале
    #[command(visible_alias = "i")]
    Interactive(FilterArgs),
    /// Следить за папками и обновлять индекс при изменении файлов (работает до Ctrl+C)
    Watch(WatchArgs),
    /// Запустить веб-интерфейс на этом компьютере
    Serve(ServeArgs),
    /// Показать состояние индекса
    Stats,
    /// Удалить индекс целиком
    Reset {
        /// Не спрашивать подтверждение
        #[arg(short, long)]
        yes: bool,
    },
    /// Показать, какой текст извлекается из файла (для диагностики)
    Extract {
        file: PathBuf,
        /// Показать весь текст, а не первые 3000 символов
        #[arg(long)]
        full: bool,
    },
}

/// Параметры обхода и чтения файлов, общие для `index`, `watch` и `serve --watch`.
#[derive(Args, Clone)]
struct ScanArgs {
    /// Исключить файлы/папки по шаблону, например '*.tmp' или '**/backup/**' (можно несколько раз)
    #[arg(short = 'x', long = "exclude", value_name = "ШАБЛОН")]
    excludes: Vec<String>,
    /// Не пропускать служебные папки ($RECYCLE.BIN, .git, node_modules…)
    #[arg(long)]
    no_default_excludes: bool,
    /// Число потоков (по умолчанию — по числу ядер)
    #[arg(short = 'j', long)]
    threads: Option<usize>,
    /// Максимум текста из одного файла, МБ
    #[arg(long, default_value_t = 16, value_name = "МБ")]
    max_text_mb: usize,
    /// Файлы больше этого размера индексируются только по имени, МБ
    #[arg(long, default_value_t = 512, value_name = "МБ")]
    max_file_mb: u64,
}

impl ScanArgs {
    fn options(&self, paths: Vec<PathBuf>) -> indexer::Options {
        let mut opts = indexer::Options::new(paths);
        opts.excludes = self.excludes.clone();
        opts.default_excludes = !self.no_default_excludes;
        if let Some(t) = self.threads {
            opts.threads = t.max(1);
        }
        opts.limits =
            Limits { max_text_bytes: self.max_text_mb.max(1) << 20, max_file_size: self.max_file_mb.max(1) << 20 };
        opts
    }
}

#[derive(Args)]
struct IndexArgs {
    /// Папки и диски, например: D:\ или /home/user/docs
    #[arg(required = true, value_name = "ПУТЬ")]
    paths: Vec<PathBuf>,
    #[command(flatten)]
    scan: ScanArgs,
    /// Переиндексировать всё, даже неизменившиеся файлы
    #[arg(long)]
    force: bool,
    /// Показать полный список файлов, которые не удалось прочитать
    #[arg(long)]
    errors: bool,
}

#[derive(Args)]
struct WatchArgs {
    /// Папки и диски, за которыми следить, например: D:\
    #[arg(required = true, value_name = "ПУТЬ")]
    paths: Vec<PathBuf>,
    #[command(flatten)]
    scan: ScanArgs,
    /// Как часто делать полную проверку на случай пропущенных событий, минут
    #[arg(long, default_value_t = 30, value_name = "МИНУТ")]
    rescan_minutes: u64,
}

#[derive(Clone, Copy, ValueEnum)]
enum SortArg {
    /// По релевантности
    Relevance,
    /// Сначала новые
    Newest,
    /// Сначала старые
    Oldest,
    /// Сначала большие
    Largest,
    /// Сначала маленькие
    Smallest,
}

#[derive(Args, Clone)]
struct FilterArgs {
    /// Только эти типы файлов, через запятую: docx,xlsx,pdf
    #[arg(short, long, value_delimiter = ',', value_name = "ТИПЫ")]
    ext: Vec<String>,
    /// Только внутри этой папки (можно несколько раз)
    #[arg(short = 'i', long = "in", value_name = "ПАПКА")]
    dirs: Vec<String>,
    /// Искать только по именам файлов
    #[arg(long, conflicts_with = "content")]
    name: bool,
    /// Искать только по содержимому
    #[arg(long)]
    content: bool,
    /// Размер не меньше (10K, 5M, 1G)
    #[arg(long, value_name = "РАЗМЕР")]
    min_size: Option<String>,
    /// Размер не больше
    #[arg(long, value_name = "РАЗМЕР")]
    max_size: Option<String>,
    /// Изменён не раньше даты (2024-03-15 или 15.03.2024)
    #[arg(long, value_name = "ДАТА")]
    after: Option<String>,
    /// Изменён не позже даты
    #[arg(long, value_name = "ДАТА")]
    before: Option<String>,
    /// Сколько результатов показать
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,
}

#[derive(Args)]
struct SearchArgs {
    /// Запрос: слова, "точная фраза", -исключить, преф*, слово~, a OR b, name:…, ext:…
    #[arg(value_name = "ЗАПРОС")]
    query: Vec<String>,
    #[command(flatten)]
    filters: FilterArgs,
    /// Пропустить первые N результатов
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Порядок результатов
    #[arg(long, value_enum, default_value_t = SortArg::Relevance)]
    sort: SortArg,
    /// Не показывать фрагменты текста
    #[arg(long)]
    no_snippets: bool,
    /// Вывести только пути, по одному в строке
    #[arg(long, conflicts_with = "json")]
    paths: bool,
    /// Вывести результат в формате JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ServeArgs {
    /// Порт
    #[arg(short, long, default_value_t = 7878)]
    port: u16,
    /// Открыть страницу в браузере
    #[arg(long)]
    open: bool,
    /// Заодно индексировать и отслеживать эти папки (можно несколько раз)
    #[arg(short, long = "watch", value_name = "ПУТЬ")]
    watch: Vec<PathBuf>,
    #[command(flatten)]
    scan: ScanArgs,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e:#}", style("Ошибка:").red().bold());
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let dir = cli.index_dir.unwrap_or_else(default_index_dir);
    match cli.command {
        Command::Index(a) => cmd_index(&dir, a),
        Command::Search(a) => cmd_search(&dir, a),
        Command::Interactive(f) => {
            let searcher = Searcher::open(&dir, false)?;
            interactive::run(&searcher, search_options(&f, String::new())?)
        }
        Command::Watch(a) => cmd_watch(&dir, a),
        Command::Serve(a) => cmd_serve(&dir, a),
        Command::Stats => cmd_stats(&dir),
        Command::Reset { yes } => cmd_reset(&dir, yes),
        Command::Extract { file, full } => cmd_extract(&file, full),
    }
}

// ------------------------------------------------------------------ index

struct Bar {
    pb: ProgressBar,
}

impl Bar {
    fn new() -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(ProgressStyle::with_template("{spinner} {msg}").unwrap());
        pb.enable_steady_tick(Duration::from_millis(100));
        Bar { pb }
    }
}

impl Progress for Bar {
    fn scanning(&self, files: u64) {
        self.pb.set_message(format!("Обход папок: найдено файлов — {files}"));
    }

    fn indexing_started(&self, total: u64) {
        self.pb.set_style(
            ProgressStyle::with_template(
                "{bar:40.cyan/blue} {pos}/{len} ({percent}%) · {per_sec} · осталось {eta} · {msg}",
            )
            .unwrap()
            .progress_chars("━╸─"),
        );
        self.pb.set_length(total);
        self.pb.set_position(0);
    }

    fn file_done(&self, path: &Path, _status: &extract::Status) {
        self.pb.inc(1);
        let name: String = path.file_name().map(|n| n.to_string_lossy().chars().take(40).collect()).unwrap_or_default();
        self.pb.set_message(name);
    }

    fn message(&self, text: &str) {
        self.pb.set_message(text.to_string());
    }
}

/// Первое Ctrl+C просит работу корректно завершиться, второе — выходит сразу.
fn install_ctrlc() -> Result<Arc<AtomicBool>> {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    ctrlc::set_handler(move || {
        if flag.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        eprintln!("\nОстанавливаюсь и сохраняю сделанное… (повторное Ctrl+C — выход сразу)");
    })
    .context("не удалось установить обработчик Ctrl+C")?;
    Ok(cancel)
}

fn cmd_watch(dir: &Path, a: WatchArgs) -> Result<()> {
    extract::install_quiet_panic_hook();
    let (index, fields) = open_or_create_index(dir)?;
    let cancel = install_ctrlc()?;
    let mut opts = WatchOptions::new(a.scan.options(a.paths));
    opts.rescan_every = Duration::from_secs(a.rescan_minutes.max(1) * 60);
    println!("Индекс: {}\nСлежение до Ctrl+C.", dir.display());
    watcher::watch(&index, &fields, &opts, &watcher::log_line, &cancel)?;
    println!("Слежение остановлено.");
    Ok(())
}

fn cmd_serve(dir: &Path, a: ServeArgs) -> Result<()> {
    if !a.watch.is_empty() {
        extract::install_quiet_panic_hook();
        let (index, fields) = open_or_create_index(dir)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let opts = WatchOptions::new(a.scan.options(a.watch.clone()));
        {
            let (cancel, done) = (cancel.clone(), done.clone());
            std::thread::spawn(move || {
                if let Err(e) = watcher::watch(&index, &fields, &opts, &watcher::log_line, &cancel) {
                    eprintln!("{} {e:#}", style("Слежение остановлено из-за ошибки:").red());
                }
                done.store(true, Ordering::SeqCst);
            });
        }
        // Сервер блокирует основной поток, поэтому завершаем процесс из обработчика Ctrl+C,
        // дав слежению закончить текущую пачку и сохранить индекс.
        ctrlc::set_handler(move || {
            cancel.store(true, Ordering::SeqCst);
            for _ in 0..100 {
                if done.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            std::process::exit(0);
        })
        .context("не удалось установить обработчик Ctrl+C")?;
    }
    web::serve(dir, a.port, a.open)
}

fn cmd_index(dir: &Path, a: IndexArgs) -> Result<()> {
    extract::install_quiet_panic_hook();
    let (index, fields) = open_or_create_index(dir)?;

    let mut opts = a.scan.options(a.paths);
    opts.force = a.force;

    let cancel = install_ctrlc()?;

    println!("Индекс: {}", dir.display());
    let bar = Bar::new();
    let result = indexer::run(&index, &fields, &opts, &bar, &cancel);
    bar.pb.finish_and_clear();
    let summary = result?;
    print_summary(&summary, a.errors);
    Ok(())
}

fn print_summary(s: &Summary, all_errors: bool) {
    let secs = s.elapsed.as_secs_f64();
    let head = if s.cancelled { "Прервано" } else { "Готово" };
    println!("{} за {:.1} с", style(head).green().bold(), secs);
    println!("  Файлов найдено:            {}", s.scanned);
    println!("  Добавлено или обновлено:   {}", s.indexed);
    println!("  Без изменений:             {}", s.unchanged);
    println!("  Удалено из индекса:        {}", s.removed);
    println!("  Извлечено текста:          {}", format_size(s.text_bytes));
    if s.encrypted > 0 {
        println!("  Защищены паролем:          {} (индексируются только по имени)", s.encrypted);
    }
    if s.truncated > 0 {
        println!("  Обрезаны по лимиту текста: {} (см. --max-text-mb)", s.truncated);
    }
    if s.failed_total > 0 {
        println!("  {} {}", style("Не удалось прочитать:     ").yellow(), s.failed_total);
    }
    if s.walk_errors_total > 0 {
        println!(
            "  {} {} (нет доступа к папкам/файлам)",
            style("Ошибок обхода:            ").yellow(),
            s.walk_errors_total
        );
    }
    let shown = if all_errors { s.failed.len() } else { 10.min(s.failed.len()) };
    if shown > 0 {
        println!("\nНе прочитаны (имя файла всё равно доступно для поиска):");
        for (path, why) in s.failed.iter().take(shown) {
            println!("  {} — {}", path.display(), why);
        }
        if shown < s.failed_total as usize {
            println!("  … и ещё {}. Полный список: добавьте --errors", s.failed_total as usize - shown);
        }
    }
    if all_errors {
        for e in &s.walk_errors {
            println!("  {e}");
        }
    }
    if s.cancelled {
        println!("\nЗапустите команду ещё раз — индексация продолжится с места остановки.");
    }
}

// ------------------------------------------------------------------ search

fn search_options(f: &FilterArgs, query: String) -> Result<SearchOptions> {
    let size = |v: &Option<String>, what: &str| -> Result<Option<u64>> {
        v.as_deref().map(|s| parse_size(s).with_context(|| format!("не понимаю размер для {what}: {s}"))).transpose()
    };
    let date = |v: &Option<String>, what: &str, end: bool| -> Result<Option<u64>> {
        v.as_deref()
            .map(|s| {
                parse_date_ms(s, end)
                    .with_context(|| format!("не понимаю дату для {what}: {s} (нужно ГГГГ-ММ-ДД или ДД.ММ.ГГГГ)"))
            })
            .transpose()
    };
    Ok(SearchOptions {
        query,
        mode: if f.name {
            Mode::Name
        } else if f.content {
            Mode::Content
        } else {
            Mode::All
        },
        exts: f.ext.clone(),
        dirs: f.dirs.clone(),
        min_size: size(&f.min_size, "--min-size")?,
        max_size: size(&f.max_size, "--max-size")?,
        modified_after: date(&f.after, "--after", false)?,
        modified_before: date(&f.before, "--before", true)?,
        limit: f.limit,
        ..SearchOptions::default()
    })
}

fn cmd_search(dir: &Path, a: SearchArgs) -> Result<()> {
    let query = a.query.join(" ");
    let mut opts = search_options(&a.filters, query)?;
    opts.offset = a.offset;
    opts.sort = match a.sort {
        SortArg::Relevance => Sort::Relevance,
        SortArg::Newest => Sort::Newest,
        SortArg::Oldest => Sort::Oldest,
        SortArg::Largest => Sort::Largest,
        SortArg::Smallest => Sort::Smallest,
    };
    opts.snippets = !a.no_snippets && !a.paths;

    let no_filters = opts.exts.is_empty()
        && opts.dirs.is_empty()
        && opts.min_size.is_none()
        && opts.max_size.is_none()
        && opts.modified_after.is_none()
        && opts.modified_before.is_none();
    if opts.query.trim().is_empty() && no_filters {
        bail!("пустой запрос. Пример: fsearch search договор аренды");
    }

    let searcher = Searcher::open(dir, false)?;
    let result = searcher.search(&opts)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if a.paths {
        let mut out = io::stdout().lock();
        for hit in &result.hits {
            writeln!(out, "{}", hit.path)?;
        }
    } else {
        render::print_result(&result, &opts);
    }
    Ok(())
}

// ------------------------------------------------------------------ stats / reset / extract

fn cmd_stats(dir: &Path) -> Result<()> {
    let searcher = Searcher::open(dir, false)?;
    let s = searcher.stats(Some(dir))?;
    println!("Индекс:        {}", dir.display());
    println!("Файлов:        {}", s.documents);
    println!("Размер на диске: {}", format_size(s.size_on_disk));
    println!("Сегментов:     {}", s.segments);
    println!("\nПо типам:");
    for (ext, n) in s.by_ext.iter().take(20) {
        println!("  {:<10} {}", if ext.is_empty() { "(без типа)" } else { ext }, n);
    }
    if s.by_ext.len() > 20 {
        println!("  … и ещё {} типов", s.by_ext.len() - 20);
    }
    println!("\nСостояние содержимого:");
    for (status, n) in &s.by_status {
        let label = match status.as_str() {
            "ok" => "прочитано",
            "empty" => "нет текста",
            "encrypted" => "защищено паролем",
            "skipped" => "не читалось (бинарные, слишком большие)",
            "error" => "ошибка чтения",
            other => other,
        };
        println!("  {label:<40} {n}");
    }
    Ok(())
}

fn cmd_reset(dir: &Path, yes: bool) -> Result<()> {
    if !index_exists(dir) {
        println!("Индекса в {} нет.", dir.display());
        return Ok(());
    }
    if !yes {
        print!("Удалить индекс {}? [y/N] ", dir.display());
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes" | "д" | "да") {
            println!("Отменено.");
            return Ok(());
        }
    }
    std::fs::remove_dir_all(dir).with_context(|| format!("не удалось удалить {}", dir.display()))?;
    println!("Индекс удалён.");
    Ok(())
}

fn cmd_extract(file: &Path, full: bool) -> Result<()> {
    extract::install_quiet_panic_hook();
    let size = std::fs::metadata(file).with_context(|| format!("не удалось прочитать {}", file.display()))?.len();
    let e = extract::extract_file(file, size, &Limits::default());
    println!("Файл:      {}", file.display());
    println!("Состояние: {}", e.status.as_string());
    if !e.units.is_empty() {
        println!("Части:     {}", e.units.join(", "));
    }
    if !e.meta.is_empty() {
        println!("Метаданные: {}", e.meta.replace('\n', " | "));
    }
    let text = e.text.replace(UNIT_SEP, "\n──────── \n");
    let shown: String = if full { text.clone() } else { text.chars().take(3000).collect() };
    println!(
        "Текст ({} симв.{}):\n{shown}",
        e.text.chars().count(),
        if e.truncated { ", обрезан по лимиту" } else { "" }
    );
    if !full && text.chars().count() > 3000 {
        println!("… (--full — показать всё)");
    }
    Ok(())
}
