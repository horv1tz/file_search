//! Схема индекса и открытие каталога индекса.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tantivy::Index;
use tantivy::directory::MmapDirectory;
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};

use crate::analyzer::{self, ANALYZER_VERSION, RAW_TOKENIZER_NAME, TOKENIZER_NAME};

/// Версия формата индекса (схема + правила анализа).
const FORMAT_VERSION: u32 = 1;
const VERSION_FILE: &str = "fsearch.version";

#[derive(Clone, Copy)]
pub struct Fields {
    /// Путь для показа пользователю.
    pub path: Field,
    /// Уникальный ключ файла (нормализованный путь, см. [`path_key`]).
    pub key: Field,
    /// Слова из имени файла (без расширения).
    pub name: Field,
    /// Полное имя файла в нижнем регистре — для поиска подстроки.
    pub name_lc: Field,
    pub ext: Field,
    /// Слова из пути к папке.
    pub folder: Field,
    /// Заголовок, автор, ключевые слова, имена листов.
    pub meta: Field,
    pub content: Field,
    /// Подписи юнитов (листы, слайды, страницы), по одной в строке.
    pub units: Field,
    pub size: Field,
    /// Время изменения, миллисекунды с 1970 года.
    pub mtime: Field,
    pub status: Field,
    /// Версия извлекателя, которой файл был проиндексирован.
    pub ver: Field,
}

fn analyzed(stored: bool, positions: bool) -> TextOptions {
    let indexing = TextFieldIndexing::default().set_tokenizer(TOKENIZER_NAME).set_index_option(if positions {
        IndexRecordOption::WithFreqsAndPositions
    } else {
        IndexRecordOption::WithFreqs
    });
    let opts = TextOptions::default().set_indexing_options(indexing);
    if stored { opts.set_stored() } else { opts }
}

pub fn build_schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let fields = Fields {
        path: b.add_text_field("path", STRING | STORED),
        key: b.add_text_field("key", STRING | FAST),
        name: b.add_text_field("name", analyzed(false, true)),
        name_lc: b.add_text_field("name_lc", STRING),
        ext: b.add_text_field("ext", STRING | STORED | FAST),
        folder: b.add_text_field("folder", analyzed(false, false)),
        meta: b.add_text_field("meta", analyzed(true, true)),
        content: b.add_text_field("content", analyzed(true, true)),
        units: b.add_text_field("units", STORED),
        size: b.add_u64_field("size", INDEXED | STORED | FAST),
        mtime: b.add_u64_field("mtime", INDEXED | STORED | FAST),
        status: b.add_text_field("status", STRING | STORED | FAST),
        ver: b.add_u64_field("ver", FAST),
    };
    (b.build(), fields)
}

/// Каталог индекса по умолчанию: `%LOCALAPPDATA%\file_search\index` на Windows.
pub fn default_index_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("file_search").join("index")
}

/// Нормализованный ключ пути: на Windows и macOS регистр не различается, разделитель всегда `/`.
pub fn path_key(path: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) { path.replace('\\', "/").to_lowercase() } else { path.to_string() }
}

fn register(index: &Index) {
    index.tokenizers().register(TOKENIZER_NAME, analyzer::build());
    index.tokenizers().register(RAW_TOKENIZER_NAME, analyzer::build_raw());
}

pub fn index_exists(dir: &Path) -> bool {
    dir.join("meta.json").exists()
}

fn check_version(dir: &Path) -> Result<()> {
    let expected = format!("{FORMAT_VERSION}.{ANALYZER_VERSION}");
    match fs::read_to_string(dir.join(VERSION_FILE)) {
        Ok(v) if v.trim() == expected => Ok(()),
        Ok(v) => bail!(
            "индекс в {} создан другой версией программы ({}), нужна {expected}. \
             Удалите его командой `fsearch reset` и проиндексируйте заново.",
            dir.display(),
            v.trim()
        ),
        Err(_) => bail!("в {} нет метки версии индекса — это не индекс file_search", dir.display()),
    }
}

/// Открывает существующий индекс.
pub fn open_index(dir: &Path) -> Result<(Index, Fields)> {
    if !index_exists(dir) {
        bail!("индекс не найден в {}. Сначала выполните: fsearch index <папка>", dir.display());
    }
    check_version(dir)?;
    let index = Index::open_in_dir(dir).with_context(|| format!("не удалось открыть индекс в {}", dir.display()))?;
    register(&index);
    let (schema, fields) = build_schema();
    if index.schema() != schema {
        bail!("схема индекса не совпадает с ожидаемой; выполните `fsearch reset` и проиндексируйте заново");
    }
    Ok((index, fields))
}

/// Открывает индекс, создавая его при необходимости.
pub fn open_or_create_index(dir: &Path) -> Result<(Index, Fields)> {
    if index_exists(dir) {
        return open_index(dir);
    }
    fs::create_dir_all(dir).with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
    let (schema, fields) = build_schema();
    let index = Index::create(MmapDirectory::open(dir)?, schema, Default::default())?;
    register(&index);
    fs::write(dir.join(VERSION_FILE), format!("{FORMAT_VERSION}.{ANALYZER_VERSION}"))?;
    Ok((index, fields))
}
