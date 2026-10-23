//! Чтение файла для предпросмотра в фоновом потоке: текст, документы (как текст), картинки и страницы PDF.
//! Окно не ждёт диск и не разбирает файл сам: готовое приходит сообщением и показывается при следующей отрисовке.

use std::io;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use egui::{ColorImage, Context};
use file_search::extract::text::read_for_view;
use file_search::extract::{Limits, Status, UNIT_SEP, extract_file, is_text_ext, pdf::Pages};

/// Сколько байт текстового файла показываем. Больше — только начало, с пометкой.
pub const MAX_VIEW_BYTES: usize = 4 << 20;
/// Страницы PDF рисуем с такой большей стороной: читаемо на экране и не тяжело для памяти.
const PAGE_PX: f32 = 1400.0;
/// Картинки больше этого размера уменьшаем: на экране они всё равно не нужны в полном разрешении.
const MAX_IMAGE_PX: u32 = 2400;
/// Больше стольких пикселей картинку не декодируем: память нужна примерно вчетверо больше размера файла.
const MAX_IMAGE_PIXELS: u64 = 40_000_000;

const IMAGE_EXTS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "bmp", "webp"];
const DOCUMENT_EXTS: [&str; 13] =
    ["doc", "docx", "docm", "xls", "xlsx", "xlsm", "xlsb", "ppt", "pptx", "pptm", "odt", "ods", "odp"];

/// Как показывать файл: от этого зависит, что читает фоновый поток.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Текстовый файл: показываем содержимое как в редакторе.
    Text,
    /// Офисный документ: показываем извлечённый текст, оформление не воспроизводим.
    Document,
    Image,
    Pdf,
    /// Бинарные и неизвестные типы: показываем только сведения о файле.
    Other,
}

impl Kind {
    pub fn of(path: &Path) -> Kind {
        Kind::from_ext(path.extension().and_then(|e| e.to_str()).unwrap_or_default())
    }

    /// Тип по расширению без точки, в любом регистре.
    pub fn from_ext(ext: &str) -> Kind {
        let ext = ext.to_lowercase();
        if ext == "pdf" {
            Kind::Pdf
        } else if IMAGE_EXTS.contains(&ext.as_str()) {
            Kind::Image
        } else if DOCUMENT_EXTS.contains(&ext.as_str()) {
            Kind::Document
        } else if is_text_ext(&ext) {
            Kind::Text
        } else {
            Kind::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "Текст",
            Kind::Document => "Документ",
            Kind::Image => "Изображение",
            Kind::Pdf => "PDF",
            Kind::Other => "Файл",
        }
    }
}

/// Что фоновый поток передаёт окну.
pub enum Msg {
    /// `prose` — текст документа (показываем абзацами с переносом), иначе код или обычный текст.
    Text {
        text: String,
        truncated: bool,
        prose: bool,
    },
    Image(ColorImage),
    /// PDF открыт: сколько в нём страниц.
    Pdf {
        pages: usize,
    },
    /// Отрисованная страница; `None` — страницу не удалось нарисовать.
    Page {
        index: usize,
        image: Option<ColorImage>,
    },
    Failed(String),
    Unsupported,
}

/// Запускает чтение файла. Запросы страниц PDF приходят через `pages`; когда окно их не ждёт, поток завершается.
pub fn spawn(ctx: Context, path: std::path::PathBuf, kind: Kind, size: u64, pages: Receiver<usize>) -> Receiver<Msg> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let send = |msg: Msg| {
            let _ = tx.send(msg);
            ctx.request_repaint();
        };
        match kind {
            Kind::Text => send(read_text(&path)),
            Kind::Document => send(read_document(&path, size)),
            Kind::Image => send(read_image(&path)),
            Kind::Pdf => serve_pdf(&path, &pages, &send),
            Kind::Other => send(Msg::Unsupported),
        }
    });
    rx
}

fn read_text(path: &Path) -> Msg {
    match read_for_view(path, MAX_VIEW_BYTES) {
        Ok((text, truncated)) => Msg::Text { text, truncated, prose: false },
        Err(e) => Msg::Failed(io_message(&e)),
    }
}

fn read_document(path: &Path, size: u64) -> Msg {
    let extracted = extract_file(path, size, &Limits::default());
    let text = match extracted.status {
        Status::Ok | Status::Recognized | Status::Empty => extracted.text,
        Status::Encrypted => return Msg::Failed("Документ защищён паролем".into()),
        Status::Scan => return Msg::Failed("В документе только картинки: текста для показа нет".into()),
        Status::Skipped(reason) => return Msg::Failed(reason),
        Status::Failed(e) => return Msg::Failed(format!("Не удалось прочитать документ: {e}")),
    };
    // Листы и слайды разделены символом перевода страницы: для показа делаем обычные пустые строки.
    let text = text.replace(UNIT_SEP, "\n\n");
    if text.trim().is_empty() {
        return Msg::Failed("В документе нет текста".into());
    }
    Msg::Text { text, truncated: extracted.truncated, prose: true }
}

fn read_image(path: &Path) -> Msg {
    match decode_image(path) {
        Ok(image) => Msg::Image(image),
        Err(e) => Msg::Failed(e),
    }
}

fn decode_image(path: &Path) -> Result<ColorImage, String> {
    let open = || image::ImageReader::open(path).and_then(|r| r.with_guessed_format());
    let (width, height) = open()
        .map_err(|e| io_message(&e))?
        .into_dimensions()
        .map_err(|e| format!("Не удалось открыть изображение: {e}"))?;
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("Изображение слишком большое для просмотра".into());
    }
    let decoded =
        open().map_err(|e| io_message(&e))?.decode().map_err(|e| format!("Не удалось открыть изображение: {e}"))?;
    let shown = if decoded.width() > MAX_IMAGE_PX || decoded.height() > MAX_IMAGE_PX {
        decoded.thumbnail(MAX_IMAGE_PX, MAX_IMAGE_PX)
    } else {
        decoded
    };
    let rgba = shown.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

fn serve_pdf(path: &Path, requests: &Receiver<usize>, send: &impl Fn(Msg)) {
    let pages = match Pages::open(path) {
        Ok(pages) => pages,
        Err(e) => return send(Msg::Failed(e.to_string())),
    };
    send(Msg::Pdf { pages: pages.count() });
    for index in requests {
        let image = pages
            .render(index, PAGE_PX)
            .map(|page| ColorImage::from_rgba_unmultiplied([page.width as usize, page.height as usize], &page.rgba));
        send(Msg::Page { index, image });
    }
}

/// Понятное сообщение об ошибке чтения файла.
pub fn io_message(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "Файл не найден — возможно, его удалили или переместили".to_string(),
        io::ErrorKind::PermissionDenied => "Нет доступа к файлу".to_string(),
        _ => format!("Не удалось прочитать файл: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_follows_extension() {
        assert_eq!(Kind::of(Path::new("a.PDF")), Kind::Pdf);
        assert_eq!(Kind::of(Path::new("scan.jpeg")), Kind::Image);
        assert_eq!(Kind::of(Path::new("отчёт.docx")), Kind::Document);
        assert_eq!(Kind::of(Path::new("таблица.xlsx")), Kind::Document);
        assert_eq!(Kind::of(Path::new("заметки.txt")), Kind::Text);
        assert_eq!(Kind::of(Path::new("program.exe")), Kind::Other);
        assert_eq!(Kind::from_ext("XLSX"), Kind::Document);
        assert_eq!(Kind::of(Path::new("без_расширения")), Kind::Other);
    }
}
