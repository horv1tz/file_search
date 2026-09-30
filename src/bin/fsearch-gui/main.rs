//! Графическое приложение «Поиск файлов».
//!
//! В релизной сборке под Windows консольного окна нет. Запуск: `fsearch-gui [запрос]`.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod backend;
mod settings;
mod settings_ui;
mod theme;
mod views;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use app::{App, Launch};

const TITLE: &str = "Поиск файлов";

/// Окно уже создано приложением: сбои после этого — не проблема драйвера, запасной рендерер не поможет.
static APP_CREATED: AtomicBool = AtomicBool::new(false);

fn renderer_flag(arg: &str) -> Option<eframe::Renderer> {
    match arg.strip_prefix("--renderer=")? {
        "glow" => Some(eframe::Renderer::Glow),
        "wgpu" => Some(eframe::Renderer::Wgpu),
        _ => None,
    }
}

fn parse_args() -> Launch {
    let mut query = Vec::new();
    let mut index_dir = std::env::var_os("FSEARCH_INDEX").map(PathBuf::from);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if renderer_flag(&arg).is_some() {
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

fn native_options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size([1000.0, 740.0])
        .with_min_inner_size([560.0, 420.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    eframe::NativeOptions { viewport, renderer, ..Default::default() }
}

fn log_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("file_search").join("gui-crash.log")
}

/// В окне без консоли сообщения о сбоях некуда выводить, поэтому пишем их в файл.
fn install_crash_log() {
    file_search::extract::install_quiet_panic_hook();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !file_search::extract::in_extraction() {
            let path = log_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let text = format!("{info}\n{}\n\n", std::backtrace::Backtrace::force_capture());
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        }
        previous(info);
    }));
}

/// Показывает ошибку окном сообщения: в релизной сборке под Windows консоли нет.
fn show_error(text: &str) {
    let _ = rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title(TITLE).set_description(text).show();
}

fn run_once(renderer: eframe::Renderer) -> Result<(), String> {
    let launch = parse_args();
    let result = catch_unwind(AssertUnwindSafe(|| {
        eframe::run_native(
            // Идентификатор для хранилища (размер и положение окна) — латиницей.
            "fsearch-gui",
            native_options(renderer),
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

fn main() {
    install_crash_log();

    // Рендерер можно задать явно: --renderer=glow или --renderer=wgpu.
    let forced = std::env::args().skip(1).find_map(|a| renderer_flag(&a));
    let first = forced.unwrap_or(eframe::Renderer::Glow);
    // Только для проверки запасного пути в отладочной сборке.
    let simulate_failure =
        cfg!(debug_assertions) && forced.is_none() && std::env::var_os("FSEARCH_GUI_FAIL_FIRST").is_some();
    let outcome =
        if simulate_failure { Err("симуляция сбоя OpenGL".to_string()) } else { run_once(first) };
    let Err(error) = outcome else { return };

    // Сначала OpenGL (быстрый старт). Если драйвера нет (удалённый рабочий стол, виртуальная машина),
    // пробуем wgpu, но новым процессом: winit не позволяет создать окно второй раз в том же.
    if forced.is_none() && !APP_CREATED.load(Ordering::SeqCst) {
        eprintln!("OpenGL недоступен ({error}), пробую wgpu…");
        let relaunch = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe).args(std::env::args().skip(1)).arg("--renderer=wgpu").status()
        });
        match relaunch {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => show_error(&format!(
                "Не удалось открыть окно программы:\n{error}\n\nЗапасной запуск тоже не удался: {e}"
            )),
        }
    } else {
        show_error(&format!(
            "Программа завершилась с ошибкой:\n{error}\n\nПодробности записаны в {}",
            log_path().display()
        ));
    }
    std::process::exit(1);
}
