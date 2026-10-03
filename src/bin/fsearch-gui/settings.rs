//! Настройки приложения: папки для поиска, слежение, исключения, тема.

use std::fs;
use std::path::{Path, PathBuf};

use file_search::schema::path_covers;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum Theme {
    /// Как в системе.
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
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
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            roots: Vec::new(),
            watch: true,
            excludes: Vec::new(),
            default_excludes: true,
            max_text_mb: 16,
            theme: Theme::Auto,
            index_dir: None,
            rescan_minutes: 30,
            pending_removals: Vec::new(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("file_search").join("gui-settings.json")
}

impl Settings {
    /// Читает настройки; при отсутствии или порче файла возвращает значения по умолчанию.
    pub fn load(path: &Path) -> Settings {
        fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
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
    fn nested_roots_are_merged() {
        let mut s = Settings::default();
        assert!(s.add_root(PathBuf::from("/data/docs")));
        assert!(!s.add_root(PathBuf::from("/data/docs/2024")), "вложенная папка уже покрыта");
        assert!(!s.add_root(PathBuf::from("/data/docs")));
        assert!(s.add_root(PathBuf::from("/data")), "объемлющая папка заменяет вложенные");
        assert_eq!(s.roots, [PathBuf::from("/data")]);
    }
}
