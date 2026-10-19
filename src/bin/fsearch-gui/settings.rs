//! Настройки приложения: папки для поиска, слежение, исключения, тема.

use std::fs;
use std::path::{Path, PathBuf};

use file_search::schema::path_covers;
use file_search::watcher::DEFAULT_RESCAN_MINUTES;
use serde::{Deserialize, Serialize};

/// Версия формата настроек: при обновлении программы значения, выставленные старыми умолчаниями, переводятся на новые.
const SETTINGS_VERSION: u32 = 2;

fn legacy_settings_version() -> u32 {
    0
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum Theme {
    /// Как в системе.
    #[default]
    Auto,
    Light,
    Dark,
}

/// Сколько ресурсов компьютера можно тратить на индексацию.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum Load {
    /// Один поток в фоновом режиме: почти незаметно, но дольше.
    Minimal,
    /// Половина ядер в фоновом режиме.
    #[default]
    Moderate,
    /// Все ядра, кроме одного, с обычным приоритетом: быстрее всего.
    Maximum,
}

impl Load {
    /// Сколько потоков отдать индексации при `cores` ядрах.
    pub fn threads(self, cores: usize) -> usize {
        match self {
            Load::Minimal => 1,
            Load::Moderate => (cores / 2).max(1),
            Load::Maximum => cores.saturating_sub(1).max(1),
        }
    }

    /// Работать с пониженным приоритетом.
    pub fn background(self) -> bool {
        self != Load::Maximum
    }
}

/// Режим, сортировка и фильтры типов — как их оставили в прошлый раз.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SavedForm {
    /// `all` | `name` | `content`.
    pub mode: String,
    /// `relevance` | `newest` | `oldest` | `largest` | `smallest`.
    pub sort: String,
    /// Включённые фильтры типов (по порядку `theme::GROUPS`).
    pub groups: Vec<bool>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Версия формата (см. [`Settings::migrate`]); в файле без этого поля — 0.
    #[serde(default = "legacy_settings_version")]
    pub settings_version: u32,
    /// Нагрузка на компьютер при индексации.
    pub load: Load,
    /// Папки и диски, по которым ищем.
    pub roots: Vec<PathBuf>,
    /// Следить за изменениями и обновлять индекс автоматически.
    pub watch: bool,
    /// Шаблоны исключений (glob), по одному на элемент.
    pub excludes: Vec<String>,
    pub default_excludes: bool,
    pub max_text_mb: usize,
    pub theme: Theme,
    /// Каталог индекса; по умолчанию общий с консольной версией.
    pub index_dir: Option<PathBuf>,
    /// Период полной проверки в режиме слежения, минут.
    pub rescan_minutes: u64,
    /// Папки, убранные из списка, записи которых ещё не удалены из индекса (удаление повторяется при запуске).
    pub pending_removals: Vec<PathBuf>,
    /// Проверять, не вышла ли новая версия (раз в сутки, при запуске).
    pub check_updates: bool,
    /// Когда проверяли в последний раз, секунды с 1970 года.
    pub last_update_check: u64,
    /// Версия, которую пользователь просил больше не предлагать.
    pub skipped_version: Option<String>,
    /// Распознавать текст на сканах (PDF-картинки), если в системе есть распознавание.
    pub ocr: bool,
    /// Состояние формы поиска на момент выхода.
    pub form: SavedForm,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            settings_version: SETTINGS_VERSION,
            load: Load::default(),
            roots: Vec::new(),
            watch: true,
            excludes: Vec::new(),
            default_excludes: true,
            max_text_mb: 16,
            theme: Theme::Auto,
            index_dir: None,
            rescan_minutes: DEFAULT_RESCAN_MINUTES,
            pending_removals: Vec::new(),
            check_updates: true,
            last_update_check: 0,
            skipped_version: None,
            ocr: true,
            form: SavedForm::default(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("file_search").join("gui-settings.json")
}

impl Settings {
    /// Читает настройки; при отсутствии или порче файла возвращает значения по умолчанию.
    pub fn load(path: &Path) -> Settings {
        let mut settings: Settings =
            fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        settings.migrate();
        settings
    }

    /// Переводит настройки старых версий на текущие умолчания там, где пользователь их не менял.
    fn migrate(&mut self) {
        if self.settings_version < 2 && self.rescan_minutes == 30 {
            // Раньше полная проверка шла каждые 30 минут и заметно нагружала диск.
            self.rescan_minutes = DEFAULT_RESCAN_MINUTES;
        }
        self.settings_version = SETTINGS_VERSION;
    }

    /// Сохраняет через временный файл, чтобы сбой посреди записи не испортил настройки.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        fs::rename(&tmp, path)
    }

    /// Добавляет папку, если её (или содержащей её папки) ещё нет; вложенные в неё убирает.
    /// Возвращает `true`, если список изменился.
    pub fn add_root(&mut self, root: PathBuf) -> bool {
        if self.roots.iter().any(|r| path_covers(r, &root)) {
            return false;
        }
        // Папка, вернувшаяся в список, больше не ждёт удаления из индекса.
        self.pending_removals.retain(|r| !path_covers(r, &root) && !path_covers(&root, r));
        self.roots.retain(|r| !path_covers(&root, r));
        self.roots.push(root);
        true
    }

    /// Убирает папку из списка поиска; её записи удалятся из индекса при следующей индексации.
    pub fn remove_root(&mut self, index: usize) -> Option<PathBuf> {
        if index >= self.roots.len() {
            return None;
        }
        let root = self.roots.remove(index);
        if !self.pending_removals.contains(&root) {
            self.pending_removals.push(root.clone());
        }
        Some(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/settings.json");
        assert_eq!(Settings::load(&path), Settings::default());

        let s = Settings { roots: vec![PathBuf::from("/a")], watch: false, theme: Theme::Dark, ..Settings::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);

        fs::write(&path, "{ битый json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        fs::write(&path, r#"{"watch": false}"#).unwrap();
        let partial = Settings::load(&path);
        assert!(
            !partial.watch && partial.default_excludes && partial.max_text_mb == 16,
            "новые поля берут значения по умолчанию"
        );
    }

    #[test]
    fn old_rescan_default_is_migrated_but_custom_values_stay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"rescan_minutes": 30}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.rescan_minutes, DEFAULT_RESCAN_MINUTES, "старое умолчание переводится на новое");
        assert_eq!(s.settings_version, SETTINGS_VERSION);
        fs::write(&path, r#"{"rescan_minutes": 15}"#).unwrap();
        assert_eq!(Settings::load(&path).rescan_minutes, 15, "выставленное вручную не трогаем");
        fs::write(&path, r#"{"settings_version": 2, "rescan_minutes": 30}"#).unwrap();
        assert_eq!(Settings::load(&path).rescan_minutes, 30, "в новом формате 30 — осознанный выбор");
    }

    #[test]
    fn removed_roots_wait_for_index_cleanup_until_readded() {
        let mut s = Settings::default();
        s.add_root(PathBuf::from("/data/a"));
        s.add_root(PathBuf::from("/data/b"));
        assert_eq!(s.remove_root(0), Some(PathBuf::from("/data/a")));
        assert_eq!(s.pending_removals, [PathBuf::from("/data/a")]);
        assert_eq!(s.remove_root(9), None);
        assert!(s.add_root(PathBuf::from("/data/a/sub")), "папку (или её часть) вернули — удаление отменяется");
        assert!(s.pending_removals.is_empty());
    }

    #[test]
    fn load_levels_pick_threads_and_priority() {
        assert_eq!([Load::Minimal, Load::Moderate, Load::Maximum].map(|l| l.threads(8)), [1, 4, 7]);
        assert_eq!([Load::Minimal, Load::Moderate, Load::Maximum].map(|l| l.threads(1)), [1, 1, 1]);
        assert!(Load::Minimal.background() && Load::Moderate.background() && !Load::Maximum.background());
    }

    #[test]
    fn nested_roots_are_merged() {
        let mut s = Settings::default();
        assert!(s.add_root(PathBuf::from("/data/docs")));
        assert!(!s.add_root(PathBuf::from("/data/docs/2024")), "вложенная папка уже покрыта");
        assert!(!s.add_root(PathBuf::from("/data/docs")));
        assert!(s.add_root(PathBuf::from("/data")), "объемлющая папка заменяет вложенные");
        assert_eq!(s.roots, [PathBuf::from("/data")]);
    }
}
