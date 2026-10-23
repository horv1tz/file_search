//! Форма поиска: запрос, фильтры, сортировка. Сохраняется в настройках и восстанавливается при запуске.

use crate::settings::SavedForm;
use crate::theme::GROUPS;
use file_search::query::Mode;
use file_search::search::{SearchOptions, Sort};

pub struct Form {
    pub query: String,
    pub groups: [bool; GROUPS.len()],
    pub mode: Mode,
    pub sort: Sort,
    /// Искать только внутри этой папки.
    pub dir: Option<String>,
}

impl Form {
    pub(super) fn new(query: String) -> Form {
        Form { query, groups: [false; GROUPS.len()], mode: Mode::All, sort: Sort::Relevance, dir: None }
    }

    /// Форма с режимом, сортировкой и фильтрами типов из прошлого сеанса (папка не восстанавливается:
    /// это разовое сужение, которое легко забыть).
    pub(super) fn restored(query: String, saved: &SavedForm) -> Form {
        let mut form = Form::new(query);
        form.mode = match saved.mode.as_str() {
            "name" => Mode::Name,
            "content" => Mode::Content,
            _ => Mode::All,
        };
        form.sort = match saved.sort.as_str() {
            "newest" => Sort::Newest,
            "oldest" => Sort::Oldest,
            "largest" => Sort::Largest,
            "smallest" => Sort::Smallest,
            _ => Sort::Relevance,
        };
        for (i, on) in saved.groups.iter().enumerate().take(GROUPS.len()) {
            form.groups[i] = *on;
        }
        form
    }

    pub(super) fn saved(&self) -> SavedForm {
        SavedForm {
            mode: match self.mode {
                Mode::All => "all",
                Mode::Name => "name",
                Mode::Content => "content",
            }
            .into(),
            sort: match self.sort {
                Sort::Relevance => "relevance",
                Sort::Newest => "newest",
                Sort::Oldest => "oldest",
                Sort::Largest => "largest",
                Sort::Smallest => "smallest",
            }
            .into(),
            groups: self.groups.to_vec(),
        }
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
