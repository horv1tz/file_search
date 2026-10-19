//! Обновления в окне: проверка и загрузка идут в фоновых потоках, окно только показывает состояние.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use file_search::update::{Client, Release};

/// Как часто проверять обновления, когда автопроверка включена.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Первая проверка — не сразу при запуске, чтобы не мешать старту индексации.
pub const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);

pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        done: u64,
        total: u64,
    },
    /// Файл загружен и проверен; осталось перезапустить программу.
    Ready {
        release: Release,
        file: PathBuf,
    },
    Failed {
        message: String,
        release: Option<Release>,
    },
}

impl Phase {
    pub fn release(&self) -> Option<&Release> {
        match self {
            Phase::Available(r) | Phase::Downloading { release: r, .. } | Phase::Ready { release: r, .. } => Some(r),
            Phase::Failed { release, .. } => release.as_ref(),
            Phase::Idle | Phase::Checking | Phase::UpToDate => None,
        }
    }
}

enum Msg {
    Checked { result: Result<Option<Release>, String>, manual: bool },
    Progress(u64, u64),
    Downloaded(Result<PathBuf, String>),
}

/// Что произошло при опросе фоновых потоков и что нужно сделать окну.
pub enum Event {
    /// Проверка завершилась (успешно или нет): время стоит запомнить, чтобы не проверять слишком часто.
    Checked,
}

pub struct Updater {
    pub phase: Phase,
    /// Полоса с предложением обновиться скрыта кнопкой «Позже» (до следующего запуска программы).
    pub dismissed: bool,
    /// Когда закончилась последняя проверка в этом сеансе.
    pub checked_at: Option<Instant>,
    /// Последняя проверка была запущена вручную — её результат показывается и в настройках.
    pub manual: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    busy: bool,
}

impl Updater {
    pub fn new(ctx: egui::Context) -> Updater {
        let (tx, rx) = channel();
        Updater { phase: Phase::Idle, dismissed: false, checked_at: None, manual: false, tx, rx, ctx, busy: false }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Запускает проверку в фоне. `manual` — по кнопке «Проверить сейчас».
    pub fn check(&mut self, manual: bool) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.manual = manual;
        self.phase = Phase::Checking;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let result = Client::new().check().map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Checked { result, manual });
            ctx.request_repaint();
        });
    }

    /// Загружает найденное обновление (если для этой системы есть готовый файл).
    pub fn download(&mut self) {
        if self.busy {
            return;
        }
        let Some(release) = self.phase.release().cloned() else { return };
        if release.installable_asset().is_none() {
            return;
        }
        self.busy = true;
        self.dismissed = false;
        self.phase = Phase::Downloading { release: release.clone(), done: 0, total: 0 };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let last = std::sync::Mutex::new(Instant::now());
            let progress = |done: u64, total: u64| {
                // Не чаще нескольких раз в секунду: иначе окно перерисовывалось бы на каждый кусок файла.
                let mut t = last.lock().unwrap();
                if t.elapsed() >= Duration::from_millis(150) || done == total {
                    *t = Instant::now();
                    let _ = tx.send(Msg::Progress(done, total));
                    ctx.request_repaint();
                }
            };
            let result = Client::new().download_update(&release, &progress).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Downloaded(result));
            ctx.request_repaint();
        });
    }

    /// Забирает результаты фоновых потоков. `skipped` — версия, которую пользователь просил не предлагать.
    pub fn poll(&mut self, skipped: Option<&str>) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Checked { result, manual } => {
                    self.busy = false;
                    self.checked_at = Some(Instant::now());
                    events.push(Event::Checked);
                    self.phase = match result {
                        Ok(Some(release)) => {
                            let skip = !manual && skipped == Some(release.version.to_string().as_str());
                            if skip { Phase::UpToDate } else { Phase::Available(release) }
                        }
                        Ok(None) => Phase::UpToDate,
                        Err(message) => Phase::Failed { message, release: None },
                    };
                }
                Msg::Progress(done, total) => {
                    if let Phase::Downloading { done: d, total: t, .. } = &mut self.phase {
                        *d = done;
                        *t = total;
                    }
                }
                Msg::Downloaded(result) => {
                    self.busy = false;
                    let release = self.phase.release().cloned();
                    self.phase = match (result, release) {
                        (Ok(file), Some(release)) => Phase::Ready { release, file },
                        (Ok(_), None) => Phase::Idle,
                        (Err(message), release) => Phase::Failed { message, release },
                    };
                }
            }
        }
        events
    }

    /// Загруженный файл поставлен на место, программа будет перезапущена: больше ничего не предлагаем.
    pub fn installed(&mut self) {
        self.phase = Phase::Idle;
        self.dismissed = true;
    }
}
