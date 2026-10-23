//! Состояние окна и главный цикл: `App` собирает части состояния и связывает их между собой.

use crate::backend::Backend;
use crate::settings::{IndexParams, Settings, Theme, settings_path};
use crate::settings_ui::Tab;
use crate::theme::{self, Kind};
use crate::updater::{self, Updater};
use crate::{settings_ui, views};
use egui::Context;
use file_search::platform;
use file_search::schema::default_index_dir;
use file_search::search::Hit;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Сколько результатов подгружается за раз.
pub const PAGE: usize = 40;

mod actions;
mod form;
mod index;
mod keys;
mod poll;
mod search;

pub use actions::Action;
use actions::DialogResult;
pub use form::Form;
use index::IndexStatus;
use index::discover_places;
use keys::ScreenshotPlan;
use search::SearchState;

const DEBOUNCE: Duration = Duration::from_millis(120);

pub const SEARCH_ID: &str = "search_box";

/// Всё, что приложение узнаёт при запуске из командной строки.
pub struct Launch {
    pub query: String,
    pub index_dir: Option<PathBuf>,
    pub screenshot: Option<PathBuf>,
    /// Только для снимков экрана при проверке: открыть окно настроек (значение — имя вкладки, может быть пустым).
    pub open_settings: Option<String>,
}

/// Поля формы поиска.
#[derive(Clone)]

/// Всплывающее сообщение.
pub struct Toast {
    pub text: String,
    pub kind: Kind,
    pub since: Instant,
}

pub struct App {
    pub settings: Settings,
    pub updater: Updater,
    started: Instant,
    /// Когда в этом сеансе в следующий раз проверять обновления автоматически.
    next_update_check: Instant,
    settings_path: PathBuf,
    pub index_dir: PathBuf,

    /// Повторный запрос из-за обновления индекса: список не должен «прыгать» наверх.
    /// Просмотр содержимого выбранного текстового файла.
    pub preview: Option<crate::preview::Preview>,
    /// Сколько файлов в каждой группе типов (по порядку `theme::GROUPS`) для текущего запроса.
    /// Справа сейчас показана панель подробностей (карточки тогда не дублируют кнопки).
    pub details_visible: bool,

    pub show_settings: bool,
    pub settings_tab: Tab,
    /// Параметры индексации, как они применены сейчас: от них отсчитываются несохранённые изменения.
    baseline: IndexParams,
    /// Узкое окно: вместо списка открыт второй экран с подробностями.
    pub narrow_details: bool,
    /// Номер записи истории, подставленной в строку поиска стрелкой вверх.
    pub focus_search: bool,
    pub toast: Option<Toast>,
    pub places: Vec<(String, PathBuf)>,
    places_rx: Receiver<(String, PathBuf)>,
    dialog_tx: Sender<DialogResult>,
    dialog_rx: Receiver<DialogResult>,
    /// Диалог выбора папки уже открыт: второй не открываем.
    dialog_open: bool,
    /// Адрес сетевой папки, который пользователь вводит вручную, и итог последней проверки.
    pub net_input: String,
    pub net_error: Option<String>,
    pub net_checking: bool,
    toast_tx: Sender<(Kind, String)>,
    toast_rx: Receiver<(Kind, String)>,
    applied_theme: Theme,
    pub excludes_text: String,
    pub confirm_reset: bool,
    /// Номер папки в списке «Где искать», удаление которой ждёт подтверждения.
    pub confirm_remove: Option<usize>,
    /// Шла ли индексация в прошлом кадре: по её окончании показываем итог.
    /// Отметка для оценки оставшегося времени: (когда, сколько было готово).
    screenshot: Option<ScreenshotPlan>,
    pub search: SearchState,
    pub index: IndexStatus,
}

pub(super) fn open_error_text(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "Файл или программа для его открытия не найдены (на Linux нужен xdg-open)".to_string()
    } else {
        format!("Не удалось открыть: {e}")
    }
}

/// Запускает поиск типовых мест для быстрого добавления: «Документы», «Рабочий стол», «Загрузки» и диски.
pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        // Тема применяется сразу при выборе, не дожидаясь «Применить» (которое перезапускает индексацию).
        if self.settings.theme != self.applied_theme {
            theme::apply(ctx, self.settings.theme);
            self.applied_theme = self.settings.theme;
        }
        let mut actions = self.handle_keys(ctx);

        // Папку можно перетащить в окно из проводника — она попадёт в список «Где искать».
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for path in dropped {
            if path.is_dir() {
                actions.push(Action::AddRoot(path));
            } else if let Some(parent) = path.parent() {
                self.toast_err(format!("Это файл, а не папка: перетащите папку «{}»", parent.display()));
            }
        }

        // Набранный запрос уходит в поиск после короткой паузы.
        if let Some(at) = self.search.dirty_at {
            let wait = DEBOUNCE.saturating_sub(at.elapsed());
            if wait.is_zero() {
                self.search.dirty_at = None;
                self.dispatch_search(false);
            } else {
                ctx.request_repaint_after(wait);
            }
        }

        self.auto_check_updates(ctx);
        views::draw(ctx, self, &mut actions);
        settings_ui::show(ctx, self, &mut actions);
        views::toast(ctx, &mut self.toast);

        for action in actions {
            self.handle_action(ctx, action);
        }

        if let Some(backend) = &self.index.backend
            && backend.state.running.load(Ordering::Relaxed)
        {
            // Полоса прогресса не требует частой перерисовки: так окно почти не грузит процессор.
            ctx.request_repaint_after(Duration::from_millis(400));
        }
        self.drive_screenshot(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.form = self.search.form.saved();
        self.remember_query();
        self.save_settings();
        if let Some(backend) = &mut self.index.backend {
            backend.shutdown();
        }
    }
}

impl App {
    pub fn new(ctx: &Context, launch: Launch) -> App {
        theme::install_fonts(ctx);
        let settings_path = settings_path();
        let mut settings = Settings::load(&settings_path);
        if let Some(dir) = &launch.index_dir {
            settings.index_dir = Some(dir.clone());
        }
        theme::apply(ctx, settings.theme);
        let index_dir = settings.index_dir.clone().unwrap_or_else(default_index_dir);
        let settings_theme = settings.theme;
        platform::set_low_priority(settings.load.background());

        let (backend, backend_error) = match Backend::new(ctx.clone(), index_dir.clone()) {
            Ok(b) => (Some(b), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };

        // Поиск мест может зависнуть на недоступном сетевом диске, поэтому делаем это в фоне.
        let (places_tx, places_rx) = channel();
        discover_places(places_tx, ctx.clone());
        let (dialog_tx, dialog_rx) = channel();
        let (toast_tx, toast_rx) = channel();

        let saved_form = settings.form.clone();
        let excludes_text = settings.excludes.join("\n");
        let baseline = settings.index_params(settings.excludes.clone());
        let mut app = App {
            updater: Updater::new(ctx.clone()),
            started: Instant::now(),
            next_update_check: Instant::now() + updater::FIRST_CHECK_AFTER,
            excludes_text,
            baseline,
            settings,
            settings_path,
            index_dir,
            preview: None,
            details_visible: false,
            show_settings: launch.open_settings.is_some(),
            settings_tab: launch.open_settings.as_deref().and_then(Tab::from_name).unwrap_or_default(),
            narrow_details: false,
            focus_search: true,
            toast: None,
            places: Vec::new(),
            places_rx,
            dialog_tx,
            dialog_rx,
            dialog_open: false,
            net_input: String::new(),
            net_error: None,
            net_checking: false,
            toast_tx,
            toast_rx,
            applied_theme: settings_theme,
            confirm_reset: false,
            confirm_remove: None,
            screenshot: launch.screenshot.map(|path| ScreenshotPlan {
                path,
                requested: false,
                started: Instant::now(),
            }),
            search: SearchState {
                form: Form::restored(launch.query.clone(), &saved_form),
                next_id: 0,
                current_id: 0,
                results: Vec::new(),
                total: 0,
                took_ms: 0.0,
                searching: false,
                loading_more: false,
                search_error: None,
                dirty_at: (!launch.query.is_empty()).then(Instant::now),
                silent_refresh: false,
                selected: None,
                facets: None,
                scroll_to_selected: false,
                scroll_to_top: false,
                last_generation: 0,
                history_pos: None,
            },
            index: IndexStatus {
                backend,
                backend_error,
                num_docs: 0,
                index_was_running: false,
                index_mark: None,
                index_eta_secs: None,
            },
        };
        if !app.settings.roots.is_empty() || !app.settings.pending_removals.is_empty() {
            app.start_indexing(false);
        }
        app
    }

    // ------------------------------------------------------------ индексация и настройки
}

#[cfg(test)]
mod tests {
    use file_search::query::Mode;
    use file_search::search::Sort;

    use super::*;

    #[test]
    pub(super) fn empty_form_does_not_search() {
        let mut form = Form::new(String::new());
        assert!(form.is_empty());
        form.query = "   ".into();
        assert!(form.is_empty(), "пробелы — это пустой запрос");
        form.groups[1] = true;
        assert!(!form.is_empty(), "один фильтр по типу уже делает запрос непустым");
    }

    #[test]
    pub(super) fn form_builds_search_options() {
        let mut form = Form::new("договор".into());
        form.groups[0] = true; // Word
        form.groups[3] = true; // PDF
        form.mode = Mode::Content;
        form.sort = Sort::Newest;
        form.dir = Some("D:\\Работа".into());
        let opts = form.options(80, PAGE);
        assert_eq!(opts.query, "договор");
        assert_eq!(opts.offset, 80);
        assert_eq!(opts.limit, PAGE);
        assert_eq!(opts.mode, Mode::Content);
        assert_eq!(opts.sort, Sort::Newest);
        assert_eq!(opts.dirs, ["D:\\Работа"]);
        assert!(opts.exts.contains(&"docx".to_string()) && opts.exts.contains(&"pdf".to_string()));
        assert!(!opts.exts.contains(&"xlsx".to_string()));
    }

    #[test]
    pub(super) fn io_errors_get_friendly_text() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(open_error_text(&missing).contains("программа"));
        let other = std::io::Error::other("boom");
        assert!(open_error_text(&other).contains("boom"));
    }
}
