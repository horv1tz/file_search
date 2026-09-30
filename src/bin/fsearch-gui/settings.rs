//! Настройки приложения: папки для поиска, слежение, исключения, тема.

use std::fs;
use std::path::{Path, PathBuf};

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
        if self.roots.iter().any(|r| root.starts_with(r)) {
            return false;
        }
        self.roots.retain(|r| !r.starts_with(&root));
        self.roots.push(root);
        true
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
    fn nested_roots_are_merged() {
        let mut s = Settings::default();
        assert!(s.add_root(PathBuf::from("/data/docs")));
        assert!(!s.add_root(PathBuf::from("/data/docs/2024")), "вложенная папка уже покрыта");
        assert!(!s.add_root(PathBuf::from("/data/docs")));
        assert!(s.add_root(PathBuf::from("/data")), "объемлющая папка заменяет вложенные");
        assert_eq!(s.roots, [PathBuf::from("/data")]);
    }
}
