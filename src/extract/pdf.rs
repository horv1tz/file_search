//! PDF: текст постранично (страница = юнит).
//!
//! Основной разбор — `pdf-extract` (хорошо собирает слова и строки). Если он споткнулся на файле или не нашёл текста,
//! пробуем второй разборщик (`lopdf`): он по-другому читает шрифты и часто справляется там, где первый сдаётся.
//! Если текста нет совсем, но в файле есть картинки, это скан: честно сообщаем, что искать по такому файлу можно
//! только по имени.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use super::doc::Doc;
use super::ocr::{self, Frame, Recognizer};
use super::{Encrypted, Limits, NoTextLayer, NoTextRecognized};

fn is_password_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("password") || lower.contains("encrypt") || lower.contains("decrypt")
}

/// Меньше стольких букв на страницу в среднем — текст подозрительно скудный (колонтитулы и номера страниц у скана,
/// мусор из шрифта без таблицы символов): стоит попробовать второй разборщик и взять лучший результат.
const THIN_LETTERS_PER_PAGE: usize = 20;
/// Меньше стольких букв во всём файле — текста нет (одиночные цифры и знаки текстом не считаются).
const MIN_LETTERS: usize = 3;

/// Основной разбор. Паника в `pdf-extract` (на редких шрифтах) не должна лишать файл запасного разбора.
fn primary(path: &Path) -> Result<Vec<String>, String> {
    match catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_by_pages(path))) {
        Ok(Ok(pages)) => Ok(pages),
        Ok(Err(e)) => Err(e.to_string()),
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "неизвестная причина".into());
            Err(format!("паника в основном разборщике: {msg}"))
        }
    }
}

/// Страница, на которой букв меньше, считается «без текста» и отдаётся на распознавание.
const THIN_PAGE_LETTERS: usize = 20;
/// Картинка страницы для распознавания: длинная сторона в пикселях (около 280 dpi для листа A4 — столько же, сколько
/// у обычного скана: на меньшем разрешении мелкие буквы сливаются, и ошибок становится много). Не больше, чем
/// принимает распознавание.
const OCR_TARGET_PX: f32 = 3300.0;
/// Дольше этого один файл не распознаётся: остальные страницы пропускаются.
const OCR_MAX_SECONDS: u64 = 180;

pub fn extract(path: &Path, out: &mut Doc, limits: &Limits) -> Result<()> {
    let recognizer = if limits.ocr { ocr::system_recognizer() } else { None };
    extract_with(path, out, limits.ocr_max_pages, recognizer)
}

/// Текст PDF; страницы без текстового слоя при наличии распознавателя читаются распознаванием.
fn extract_with(path: &Path, out: &mut Doc, ocr_max_pages: usize, recognizer: Option<&Recognizer>) -> Result<()> {
    let mut read_error = None;
    let mut pages = match read_text(path) {
        Ok(pages) => pages,
        // Файл с паролем распознаванием не открыть; остальные сбои — повод попробовать прочитать картинки.
        Err(e) if recognizer.is_none() || e.downcast_ref::<Encrypted>().is_some() => return Err(e),
        Err(e) => {
            read_error = Some(e);
            Vec::new()
        }
    };
    let mut ocr_tried = false;
    if is_thin(&pages)
        && let Some(recognizer) = recognizer
    {
        match recognize_pages(path, &mut pages, ocr_max_pages, recognizer, out) {
            Ok(tried) => ocr_tried = tried,
            // Распознавание не сработало ни на одной странице: это сбой (его повторят позже), а не пустой скан.
            Err(why) => return Err(anyhow!("распознавание не удалось: {why}")),
        }
    }
    if letters(&pages) < MIN_LETTERS {
        if ocr_tried {
            return Err(NoTextRecognized.into());
        }
        if let Some(e) = read_error {
            return Err(e);
        }
        return Err(if has_images(path) { NoTextLayer.into() } else { anyhow!("в PDF нет текста") });
    }
    for (i, page) in pages.iter().enumerate() {
        if out.is_full() {
            break;
        }
        out.begin_unit(format!("Стр. {}", i + 1));
        for line in page.lines() {
            let line = line.trim();
            if !line.is_empty() {
                out.push(line);
                out.newline();
            }
        }
    }
    Ok(())
}

/// Текстовый слой PDF, постранично (может оказаться скудным или пустым).
fn read_text(path: &Path) -> Result<Vec<String>> {
    match primary(path) {
        Ok(pages) if !is_thin(&pages) => Ok(pages),
        // Текста нет или его слишком мало: второй разборщик иногда читает то, что не прочёл первый.
        Ok(pages) => match fallback(path) {
            Ok(second) if letters(&second) > letters(&pages) => Ok(second),
            Err(e) if e.downcast_ref::<Encrypted>().is_some() => Err(e),
            _ => Ok(pages),
        },
        // «Пароль» только на права (печать, копирование): файл открывается с пустым паролем.
        Err(msg) if is_password_error(&msg) => fallback(path).map_err(|_| Encrypted.into()),
        Err(msg) => fallback(path).map_err(|second| anyhow!("{msg}; запасной разбор: {second}")),
    }
}

/// Распознаёт страницы, на которых нет текста, и подставляет результат в `pages`. `Ok(true)` — распознавание
/// действительно запускалось (чтобы отличить «не распознали» от «не пробовали»); `Err` — оно не удалось
/// ни на одной из попыток.
fn recognize_pages(
    path: &Path,
    pages: &mut Vec<String>,
    max_pages: usize,
    recognizer: &Recognizer,
    out: &mut Doc,
) -> Result<bool, String> {
    let Ok(bytes) = std::fs::read(path) else { return Ok(false) };
    let Ok(Ok(pdf)) = catch_unwind(AssertUnwindSafe(|| hayro::hayro_syntax::Pdf::new(Arc::new(bytes)))) else {
        return Ok(false);
    };
    let total = pdf.pages().len();
    if total == 0 {
        return Ok(false);
    }
    if pages.len() < total {
        pages.resize(total, String::new());
    }
    let started = Instant::now();
    let cache = hayro::RenderCache::new();
    let mut tried = 0usize;
    let mut failed = 0usize;
    let mut last_error = String::new();
    let mut recognized = false;
    for (i, page) in pdf.pages().iter().enumerate() {
        if i >= max_pages || started.elapsed() >= Duration::from_secs(OCR_MAX_SECONDS) {
            out.mark_truncated();
            break;
        }
        if letters(&pages[i..=i]) >= THIN_PAGE_LETTERS {
            continue;
        }
        let Some((rgba, width, height)) = render_page(page, &cache) else { continue };
        tried += 1;
        let text = match recognizer(&Frame { rgba: &rgba, width, height }) {
            Ok(text) => text,
            // Сбой на одной странице не лишает файл остальных: страница пропускается.
            Err(e) => {
                failed += 1;
                last_error = format!("{e:#}");
                continue;
            }
        };
        if letters(std::slice::from_ref(&text)) > letters(&pages[i..=i]) {
            pages[i] = text;
            recognized = true;
        }
    }
    if recognized {
        out.mark_ocr();
    }
    if tried > 0 && failed == tried {
        return Err(last_error);
    }
    Ok(tried > 0)
}

/// Рисует страницу в RGBA на белом фоне. `None` — страницу нарисовать не удалось.
fn render_page<'a>(
    page: &'a hayro::hayro_syntax::page::Page<'a>,
    cache: &hayro::RenderCache<'a>,
) -> Option<(Vec<u8>, u32, u32)> {
    let (w, h) = page.render_dimensions();
    if !(w > 1.0 && h > 1.0) {
        return None;
    }
    let target = OCR_TARGET_PX.min(ocr::max_dimension().saturating_sub(40) as f32);
    let scale = (target / w.max(h)).clamp(0.5, 5.0);
    let settings = hayro::PixmapSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: hayro::vello_cpu::color::palette::css::WHITE,
    };
    let pixmap = catch_unwind(AssertUnwindSafe(|| {
        hayro::render(
            page,
            cache,
            &hayro::hayro_interpret::InterpreterSettings::default(),
            &hayro::RenderSettings::default(),
            &settings,
        )
    }))
    .ok()?;
    let (width, height) = (pixmap.width() as u32, pixmap.height() as u32);
    (width > 0 && height > 0).then(|| (pixmap.data_as_u8_slice().to_vec(), width, height))
}

/// Сколько букв в тексте всех страниц (цифры и знаки не считаются).
fn letters(pages: &[String]) -> usize {
    pages.iter().map(|p| p.chars().filter(|c| c.is_alphabetic()).count()).sum()
}

fn is_thin(pages: &[String]) -> bool {
    letters(pages) < THIN_LETTERS_PER_PAGE * pages.len().max(1)
}

/// Запасной разборщик на `lopdf`: страницы читаются по одной, нечитаемая страница не мешает остальным.
fn fallback(path: &Path) -> Result<Vec<String>> {
    let mut doc = lopdf::Document::load(path).map_err(|e| {
        let msg = e.to_string();
        if is_password_error(&msg) { anyhow::Error::from(Encrypted) } else { anyhow!("{msg}") }
    })?;
    if doc.is_encrypted() {
        // Чаще всего пароль стоит только на права (копирование, печать), а читается файл с пустым паролем.
        doc.decrypt("").map_err(|_| anyhow::Error::from(Encrypted))?;
    }
    let numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
    Ok(numbers.iter().map(|n| doc.extract_text(&[*n]).unwrap_or_default()).collect())
}

/// Есть ли в файле растровые картинки (признак скана).
fn has_images(path: &Path) -> bool {
    let Ok(doc) = lopdf::Document::load(path) else { return false };
    doc.objects.values().any(|object| match object {
        lopdf::Object::Stream(stream) => {
            stream.dict.get(b"Subtype").ok().and_then(|v| v.as_name().ok()).is_some_and(|name| name == b"Image")
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    use super::*;
    use crate::extract::{Limits, Status, extract_file};

    /// Одностраничный PDF: с текстом или со «сканом» (только картинка).
    fn make_pdf(path: &Path, text: Option<&str>) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
            },
            vec![255],
        ));
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Im1" => image_id },
        });
        let operations = match text {
            Some(t) => vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![100.into(), 600.into()]),
                Operation::new("Tj", vec![Object::string_literal(t)]),
                Operation::new("ET", vec![]),
            ],
            None => vec![
                Operation::new("q", vec![]),
                Operation::new("cm", vec![200.into(), 0.into(), 0.into(), 200.into(), 50.into(), 500.into()]),
                Operation::new("Do", vec!["Im1".into()]),
                Operation::new("Q", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, Content { operations }.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).unwrap();
    }

    /// PDF с паролем только на права: открывается с пустым паролем, как в любом просмотрщике.
    fn make_restricted_pdf(path: &Path, text: &str) {
        use lopdf::encryption::{EncryptionState, EncryptionVersion, Permissions};
        let plain = path.with_extension("plain.pdf");
        make_pdf(&plain, Some(text));
        let mut doc = Document::load(&plain).unwrap();
        // Для шифрования нужен идентификатор файла.
        let id = Object::string_literal(b"0123456789abcdef".to_vec());
        doc.trailer.set("ID", vec![id.clone(), id]);
        let state = EncryptionState::try_from(EncryptionVersion::V2 {
            document: &doc,
            owner_password: "secret-owner",
            user_password: "",
            key_length: 128,
            permissions: Permissions::PRINTABLE,
        })
        .unwrap();
        doc.encrypt(&state).unwrap();
        doc.save(path).unwrap();
    }

    #[test]
    fn restricted_but_openable_pdf_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restricted.pdf");
        make_restricted_pdf(&path, "Confidential budget");
        let e = extract_file(&path, std::fs::metadata(&path).unwrap().len(), &Limits::default());
        assert_eq!(e.status, Status::Ok, "{e:?}");
        assert!(e.text.contains("Confidential budget"), "{}", e.text);
        let mut doc = Document::load(&path).unwrap();
        assert!(!doc.is_encrypted() || doc.decrypt("").is_ok());
    }

    /// Распознаватель-пустышка: считает, сколько раз его позвали, и «читает» один и тот же текст.
    fn fake(text: &'static str, calls: &std::cell::Cell<usize>) -> impl Fn(&Frame) -> Result<String> {
        move |frame: &Frame| {
            calls.set(calls.get() + 1);
            assert!(frame.width > 100 && frame.height > 100, "страница отрисована в нормальный размер");
            assert_eq!(frame.rgba.len(), (frame.width * frame.height * 4) as usize);
            Ok(text.to_string())
        }
    }

    #[test]
    fn scan_pages_are_recognized_when_a_recognizer_exists() {
        let dir = tempfile::tempdir().unwrap();
        let scan = dir.path().join("scan.pdf");
        make_pdf(&scan, None);
        let calls = std::cell::Cell::new(0);
        let recognizer = fake("СЧЁТ НА ОПЛАТУ Invoice 2026", &calls);

        let mut doc = Doc::new(1 << 20);
        extract_with(&scan, &mut doc, 30, Some(&recognizer)).unwrap();
        let e = doc.finish();
        assert_eq!(calls.get(), 1, "одна страница — один вызов");
        assert!(e.ocr && e.text.contains("Invoice 2026"), "{e:?}");
        assert_eq!(e.units, ["Стр. 1"]);

        // Без распознавателя — по-прежнему скан, найденный только по имени.
        let mut doc = Doc::new(1 << 20);
        let err = extract_with(&scan, &mut doc, 30, None).unwrap_err();
        assert!(err.downcast_ref::<NoTextLayer>().is_some(), "{err:#}");
    }

    #[test]
    fn pages_with_text_are_not_sent_to_recognition() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("text.pdf");
        make_pdf(&text, Some("Quarterly revenue grew by twelve percent this year"));
        let calls = std::cell::Cell::new(0);
        let recognizer = fake("лишнее", &calls);
        let mut doc = Doc::new(1 << 20);
        extract_with(&text, &mut doc, 30, Some(&recognizer)).unwrap();
        let e = doc.finish();
        assert_eq!(calls.get(), 0, "на страницах с текстом распознавание не нужно");
        assert!(!e.ocr && e.text.contains("Quarterly revenue"));
    }

    #[test]
    fn recognition_failures_and_blank_pages_are_reported_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let scan = dir.path().join("scan.pdf");
        make_pdf(&scan, None);
        // Распознавание ничего не нашло: файл остаётся найденным по имени, но это не «не пробовали».
        let calls = std::cell::Cell::new(0);
        let blank = fake("  \n", &calls);
        let err = extract_with(&scan, &mut Doc::new(1 << 20), 30, Some(&blank)).unwrap_err();
        assert!(err.downcast_ref::<NoTextRecognized>().is_some(), "{err:#}");
        // Распознаватель сломан: это сбой (файл будет прочитан позже), а не «пустой скан».
        let broken = |_: &Frame| -> Result<String> { Err(anyhow!("сбой")) };
        let err = extract_with(&scan, &mut Doc::new(1 << 20), 30, Some(&broken)).unwrap_err();
        assert!(err.to_string().contains("распознавание не удалось: сбой"), "{err:#}");
        // Лимит страниц: ноль страниц — распознавание не запускается, файл помечен обрезанным.
        let calls = std::cell::Cell::new(0);
        let recognizer = fake("текст", &calls);
        let mut doc = Doc::new(1 << 20);
        let _ = extract_with(&scan, &mut doc, 0, Some(&recognizer));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn status_reflects_recognition_through_extract_file() {
        let dir = tempfile::tempdir().unwrap();
        let scan = dir.path().join("scan.pdf");
        make_pdf(&scan, None);
        let size = std::fs::metadata(&scan).unwrap().len();
        // Распознавание не запрошено: статус «скан», а не «пропущено».
        assert_eq!(extract_file(&scan, size, &Limits::default()).status, Status::Scan);
    }

    /// Только Windows: настоящее встроенное распознавание читает отрисованный текст. Если в системе нет языка
    /// распознавания (бывает на серверных образах), проверка пропускается с пояснением.
    #[cfg(windows)]
    #[test]
    fn windows_ocr_reads_rendered_text() {
        let info = ocr::engine_info();
        if !info.available {
            eprintln!("пропуск: нет языка распознавания. Доступно: {:?}. {}", info.languages, info.note);
            return;
        }
        eprintln!("распознавание доступно, языки: {:?}", info.languages);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invoice.pdf");
        make_pdf(&path, Some("INVOICE 2026 PAYMENT DUE"));
        let bytes = std::fs::read(&path).unwrap();
        let pdf = hayro::hayro_syntax::Pdf::new(Arc::new(bytes)).unwrap();
        let cache = hayro::RenderCache::new();
        let (rgba, width, height) = render_page(&pdf.pages()[0], &cache).expect("страница отрисована");
        let text = ocr::system(&Frame { rgba: &rgba, width, height }).expect("распознавание выполнено");
        eprintln!("распознано: {text:?}");
        assert!(text.to_uppercase().contains("INVOICE"), "{text:?}");
    }

    #[test]
    fn thin_text_does_not_hide_a_scan() {
        // Номер страницы вместо текста — это скан, а не «проиндексированный документ».
        assert!(is_thin(&["3".to_string()]));
        assert!(is_thin(&["Стр. 12".to_string()]));
        assert!(!is_thin(&["Договор аренды нежилого помещения, заключённый между сторонами".to_string()]));
        assert_eq!(letters(&["Hello, мир 123".to_string()]), 8);
    }

    #[test]
    fn fallback_reads_text_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.pdf");
        make_pdf(&path, Some("Hello fallback parser"));
        let pages = fallback(&path).unwrap();
        assert_eq!(pages.len(), 1);
        assert!(pages[0].contains("Hello fallback parser"), "{pages:?}");
    }

    #[test]
    fn text_pdf_is_indexed_and_scan_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("text.pdf");
        make_pdf(&text, Some("Quarterly revenue"));
        let e = extract_file(&text, std::fs::metadata(&text).unwrap().len(), &Limits::default());
        assert_eq!(e.status, Status::Ok, "{e:?}");
        assert!(e.text.contains("Quarterly revenue"), "{}", e.text);

        let scan = dir.path().join("scan.pdf");
        make_pdf(&scan, None);
        assert!(has_images(&scan));
        let e = extract_file(&scan, std::fs::metadata(&scan).unwrap().len(), &Limits::default());
        assert_eq!(e.status, Status::Scan, "{e:?}");
    }
}
