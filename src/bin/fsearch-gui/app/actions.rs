//! Действия пользователя: всё, что меняет состояние по нажатию кнопки или пункта меню.

use super::*;
use crate::theme::{GROUPS, Kind};
use crate::updater::{self, Phase};
use egui::Context;
use file_search::netpath;
use file_search::platform;
use file_search::query::Mode;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Что пользователь попросил сделать в интерфейсе; выполняется после отрисовки кадра.
pub enum Action {
    Select(usize),
    Open(usize),
    Reveal(usize),
    CopyPath(usize),
    CopyName(usize),
    SearchInFolder(usize),
    /// Подставить запрос (пример или из истории) и искать.
    UseQuery(String),
    ClearHistory,
    /// Сбросить фильтры типов, режим и папку.
    ResetFilters,
    /// Узкое окно: показать подробности выбранного файла вторым экраном.
    ShowDetails(usize),
    BackToList,
    OpenLogFolder,
    /// Скопировать текст и показать сообщение с этой подписью.
    CopyText(String, &'static str),
    AddRoot(PathBuf),
    /// Добавить папку по введённому адресу (например, `\\192.168.1.10\документы`).
    AddRootText(String),
    PickRoot,
    PickFilterFolder,
    RemoveRoot(usize),
    /// Спросить подтверждение перед удалением папки из поиска.
    ConfirmRemoveRoot(usize),
    CancelRemoveRoot,
    OpenSettings,
    CloseSettings,
    ApplySettings {
        force: bool,
    },
    StopIndexing,
    ClearDirFilter,
    RecreateIndex,
    /// Спросить подтверждение перед пересозданием индекса / отказаться от него.
    AskRecreate,
    CancelRecreate,
    OpenIndexFolder,
    /// Сохранить настройки, не перезапуская индексацию (для параметров, которые на неё не влияют).
    SaveSettings,
    /// Проверить обновления по кнопке.
    CheckUpdates,
    /// Загрузить найденное обновление (или открыть страницу выпуска там, где само не ставится).
    DownloadUpdate,
    /// Поставить загруженное обновление и перезапустить программу.
    InstallUpdate,
    /// Скрыть предложение обновиться до следующего запуска.
    DismissUpdate,
    /// Больше не предлагать эту версию.
    SkipUpdateVersion,
    OpenUrl(String),
}

pub(super) enum DialogResult {
    Root(PathBuf),
    FilterFolder(PathBuf),
    /// Диалог закрыт (выбрали папку или отказались).
    Closed,
    /// Введённая сетевая папка отвечает.
    NetRoot(PathBuf),
    /// Введённую сетевую папку открыть не удалось: пояснение для пользователя.
    NetFailed(String),
}

impl App {
    pub(super) fn pick_folder(&mut self, ctx: &Context, for_filter: bool) {
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
    pub(super) fn open_in_background(&self, ctx: &Context, path: PathBuf, reveal: bool) {
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
                let _ = tx.send((Kind::Error, text));
                ctx.request_repaint();
            }
        });
    }

    pub fn handle_action(&mut self, ctx: &Context, action: Action) {
        match action {
            Action::Select(i) => {
                self.selected = Some(i);
                self.remember_query();
            }
            Action::Open(i) => {
                self.remember_query();
                if let Some(path) = self.hit_path(i) {
                    self.open_in_background(ctx, path, false);
                }
            }
            Action::Reveal(i) => {
                self.remember_query();
                if let Some(path) = self.hit_path(i) {
                    self.open_in_background(ctx, path, true);
                }
            }
            Action::CopyPath(i) => {
                if let Some(h) = self.results.get(i) {
                    ctx.copy_text(h.path.clone());
                    self.toast_ok("Путь скопирован");
                }
            }
            Action::CopyName(i) => {
                if let Some(h) = self.results.get(i) {
                    ctx.copy_text(h.name.clone());
                    self.toast_ok("Имя скопировано");
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
            Action::UseQuery(text) => {
                self.form.query = text;
                self.focus_search = true;
                self.mark_dirty(false);
            }
            Action::ClearHistory => {
                self.settings.history.clear();
                self.save_settings();
            }
            Action::ResetFilters => {
                self.form.groups = [false; GROUPS.len()];
                self.form.mode = Mode::All;
                self.form.dir = None;
                self.focus_search = true;
                self.mark_dirty(false);
            }
            Action::ShowDetails(i) => {
                self.selected = Some(i);
                self.narrow_details = true;
            }
            Action::BackToList => {
                self.narrow_details = false;
                self.focus_search = true;
            }
            Action::OpenLogFolder => {
                let _ = platform::open_path(&crate::config_dir());
            }
            Action::CopyText(text, label) => {
                ctx.copy_text(text);
                self.toast_ok(label);
            }
            Action::AddRoot(path) => {
                if self.settings.add_root(path) {
                    self.apply_settings(ctx, false);
                    self.toast_ok("Папка добавлена, идёт индексация");
                } else {
                    self.toast("Эта папка уже входит в список");
                }
            }
            Action::PickRoot => self.pick_folder(ctx, false),
            Action::PickFilterFolder => self.pick_folder(ctx, true),
            Action::RemoveRoot(i) => {
                self.confirm_remove = None;
                if let Some(root) = self.settings.remove_root(i) {
                    self.apply_settings(ctx, false);
                    self.toast(format!("Папка «{}» убрана из поиска", root.display()));
                }
            }
            Action::ConfirmRemoveRoot(i) => self.confirm_remove = Some(i),
            Action::CancelRemoveRoot => self.confirm_remove = None,
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
            Action::CloseSettings => self.close_settings(ctx),
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
            Action::AskRecreate => self.confirm_reset = true,
            Action::CancelRecreate => self.confirm_reset = false,
            Action::OpenIndexFolder => {
                let _ = platform::open_path(&self.index_dir);
            }
            Action::SaveSettings => self.save_settings(),
            Action::CheckUpdates => self.updater.check(true),
            Action::DownloadUpdate => {
                let page = self.updater.phase.release().map(|r| r.url.clone());
                if self.updater.phase.release().is_some_and(|r| r.installable_asset().is_some()) {
                    self.updater.download();
                } else if let Some(url) = page {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
            Action::InstallUpdate => self.install_update(ctx),
            Action::DismissUpdate => self.updater.dismissed = true,
            Action::SkipUpdateVersion => {
                if let Some(release) = self.updater.phase.release() {
                    self.settings.skipped_version = Some(release.version.to_string());
                    self.save_settings();
                }
                self.updater.dismissed = true;
            }
            Action::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
        }
    }

    /// Ставит загруженное обновление на место и закрывает окно; новая версия запускается после закрытия.
    pub(super) fn install_update(&mut self, ctx: &Context) {
        let Phase::Ready { file, release } = &self.updater.phase else { return };
        let (file, release) = (file.clone(), release.clone());
        match file_search::update::install_self(&file) {
            Ok(exe) => {
                self.updater.installed();
                self.save_settings();
                crate::request_relaunch(exe);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&file);
                self.updater.phase = Phase::Failed { message: format!("{e:#}"), release: Some(release) };
            }
        }
    }

    /// Автоматическая проверка обновлений: через несколько секунд после запуска и дальше раз в сутки,
    /// если с прошлой проверки (в том числе в прошлых сеансах) прошли сутки.
    pub(super) fn auto_check_updates(&mut self, ctx: &Context) {
        if !self.settings.check_updates || self.started.elapsed() < updater::FIRST_CHECK_AFTER {
            return;
        }
        if Instant::now() < self.next_update_check {
            return;
        }
        self.next_update_check = Instant::now() + updater::CHECK_EVERY;
        let since_last = unix_now().saturating_sub(self.settings.last_update_check);
        if since_last >= updater::CHECK_EVERY.as_secs() && matches!(self.updater.phase, Phase::Idle | Phase::UpToDate) {
            self.updater.check(false);
        }
        ctx.request_repaint_after(updater::CHECK_EVERY);
    }

    // ------------------------------------------------------------ клавиатура
}
