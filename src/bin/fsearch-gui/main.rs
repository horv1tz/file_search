//! Графическое приложение «Поиск файлов».
//!
//! В релизной сборке под Windows консольного окна нет. Запуск: `fsearch-gui [запрос]`.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod backend;
mod preview;
mod settings;
mod settings_ui;
mod theme;
mod views;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use app::{App, Launch};
use file_search::schema::{default_index_dir, open_or_create_index};
use file_search::watcher::{self, WatchOptions};
use file_search::{indexer, web};
use settings::{Settings, settings_path};

const TITLE: &str = "Поиск файлов";

/// Окно не удалось создать (нет или сломан драйвер видеокарты): пробуем другой способ отрисовки.
const EXIT_WINDOW_FAILED: i32 = 3;
/// Программа уже запущена: второй экземпляр просто закрывается.
const EXIT_ALREADY_RUNNING: i32 = 4;

/// Окно уже создано приложением: сбои после этого — не проблема драйвера, запасной рендерер не поможет.
static APP_CREATED: AtomicBool = AtomicBool::new(false);

/// Как показать окно. Запуск без ключа выбирает способ сам (`choose_mode`); при неудаче открывается поиск в браузере.
/// Способ можно задать вручную: `--renderer=wgpu`, `--renderer=glow` или `--renderer=browser`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Glow,
    Wgpu,
    Browser,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Glow => "glow",
            Mode::Wgpu => "wgpu",
            Mode::Browser => "browser",
        }
    }
}

fn mode_flag(arg: &str) -> Option<Mode> {
    match arg.strip_prefix("--renderer=")? {
        "glow" => Some(Mode::Glow),
        "wgpu" => Some(Mode::Wgpu),
        "browser" => Some(Mode::Browser),
        _ => None,
    }
}

fn parse_args() -> Launch {
    let mut query = Vec::new();
    let mut index_dir = std::env::var_os("FSEARCH_INDEX").map(PathBuf::from);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if mode_flag(&arg).is_some() {
            continue;
        }
        match arg.as_str() {
            "--index-dir" => index_dir = args.next().map(PathBuf::from),
            _ => query.push(arg),
        }
    }
    // Служебные переменные для автоматической проверки окна (снимок экрана).
    Launch {
        query: query.join(" "),
        index_dir,
        screenshot: std::env::var_os("FSEARCH_GUI_SCREENSHOT").map(PathBuf::from),
        open_settings: std::env::var_os("FSEARCH_GUI_SETTINGS").is_some(),
    }
}

fn window_icon() -> Option<Arc<egui::IconData>> {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon.png")).ok().map(Arc::new)
}

fn native_options(mode: Mode) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size([1040.0, 680.0])
        .with_min_inner_size([560.0, 420.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let renderer = if mode == Mode::Wgpu { eframe::Renderer::Wgpu } else { eframe::Renderer::Glow };
    let mut options = eframe::NativeOptions { viewport, renderer, ..Default::default() };
    if mode == Mode::Wgpu && cfg!(windows) {
        // На Windows опрашиваем только Direct3D 12 и Vulkan. Драйверы OpenGL не трогаем совсем: если в системе осталась
        // запись о драйвере видеокарты без самого файла, Windows показывает окно «LoadLibrary failed with error 126».
        use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
        let mut create = WgpuSetupCreateNew::default();
        create.instance_descriptor.backends =
            eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12 | eframe::wgpu::Backends::VULKAN);
        options.wgpu_options = WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(create), ..Default::default() };
    }
    options
}

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("file_search")
}

fn log_path() -> PathBuf {
    config_dir().join("gui-crash.log")
}

fn append_log(text: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // Журнал не должен расти бесконечно: при переполнении начинаем заново.
    if fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024) {
        let _ = fs::remove_file(&path);
    }
    let _ = OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(text.as_bytes()));
}

fn log_line(text: &str) {
    append_log(&format!("[{}] {text}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
}

/// В окне без консоли сообщения о сбоях некуда выводить, поэтому пишем их в файл.
fn install_crash_log() {
    file_search::extract::install_quiet_panic_hook();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !file_search::extract::in_extraction() {
            append_log(&format!("{info}\n{}\n\n", std::backtrace::Backtrace::force_capture()));
        }
        previous(info);
    }));
}

/// Показывает ошибку окном сообщения: в релизной сборке под Windows консоли нет.
fn show_error(text: &str) {
    let _ = rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title(TITLE).set_description(text).show();
}

fn show_info(text: &str) {
    let _ = rfd::MessageDialog::new().set_level(rfd::MessageLevel::Info).set_title(TITLE).set_description(text).show();
}

// ------------------------------------------------------------------ единственный экземпляр

enum Lock {
    /// Замок взят; держим файл открытым до конца работы.
    Held(#[allow(dead_code)] File),
    Busy,
    /// Замок взять не удалось по постороннему поводу (например, нет доступа к папке) — работаем без него.
    Unavailable,
}

/// Два экземпляра с одним индексом мешали бы друг другу (индекс пишет только один), поэтому второй не запускаем.
fn acquire_lock(launch: &Launch) -> Lock {
    // Автоматические проверки и запуск со своим индексом не мешают основному экземпляру.
    if launch.screenshot.is_some() || launch.index_dir.is_some() {
        return Lock::Unavailable;
    }
    let dir = config_dir();
    let _ = fs::create_dir_all(&dir);
    let Ok(file) = OpenOptions::new().create(true).write(true).truncate(false).open(dir.join("fsearch-gui.lock"))
    else {
        return Lock::Unavailable;
    };
    match file.try_lock() {
        Ok(()) => Lock::Held(file),
        Err(fs::TryLockError::WouldBlock) => Lock::Busy,
        Err(_) => Lock::Unavailable,
    }
}

fn already_running() {
    show_info("Программа уже запущена. Найдите её окно на панели задач.");
}

// ------------------------------------------------------------------ окно

fn run_once(mode: Mode, launch: Launch) -> Result<(), String> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        eframe::run_native(
            // Идентификатор для хранилища (размер и положение окна) — латиницей.
            "fsearch-gui",
            native_options(mode),
            Box::new(move |cc| {
                APP_CREATED.store(true, Ordering::SeqCst);
                Ok(Box::new(App::new(&cc.egui_ctx, launch)))
            }),
        )
    }));
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("внутренняя ошибка (подробности в gui-crash.log)".to_string()),
    }
}

/// Показывает окно выбранным способом. Возвращает код выхода процесса.
/// `quiet` — не показывать окно с ошибкой (запасной режим откроется сам).
fn run_window(mode: Mode, quiet: bool) -> i32 {
    let launch = parse_args();
    let _lock = match acquire_lock(&launch) {
        Lock::Held(f) => Some(f),
        Lock::Unavailable => None,
        Lock::Busy => {
            already_running();
            return EXIT_ALREADY_RUNNING;
        }
    };
    // Только для проверки запасного пути в отладочной сборке: FSEARCH_GUI_FAIL=glow,wgpu (ошибка создания окна).
    let simulated = cfg!(debug_assertions)
        && std::env::var("FSEARCH_GUI_FAIL").is_ok_and(|v| v.split(',').any(|m| m.trim() == mode.name()));
    let outcome = if simulated { Err("симуляция сбоя".to_string()) } else { run_once(mode, launch) };
    let Err(error) = outcome else { return 0 };

    log_line(&format!("Окно ({}) не удалось: {error}", mode.name()));
    if APP_CREATED.load(Ordering::SeqCst) {
        show_error(&format!(
            "Программа завершилась с ошибкой:\n{error}\n\nПодробности записаны в {}",
            log_path().display()
        ));
        return 1;
    }
    if !quiet {
        show_error(&format!(
            "Не удалось открыть окно программы:\n{error}\n\nПодробности записаны в {}",
            log_path().display()
        ));
    }
    EXIT_WINDOW_FAILED
}

/// Выбирает способ отрисовки. На Windows окно рисуется через Direct3D 12 или Vulkan, если есть подходящий
/// видеоадаптер (драйвер OpenGL при этом не трогается совсем), иначе через OpenGL. На остальных системах — OpenGL.
fn choose_mode() -> Mode {
    if cfg!(windows) && gpu_available() { Mode::Wgpu } else { Mode::Glow }
}

/// Спрашивает у системы, есть ли видеоадаптер для Direct3D 12 / Vulkan. Окно при этом не создаётся.
#[cfg(windows)]
fn gpu_available() -> bool {
    use eframe::wgpu::{Backends, Instance, InstanceDescriptor, RequestAdapterOptions};
    let backends = Backends::from_env().unwrap_or(Backends::DX12 | Backends::VULKAN);
    let instance = Instance::new(&InstanceDescriptor { backends, ..Default::default() });
    pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).is_ok()
}

#[cfg(not(windows))]
fn gpu_available() -> bool {
    false
}

/// Обычный запуск: окно выбранным способом; если открыть его не удалось — поиск в браузере.
fn auto() -> ! {
    let mode = choose_mode();
    log_line(&format!("Способ отрисовки: {}", mode.name()));
    match run_window(mode, true) {
        0 | EXIT_ALREADY_RUNNING => std::process::exit(0),
        EXIT_WINDOW_FAILED => browser_mode("окно не удалось создать"),
        code => std::process::exit(code),
    }
}

// ------------------------------------------------------------------ запасной режим: поиск в браузере

fn free_port() -> Option<u16> {
    (7878u16..7900).find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
}

/// Окно открыть не удалось ни одним способом: индексируем и следим за папками в фоне, а интерфейс поиска
/// показываем в браузере (тот же сервер, что и `fsearch serve`).
fn browser_mode(reason: &str) -> ! {
    log_line(&format!("Запасной режим (браузер). Причины: {reason}"));
    let launch = parse_args();
    let _lock = match acquire_lock(&launch) {
        Lock::Held(f) => Some(f),
        Lock::Unavailable => None,
        Lock::Busy => {
            already_running();
            std::process::exit(0);
        }
    };

    let settings_file = settings_path();
    let mut settings = Settings::load(&settings_file);
    if settings.roots.is_empty() {
        show_info(
            "Окно программы открыть не удалось (подробности в gui-crash.log), поэтому поиск откроется в браузере.\n\n\
             Сейчас выберите папку или диск, по которым нужно искать.",
        );
        let Some(dir) = rfd::FileDialog::new().set_title("Папка для поиска").pick_folder() else {
            std::process::exit(0);
        };
        settings.add_root(dir);
        let _ = settings.save(&settings_file);
    }
    let index_dir = launch.index_dir.or_else(|| settings.index_dir.clone()).unwrap_or_else(default_index_dir);
    let (index, fields) = match open_or_create_index(&index_dir) {
        Ok(x) => x,
        Err(e) => {
            log_line(&format!("Не удалось открыть индекс: {e:#}"));
            show_error(&format!("Не удалось открыть индекс {}:\n{e:#}", index_dir.display()));
            std::process::exit(1);
        }
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    {
        let (cancel, done) = (cancel.clone(), done.clone());
        let mut opts = WatchOptions::new(backend::scan_options(&settings, settings.roots.clone(), &index_dir));
        opts.rescan_every = Duration::from_secs(settings.rescan_minutes.max(1) * 60);
        std::thread::spawn(move || {
            if let Err(e) = watcher::watch(&index, &fields, &opts, &indexer::Silent, &|t: &str| log_line(t), &cancel) {
                log_line(&format!("Индексация остановлена из-за ошибки: {e:#}"));
            }
            done.store(true, Ordering::SeqCst);
        });
    }

    let Some(port) = free_port() else {
        show_error("Не удалось запустить поиск в браузере: заняты все порты 7878–7899.");
        std::process::exit(1);
    };
    {
        let dir = index_dir.clone();
        std::thread::spawn(move || {
            if let Err(e) = web::serve(&dir, port, true) {
                log_line(&format!("Сервер поиска остановлен: {e:#}"));
                show_error(&format!("Не удалось запустить поиск в браузере:\n{e:#}"));
                std::process::exit(1);
            }
        });
    }

    let shown = Instant::now();
    show_info(&format!(
        "Поиск открыт в браузере: http://127.0.0.1:{port}/\n\nИндексация идёт в фоне. \
         Нажмите «ОК», чтобы закончить работу и закрыть программу."
    ));
    // Если диалог не смог открыться (нет графической оболочки), он возвращается сразу: тогда просто работаем,
    // пока процесс не остановят (Ctrl+C или диспетчер задач).
    if shown.elapsed() < Duration::from_millis(700) {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    cancel.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        if done.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::process::exit(0);
}

fn main() {
    install_crash_log();
    match std::env::args().skip(1).find_map(|a| mode_flag(&a)) {
        Some(Mode::Browser) => browser_mode("запрошен режим браузера"),
        Some(mode) => std::process::exit(run_window(mode, false)),
        None => auto(),
    }
}
