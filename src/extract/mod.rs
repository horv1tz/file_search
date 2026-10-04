//! Извлечение текста из файлов. Точка входа — [`extract_file`].

pub mod doc;
pub mod legacy;
pub mod odf;
pub mod ooxml;
pub mod pdf;
pub mod rtf;
pub mod text;
pub mod xml;

use std::cell::Cell;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

pub use doc::{Doc, Extracted, Status, UNIT_SEP};

/// Версия логики извлечения. При её росте файлы переиндексируются, даже если не менялись.
pub const EXTRACTOR_VERSION: u64 = 1;

thread_local! {
    /// Внутри `extract_file` паники ожидаемы (битые файлы) и не должны засорять вывод.
    static IN_EXTRACT: Cell<bool> = const { Cell::new(false) };
}

/// Выполняется ли сейчас разбор файла (паники там ожидаемы и перехватываются).
pub fn in_extraction() -> bool {
    IN_EXTRACT.with(Cell::get)
}

/// Заглушает сообщения о паниках, перехваченных при разборе файлов; остальные паники печатаются как обычно.
pub fn install_quiet_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !IN_EXTRACT.with(Cell::get) {
            previous(info);
        }
    }));
}

/// Файл защищён паролем.
#[derive(Debug)]
pub struct Encrypted;

impl fmt::Display for Encrypted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("файл защищён паролем")
    }
}

impl std::error::Error for Encrypted {}

/// Файл с неизвестным расширением оказался не текстовым — это не ошибка, просто читать нечего.
#[derive(Debug)]
pub struct NotText;

impl fmt::Display for NotText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("не текстовый файл")
    }
}

impl std::error::Error for NotText {}

/// PDF — скан: страницы состоят из картинок, текстового слоя нет. Индексируется только имя файла.
#[derive(Debug)]
pub struct NoTextLayer;

impl fmt::Display for NoTextLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("скан без текстового слоя")
    }
}

impl std::error::Error for NoTextLayer {}

#[derive(Debug, Clone)]
pub struct Limits {
    /// Максимум текста, который берём из одного файла.
    pub max_text_bytes: usize,
    /// Файлы больше этого размера индексируются только по имени.
    pub max_file_size: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_text_bytes: 16 << 20, max_file_size: 512 << 20 }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Kind {
    /// Word/Excel/PowerPoint любых поколений: формат уточняется по содержимому.
    Office,
    Odf,
    Pdf,
    Rtf,
    Html,
    Text,
    Binary,
    Unknown,
}

const OFFICE: &[&str] = &[
    "docx", "docm", "dotx", "dotm", "doc", "dot", "xlsx", "xlsm", "xltx", "xltm", "xls", "xlt", "xlsb", "pptx", "pptm",
    "potx", "potm", "ppsx", "ppsm", "ppt", "pps", "pot",
];
const ODF: &[&str] = &["odt", "ott", "ods", "ots", "odp", "otp", "odg"];
const HTML: &[&str] = &["html", "htm", "xhtml", "mht", "mhtml"];
const TEXT: &[&str] = &[
    "txt",
    "md",
    "markdown",
    "rst",
    "csv",
    "tsv",
    "log",
    "json",
    "jsonl",
    "xml",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "properties",
    "env",
    "css",
    "scss",
    "less",
    "js",
    "mjs",
    "ts",
    "jsx",
    "tsx",
    "py",
    "rs",
    "go",
    "java",
    "kt",
    "kts",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "php",
    "rb",
    "sh",
    "bash",
    "zsh",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "sql",
    "tex",
    "bib",
    "srt",
    "vtt",
    "lua",
    "swift",
    "dart",
    "r",
    "scala",
    "pl",
    "vb",
    "vbs",
    "asm",
    "gradle",
    "cmake",
    "fb2",
    "eml",
    "ics",
    "vcf",
    "reg",
    "inf",
    "url",
    "svg",
    "1c",
    "bsl",
    "vue",
    "svelte",
    "tf",
    "proto",
    "diff",
    "patch",
];
const BINARY: &[&str] = &[
    "exe", "dll", "sys", "so", "dylib", "o", "obj", "a", "lib", "pdb", "class", "jar", "bin", "dat", "iso", "img",
    "vhd", "vhdx", "vmdk", "zip", "rar", "7z", "gz", "tgz", "bz2", "xz", "zst", "cab", "msi", "png", "jpg", "jpeg",
    "gif", "bmp", "ico", "tif", "tiff", "webp", "heic", "psd", "ai", "raw", "cr2", "nef", "mp3", "wav", "flac", "ogg",
    "m4a", "aac", "wma", "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "ttf", "otf", "woff", "woff2", "eot", "db",
    "sqlite", "sqlite3", "mdb", "accdb", "pyc", "wasm", "lnk", "tmp", "cache", "idx", "pack", "djvu", "epub", "mobi",
];

fn classify(ext: &str) -> Kind {
    if OFFICE.contains(&ext) {
        Kind::Office
    } else if ODF.contains(&ext) {
        Kind::Odf
    } else if ext == "pdf" {
        Kind::Pdf
    } else if ext == "rtf" {
        Kind::Rtf
    } else if HTML.contains(&ext) {
        Kind::Html
    } else if TEXT.contains(&ext) {
        Kind::Text
    } else if BINARY.contains(&ext) {
        Kind::Binary
    } else {
        Kind::Unknown
    }
}

fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(n);
    File::open(path)?.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// PDF разбирается целиком в памяти, поэтому для него свой, более строгий предел.
const PDF_MAX_SIZE: u64 = 128 << 20;

const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
const ZIP_EMPTY_MAGIC: &[u8] = b"PK\x05\x06";
const OLE_MAGIC: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Извлекает текст и метаданные из файла. Не паникует и не возвращает ошибок:
/// любая проблема отражается в [`Extracted::status`], а имя файла индексируется всегда.
pub fn extract_file(path: &Path, size: u64, limits: &Limits) -> Extracted {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    let kind = classify(&ext);

    if kind == Kind::Binary {
        return Extracted::without_content(Status::Skipped("бинарный тип".into()));
    }
    if size == 0 {
        return Extracted::without_content(Status::Empty);
    }
    if size > limits.max_file_size || (kind == Kind::Pdf && size > PDF_MAX_SIZE) {
        return Extracted::without_content(Status::Skipped("файл слишком большой".into()));
    }

    let mut doc = Doc::new(limits.max_text_bytes);
    IN_EXTRACT.with(|f| f.set(true));
    let result = catch_unwind(AssertUnwindSafe(|| run(path, kind, limits, &mut doc)));
    IN_EXTRACT.with(|f| f.set(false));
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "паника при разборе".into());
            Some(anyhow::anyhow!("паника при разборе: {msg}"))
        }
    };

    let mut extracted = doc.finish();
    match error {
        None => {}
        Some(e) if e.downcast_ref::<Encrypted>().is_some() => extracted.status = Status::Encrypted,
        Some(e) if e.downcast_ref::<NotText>().is_some() => extracted.status = Status::Skipped(e.to_string()),
        Some(e) if e.downcast_ref::<NoTextLayer>().is_some() => extracted.status = Status::Skipped(e.to_string()),
        Some(e) => extracted.status = Status::Failed(format!("{e:#}")),
    }
    extracted
}

fn run(path: &Path, kind: Kind, limits: &Limits, out: &mut Doc) -> anyhow::Result<()> {
    match kind {
        Kind::Office | Kind::Odf => office(path, limits, out),
        Kind::Pdf => pdf::extract(path, out),
        Kind::Rtf => plain(path, limits, out, PlainMode::Rtf),
        Kind::Html => plain(path, limits, out, PlainMode::Html),
        Kind::Text => plain(path, limits, out, PlainMode::Text),
        Kind::Unknown => plain(path, limits, out, PlainMode::Sniff),
        Kind::Binary => Ok(()),
    }
}

/// Office-файл: формат определяется по сигнатуре и содержимому, а не по расширению
/// (`.doc` бывает HTML или RTF, `.xls` — CSV, `.docx` — зашифрованным OLE или файлом OpenDocument).
fn office(path: &Path, limits: &Limits, out: &mut Doc) -> anyhow::Result<()> {
    let head = read_head(path, 16)?;
    if head.starts_with(ZIP_MAGIC) || head.starts_with(ZIP_EMPTY_MAGIC) {
        let mut pkg = ooxml::Pkg::open(path)?;
        if pkg.has("xl/workbook.bin") {
            legacy::excel(path, true, out)
        } else if pkg.has("mimetype") && pkg.has("content.xml") {
            odf::extract_pkg(&mut pkg, out)
        } else {
            ooxml::extract_pkg(&mut pkg, out)
        }
    } else if head.starts_with(OLE_MAGIC) {
        legacy::extract(path, out)
    } else if rtf::looks_like_rtf(&head) {
        plain(path, limits, out, PlainMode::Rtf)
    } else {
        // Ни zip, ни OLE, ни RTF: бывает HTML или обычный текст с «офисным» расширением.
        plain(path, limits, out, PlainMode::Sniff)
            .map_err(|_| anyhow::anyhow!("содержимое не похоже на документ Office"))
    }
}

#[derive(Clone, Copy)]
enum PlainMode {
    Text,
    Html,
    Rtf,
    /// Неизвестное расширение: читаем, только если содержимое похоже на текст.
    Sniff,
}

fn plain(path: &Path, limits: &Limits, out: &mut Doc, mode: PlainMode) -> anyhow::Result<()> {
    // Неизвестные расширения: по первым килобайтам решаем, стоит ли читать файл целиком.
    if matches!(mode, PlainMode::Sniff) && text::looks_binary(&read_head(path, 8192)?) {
        return Err(NotText.into());
    }
    let (bytes, cut) = text::read_prefix(path, limits.max_text_bytes)?;
    if cut {
        out.mark_truncated();
    }
    if matches!(mode, PlainMode::Rtf) {
        rtf::extract(&bytes, out);
        return Ok(());
    }
    let decoded = text::decode_bytes(&bytes);
    let as_html = match mode {
        PlainMode::Html => true,
        PlainMode::Sniff => text::looks_like_html(&decoded),
        _ => false,
    };
    if as_html {
        out.push(&text::strip_html(&decoded));
    } else {
        out.push(&decoded);
    }
    Ok(())
}
