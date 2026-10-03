//! Состояние окна и главный цикл.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Context, Id, Key, Modifiers};
use file_search::netpath;
use file_search::platform;
use file_search::query::Mode;
use file_search::schema::default_index_dir;
use file_search::search::{Hit, SearchOptions, Sort};

use crate::backend::{Backend, Reply, SearchRequest};
use crate::settings::{Settings, Theme, settings_path};
use crate::theme::{self, GROUPS};
use crate::{settings_ui, views};

/// Сколько результатов подгружается за раз.
pub const PAGE: usize = 40;
const DEBOUNCE: Duration = Duration::from_millis(120);
pub const SEARCH_ID: &str = "search_box";

/// Всё, что приложение узнаёт при запуске из командной строки.
pub struct Launch {
    pub query: String,
    pub index_dir: Option<PathBuf>,
    pub screenshot: Option<PathBuf>,
    /// Только для снимков экрана при проверке: открыть окно настроек.
    pub open_settings: bool,
}

/// Поля формы поиска.
#[derive(Clone)]
pub struct Form {
    pub query: String,
    pub groups: [bool; GROUPS.len()],
    pub mode: Mode,
    pub sort: Sort,
    /// Искать только внутри этой папки.
    pub dir: Option<String>,
}

impl Form {
    fn new(query: String) -> Form {
        Form { query, groups: [false; GROUPS.len()], mode: Mode::All, sort: Sort::Relevance, dir: None }
    }

    pub fn is_empty(&self) -> bool {
        self.query.trim().is_empty() && !self.groups.iter().any(|g| *g) && self.dir.is_none()
    }

    pub fn options(&self, offset: usize, limit: usize) -> SearchOptions {
        let exts = GROUPS
            .iter()
            .zip(&self.groups)
            .filter(|(_, on)| **on)
            .flat_map(|((_, exts), _)| exts.iter().map(|e| e.to_string()))
            .collect();
        SearchOptions {
            query: self.query.clone(),
            mode: self.mode,
            exts,
            dirs: self.dir.iter().cloned().collect(),
            limit,
            offset,
            sort: self.sort,
            snippets: true,
            ..SearchOptions::default()
        }
    }
}

/// Что пользователь попросил сделать в интерфейсе; выполняется после отрисовки кадра.
pub enum Action {
    Select(usize),
    Open(usize),
    Reveal(usize),
    CopyPath(usize),
    CopyName(usize),
    SearchInFolder(usize),
    UseExample(String),
    AddRoot(PathBuf),
    /// Добавить папку по введённому адресу (например, `\\192.168.1.10\документы`).
    AddRootText(String),
    PickRoot,
    PickFilterFolder,
    RemoveRoot(usize),
    OpenSettings,
    CloseSettings,
    ApplySettings {
        force: bool,
    },
    StopIndexing,
    ClearDirFilter,
    RecreateIndex,
    OpenIndexFolder,
}

pub struct App {
    pub backend: Option<Backend>,
    pub backend_error: Option<String>,
    pub settings: Settings,
    settings_path: PathBuf,
    pub index_dir: PathBuf,

    pub form: Form,
    next_id: u64,
    current_id: u64,
    pub results: Vec<Hit>,
    pub total: usize,
    pub took_ms: f64,
    pub searching: bool,
    loading_more: bool,
    pub search_error: Option<String>,
    dirty_at: Option<Instant>,
    /// Повторный запрос из-за обновления индекса: список не должен «прыгать» наверх.
    silent_refresh: bool,
    pub selected: Option<usize>,
    pub scroll_to_selected: bool,
    pub scroll_to_top: bool,
    last_generation: u64,
    pub num_docs: u64,

    pub show_settings: bool,
    pub focus_search: bool,
    toast: Option<(String, Instant)>,
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
    toast_tx: Sender<String>,
    toast_rx: Receiver<String>,
    applied_theme: Theme,
    pub excludes_text: String,
    pub confirm_reset: bool,

    screenshot: Option<ScreenshotPlan>,
}

enum DialogResult {
    Root(PathBuf),
    FilterFolder(PathBuf),
    /// Диалог закрыт (выбрали папку или отказались).
    Closed,
    /// Введённая сетевая папка отвечает.
    NetRoot(PathBuf),
    /// Введённую сетевую папку открыть не удалось: пояснение для пользователя.
    NetFailed(String),
}

struct ScreenshotPlan {
    path: PathBuf,
    requested: bool,
    started: Instant,
}

fn open_error_text(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "Файл или программа для его открытия не найдены (на Linux нужен xdg-open)".to_string()
    } else {
        format!("Не удалось открыть: {e}")
    }
}

/// Запускает поиск типовых мест для быстрого добавления: «Документы», «Рабочий стол», «Загрузки» и диски.
/// Каждый диск проверяется в своём потоке: отключённый сетевой диск отвечает секундами и не должен
/// задерживать остальные кнопки.
fn discover_places(tx: Sender<(String, PathBuf)>, ctx: Context) {
    let send = move |tx: &Sender<(String, PathBuf)>, ctx: &Context, item: (String, PathBuf)| {
        let _ = tx.send(item);
        ctx.request_repaint();
    };
    if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let root = PathBuf::from(format!("{}:\\", letter as char));
                if root.exists() {
                    send(&tx, &ctx, (format!("Диск {}:", letter as char), root));
                }
            });
        }
    } else {
        send(&tx, &ctx, ("Корень /".to_string(), PathBuf::from("/")));
    }
    std::thread::spawn(move || {
        for (name, dir) in [
            ("Документы", dirs::document_dir()),
            ("Рабочий стол", dirs::desktop_dir()),
            ("Загрузки", dirs::download_dir()),
            ("Домашняя папка", dirs::home_dir()),
        ] {
            if let Some(dir) = dir.filter(|d| d.is_dir()) {
                send(&tx, &ctx, (name.to_string(), dir));
            }
        }
    });
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

        let (backend, backend_error) = match Backend::new(ctx.clone(), index_dir.clone()) {
            Ok(b) => (Some(b), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };

        // Поиск мест может зависнуть на недоступном сетевом диске, поэтому делаем это в фоне.
        let (places_tx, places_rx) = channel();
        discover_places(places_tx, ctx.clone());
        let (dialog_tx, dialog_rx) = channel();
        let (toast_tx, toast_rx) = channel();

        let mut app = App {
            backend,
            backend_error,
            excludes_text: settings.excludes.join("\n"),
            settings,
            settings_path,
            index_dir,
            form: Form::new(launch.query.clone()),
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
            scroll_to_selected: false,
            scroll_to_top: false,
            last_generation: 0,
            num_docs: 0,
            show_settings: launch.open_settings,
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
            screenshot: launch.screenshot.map(|path| ScreenshotPlan {
                path,
                requested: false,
                started: Instant::now(),
            }),
        };
        if !app.settings.roots.is_empty() || !app.settings.pending_removals.is_empty() {
            app.start_indexing(false);
        }
        app
    }

    // ------------------------------------------------------------ индексация и настройки

    fn start_indexing(&mut self, force: bool) {
        if let Some(backend) = &mut self.backend {
            backend.start_indexing(&self.settings, force);
        }
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.settings_path) {
            self.toast(format!("Не удалось сохранить настройки: {e}"));
        }
    }

    /// Применяет настройки: сохраняет и перезапускает индексацию.
    fn apply_settings(&mut self, ctx: &Context, force: bool) {
        self.settings.excludes =
            self.excludes_text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
        theme::apply(ctx, self.settings.theme);
        self.applied_theme = self.settings.theme;
        self.save_settings();
        self.start_indexing(force);
        self.mark_dirty(true);
    }

    fn recreate_index(&mut self, ctx: &Context) {
        // Удаляем только каталог, который действительно является индексом программы: путь берётся из настроек,
        // и ошибка в нём не должна стоить пользователю случайной папки.
        let is_index = self.index_dir.join("fsearch.version").exists() || self.index_dir.join("meta.json").exists();
        if !is_index {
            self.toast(format!("{} не похож на индекс программы — удалять не буду", self.index_dir.display()));
            return;
        }
        if let Some(backend) = &mut self.backend
            && !backend.shutdown()
        {
            self.toast("Индексация ещё не остановилась — подождите несколько секунд и повторите");
            return;
        }
        self.backend = None;
        if let Err(e) = std::fs::remove_dir_all(&self.index_dir)
            && self.index_dir.exists()
        {
            self.backend_error = Some(format!("Не удалось удалить индекс: {e}"));
            return;
        }
        match Backend::new(ctx.clone(), self.index_dir.clone()) {
            Ok(b) => {
                self.backend = Some(b);
                self.backend_error = None;
                self.results.clear();
                self.total = 0;
                self.num_docs = 0;
                self.last_generation = 0;
                self.start_indexing(true);
                self.toast("Индекс пересоздан");
            }
            Err(e) => self.backend_error = Some(format!("{e:#}")),
        }
    }

    // ------------------------------------------------------------ поиск

    /// Новый запрос набран, но ответа на него ещё нет: список на экране относится к прежнему запросу.
    pub fn pending(&self) -> bool {
        self.searching || self.dirty_at.is_some()
    }

    /// Запросить поиск после короткой паузы в наборе.
    pub fn mark_dirty(&mut self, silent: bool) {
        self.dirty_at = Some(Instant::now());
        self.silent_refresh = silent && !self.results.is_empty();
    }

    fn dispatch_search(&mut self, append: bool) {
        let Some(backend) = &self.backend else { return };
        if self.form.is_empty() {
            self.results.clear();
            self.total = 0;
            self.searching = false;
            self.search_error = None;
            self.selected = None;
            self.current_id = self.next_id + 1;
            self.next_id += 1;
            return;
        }
        if append {
            if self.loading_more {
                return;
            }
            self.loading_more = true;
        } else {
            self.next_id += 1;
            self.current_id = self.next_id;
            self.loading_more = false;
            self.searching = true;
        }
        let offset = if append { self.results.len() } else { 0 };
        // Фоновое обновление не должно сворачивать уже подгруженный список обратно до первой страницы.
        let limit = if !append && self.silent_refresh { self.results.len().clamp(PAGE, 500) } else { PAGE };
        backend.search(SearchRequest { id: self.current_id, opts: self.form.options(offset, limit), append });
    }

    fn poll(&mut self, ctx: &Context) {
        while let Ok(place) = self.places_rx.try_recv() {
            if !self.places.iter().any(|(_, p)| *p == place.1) {
                self.places.push(place);
                self.places.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }
        while let Ok(text) = self.toast_rx.try_recv() {
            self.toast(text);
        }
        while let Ok(done) = self.dialog_rx.try_recv() {
            match done {
                DialogResult::Root(path) => self.handle_action(ctx, Action::AddRoot(path)),
                DialogResult::FilterFolder(path) => {
                    self.form.dir = Some(path.to_string_lossy().into_owned());
                    self.mark_dirty(false);
                }
                DialogResult::Closed => {
                    self.dialog_open = false;
                    self.focus_search = true;
                }
                DialogResult::NetRoot(path) => {
                    self.net_checking = false;
                    self.net_error = None;
                    self.net_input.clear();
                    self.handle_action(ctx, Action::AddRoot(path));
                }
                DialogResult::NetFailed(text) => {
                    self.net_checking = false;
                    self.net_error = Some(text);
                }
            }
        }
        // Папки, записи которых уже удалены из индекса, больше не нужно помнить.
        let removed_done: Vec<PathBuf> = self
            .backend
            .as_ref()
            .map(|b| std::mem::take(&mut *b.state.removed_done.lock().unwrap()))
            .unwrap_or_default();
        if !removed_done.is_empty() {
            self.settings.pending_removals.retain(|r| !removed_done.contains(r));
            self.save_settings();
        }
        let Some(backend) = &self.backend else { return };
        while let Ok(reply) = backend.replies.try_recv() {
            match reply {
                Reply::Count(n) => self.num_docs = n,
                Reply::Search(resp) => {
                    if resp.id != self.current_id {
                        continue;
                    }
                    if resp.append {
                        self.loading_more = false;
                    } else {
                        self.searching = false;
                    }
                    match resp.result {
                        Ok(result) => {
                            self.search_error = None;
                            self.total = result.total;
                            self.took_ms = result.took.as_secs_f64() * 1000.0;
                            if resp.append {
                                self.results.extend(result.hits);
                            } else {
                                self.results = result.hits;
                                if self.silent_refresh {
                                    self.selected = self.selected.filter(|i| *i < self.results.len());
                                } else {
                                    self.selected = None;
                                    self.scroll_to_top = true;
                                }
                                self.silent_refresh = false;
                            }
                        }
                        Err(message) => {
                            self.search_error = Some(message);
                            self.results.clear();
                            self.total = 0;
                        }
                    }
                }
            }
        }
        // Индекс обновился (закончилась индексация, пришли изменения с диска) — обновим выдачу.
        let generation = backend.state.generation.load(Ordering::SeqCst);
        if generation != self.last_generation {
            self.last_generation = generation;
            backend.request_count();
            if !self.form.is_empty() {
                self.mark_dirty(true);
            }
        }
    }

    // ------------------------------------------------------------ действия

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    fn hit_path(&self, index: usize) -> Option<PathBuf> {
        self.results.get(index).map(|h| PathBuf::from(&h.path))
    }

    fn pick_folder(&mut self, ctx: &Context, for_filter: bool) {
        if self.dialog_open {
            return;
        }
        self.dialog_open = true;
        let (tx, ctx) = (self.dialog_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let title =
                if for_filter { "Искать только в папке" } else { "Папка для поиска" };
            if let Some(path) = rfd::FileDialog::new().set_title(title).pick_folder() {
                let _ = tx.send(if for_filter { DialogResult::FilterFolder(path) } else { DialogResult::Root(path) });
            }
            let _ = tx.send(DialogResult::Closed);
            ctx.request_repaint();
        });
    }

    /// Открывает файл (или показывает его в проводнике) в фоне: проверка пути на недоступном сетевом диске
    /// может занять секунды и не должна замораживать окно.
    fn open_in_background(&self, ctx: &Context, path: PathBuf, reveal: bool) {
        let (tx, ctx) = (self.toast_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let problem = if !path.exists() {
                Some("Файла уже нет на диске: его удалили или переместили".to_string())
            } else if !reveal && !platform::is_safe_to_open(&path) {
                Some("Исполняемые файлы не запускаются отсюда — используйте «В папке»".to_string())
            } else {
                let result = if reveal { platform::reveal_path(&path) } else { platform::open_path(&path) };
                result.err().map(|e| open_error_text(&e))
            };
            if let Some(text) = problem {
                let _ = tx.send(text);
                ctx.request_repaint();
            }
        });
    }

    pub fn handle_action(&mut self, ctx: &Context, action: Action) {
        match action {
            Action::Select(i) => self.selected = Some(i),
            Action::Open(i) => {
                if let Some(path) = self.hit_path(i) {
                    self.open_in_background(ctx, path, false);
                }
            }
            Action::Reveal(i) => {
                if let Some(path) = self.hit_path(i) {
                    self.open_in_background(ctx, path, true);
                }
            }
            Action::CopyPath(i) => {
                if let Some(h) = self.results.get(i) {
                    ctx.copy_text(h.path.clone());
                    self.toast("Путь скопирован");
                }
            }
            Action::CopyName(i) => {
                if let Some(h) = self.results.get(i) {
                    ctx.copy_text(h.name.clone());
                    self.toast("Имя скопировано");
                }
            }
            Action::SearchInFolder(i) => {
                if let Some(dir) = self.hit_path(i).and_then(|p| p.parent().map(Path::to_path_buf)) {
                    self.form.dir = Some(dir.to_string_lossy().into_owned());
                    self.mark_dirty(false);
                }
            }
            Action::ClearDirFilter => {
                self.form.dir = None;
                self.mark_dirty(false);
            }
            Action::UseExample(text) => {
                self.form.query = text;
                self.focus_search = true;
                self.mark_dirty(false);
            }
            Action::AddRoot(path) => {
                if self.settings.add_root(path) {
                    self.apply_settings(ctx, false);
                    self.toast("Папка добавлена, идёт индексация");
                } else {
                    self.toast("Эта папка уже входит в список");
                }
            }
            Action::PickRoot => self.pick_folder(ctx, false),
            Action::PickFilterFolder => self.pick_folder(ctx, true),
            Action::RemoveRoot(i) => {
                if self.settings.remove_root(i).is_some() {
                    self.apply_settings(ctx, false);
                }
            }
            Action::OpenSettings => self.show_settings = true,
            Action::AddRootText(text) => {
                if self.net_checking {
                    return;
                }
                match netpath::parse_user_path(&text) {
                    Err(message) => self.net_error = Some(message),
                    Ok(path) => {
                        self.net_error = None;
                        self.net_checking = true;
                        let (tx, ctx) = (self.dialog_tx.clone(), ctx.clone());
                        // Выключенный компьютер отвечает не сразу — проверяем в фоне, окно не замирает.
                        std::thread::spawn(move || {
                            let result = match netpath::check_dir(&path, netpath::NETWORK_TIMEOUT) {
                                Ok(()) => DialogResult::NetRoot(path),
                                Err(text) => DialogResult::NetFailed(text),
                            };
                            let _ = tx.send(result);
                            ctx.request_repaint();
                        });
                    }
                }
            }
            Action::CloseSettings => {
                self.show_settings = false;
                self.confirm_reset = false;
                self.focus_search = true;
            }
            Action::ApplySettings { force } => self.apply_settings(ctx, force),
            Action::StopIndexing => {
                if let Some(backend) = &mut self.backend {
                    backend.stop_indexing();
                }
            }
            Action::RecreateIndex => {
                self.confirm_reset = false;
                self.recreate_index(ctx);
            }
            Action::OpenIndexFolder => {
                let _ = platform::open_path(&self.index_dir);
            }
        }
    }

    // ------------------------------------------------------------ клавиатура

    fn handle_keys(&mut self, ctx: &Context) -> Vec<Action> {
        let mut actions = Vec::new();
        let search_id = Id::new(SEARCH_ID);
        let typing_elsewhere = ctx.memory(|m| m.focused()).is_some_and(|id| id != search_id);

        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F) || i.consume_key(Modifiers::COMMAND, Key::L)) {
            self.focus_search = true;
        }
        if typing_elsewhere || self.show_settings || egui::Popup::is_any_open(ctx) {
            return actions;
        }

        // Стрелки, Enter и Esc обрабатываем до того, как поле ввода успеет забрать их себе.
        let (mut delta, mut reveal, mut open, mut escape) = (0isize, false, false, false);
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                delta += 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                delta -= 1;
            }
            if i.consume_key(Modifiers::NONE, Key::PageDown) {
                delta += 8;
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp) {
                delta -= 8;
            }
            reveal = i.consume_key(Modifiers::COMMAND, Key::Enter) || i.consume_key(Modifiers::SHIFT, Key::Enter);
            open = i.consume_key(Modifiers::NONE, Key::Enter);
            escape = i.consume_key(Modifiers::NONE, Key::Escape);
        });

        if delta != 0 && !self.results.is_empty() {
            let last = self.results.len() as isize - 1;
            let next = match self.selected {
                None => 0,
                Some(i) => (i as isize + delta).clamp(0, last),
            };
            self.selected = Some(next as usize);
            self.scroll_to_selected = true;
            // Дошли до конца загруженного — подгружаем следующую страницу.
            if next == last {
                self.load_more();
            }
        }
        if !self.results.is_empty() && !self.pending() {
            let target = self.selected.unwrap_or(0);
            if reveal {
                actions.push(Action::Reveal(target));
            } else if open {
                actions.push(Action::Open(target));
            }
        }
        if escape && !self.form.query.is_empty() {
            self.form.query.clear();
            self.mark_dirty(false);
        }
        actions
    }

    // ------------------------------------------------------------ снимок экрана (для проверки)

    fn drive_screenshot(&mut self, ctx: &Context) {
        let Some(plan) = &mut self.screenshot else { return };
        // Снимок делаем, когда индексация закончилась и выдача обновилась.
        let settled = self.backend.as_ref().is_none_or(|b| {
            !b.state.running.load(Ordering::Relaxed)
                && b.state.generation.load(Ordering::SeqCst) == self.last_generation
        });
        let ready = plan.started.elapsed() > Duration::from_millis(2500)
            && settled
            && !self.searching
            && self.dirty_at.is_none();
        if !plan.requested && (ready || plan.started.elapsed() > Duration::from_secs(10)) {
            plan.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let path = plan.path.clone();
            let _ = image::save_buffer(
                &path,
                image.as_raw(),
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }
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
                self.toast(format!("Это файл, а не папка: перетащите папку «{}»", parent.display()));
            }
        }

        // Набранный запрос уходит в поиск после короткой паузы.
        if let Some(at) = self.dirty_at {
            let wait = DEBOUNCE.saturating_sub(at.elapsed());
            if wait.is_zero() {
                self.dirty_at = None;
                self.dispatch_search(false);
            } else {
                ctx.request_repaint_after(wait);
            }
        }

        views::draw(ctx, self, &mut actions);
        settings_ui::show(ctx, self, &mut actions);
        views::toast(ctx, &mut self.toast);

        for action in actions {
            self.handle_action(ctx, action);
        }

        if let Some(backend) = &self.backend
            && backend.state.running.load(Ordering::Relaxed)
        {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        self.drive_screenshot(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
        if let Some(backend) = &mut self.backend {
            backend.shutdown();
        }
    }
}

impl App {
    /// Подгрузить следующую страницу, когда пользователь докрутил до конца списка.
    pub fn load_more(&mut self) {
        // Пока идёт новый запрос, подгружать к старому списку нечего: ответы перепутались бы.
        if self.results.len() < self.total && !self.pending() {
            self.dispatch_search(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_form_does_not_search() {
        let mut form = Form::new(String::new());
        assert!(form.is_empty());
        form.query = "   ".into();
        assert!(form.is_empty(), "пробелы — это пустой запрос");
        form.groups[1] = true;
        assert!(!form.is_empty(), "один фильтр по типу уже делает запрос непустым");
    }

    #[test]
    fn form_builds_search_options() {
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
    fn io_errors_get_friendly_text() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(open_error_text(&missing).contains("программа"));
        let other = std::io::Error::other("boom");
        assert!(open_error_text(&other).contains("boom"));
    }
}
