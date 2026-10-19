//! PDF: текст постранично (страница = юнит).
//!
//! Основной разбор — `pdf-extract` (хорошо собирает слова и строки). Если он споткнулся на файле или не нашёл текста,
//! пробуем второй разборщик (`lopdf`): он по-другому читает шрифты и часто справляется там, где первый сдаётся.
//! Если текста нет совсем, но в файле есть картинки, это скан: честно сообщаем, что искать по такому файлу можно
//! только по имени.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use anyhow::{Result, anyhow};

use super::doc::Doc;
use super::{Encrypted, NoTextLayer};

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

pub fn extract(path: &Path, out: &mut Doc) -> Result<()> {
    let pages = match primary(path) {
        Ok(pages) if !is_thin(&pages) => pages,
        // Текста нет или его слишком мало: второй разборщик иногда читает то, что не прочёл первый.
        Ok(pages) => match fallback(path) {
            Ok(second) if letters(&second) > letters(&pages) => second,
            Err(e) if e.downcast_ref::<Encrypted>().is_some() => return Err(e),
            _ => pages,
        },
        Err(msg) if is_password_error(&msg) => match fallback(path) {
            // «Пароль» только на права (печать, копирование): файл открывается с пустым паролем.
            Ok(pages) => pages,
            Err(_) => return Err(Encrypted.into()),
        },
        Err(msg) => fallback(path).map_err(|second| anyhow!("{msg}; запасной разбор: {second}"))?,
    };
    if letters(&pages) < MIN_LETTERS {
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
        assert_eq!(e.status, Status::Skipped("скан без текстового слоя".into()), "{e:?}");
    }
}
