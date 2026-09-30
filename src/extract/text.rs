//! Обычные текстовые файлы: определение кодировки (UTF-8/16, cp1251, cp866, KOI8-R…),
//! очистка HTML.

use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use encoding_rs::{UTF_16BE, UTF_16LE};

/// Читает не более `max` байт файла. Второй элемент — файл был обрезан.
pub fn read_prefix(path: &Path, max: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() > max;
    buf.truncate(max);
    Ok((buf, truncated))
}

fn utf16_without_bom(sample: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let sample = &sample[..sample.len().min(4096) & !1];
    if sample.len() < 4 {
        return None;
    }
    let pairs = sample.len() / 2;
    let (mut even_zero, mut odd_zero) = (0usize, 0usize);
    for ch in sample.chunks_exact(2) {
        even_zero += (ch[0] == 0) as usize;
        odd_zero += (ch[1] == 0) as usize;
    }
    if odd_zero * 10 >= pairs * 3 && even_zero * 50 < pairs {
        Some(UTF_16LE)
    } else if even_zero * 10 >= pairs * 3 && odd_zero * 50 < pairs {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// Похоже ли начало файла на бинарные данные.
pub fn looks_binary(sample: &[u8]) -> bool {
    if sample.starts_with(&[0xFF, 0xFE]) || sample.starts_with(&[0xFE, 0xFF]) || utf16_without_bom(sample).is_some() {
        return false;
    }
    if sample.contains(&0) {
        return true;
    }
    let control = sample.iter().filter(|&&b| b < 9 || (b > 13 && b < 32 && b != 27)).count();
    control * 20 > sample.len()
}

pub fn decode_bytes(bytes: &[u8]) -> Cow<'_, str> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return UTF_16LE.decode_without_bom_handling(rest).0;
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return UTF_16BE.decode_without_bom_handling(rest).0;
    }
    if let Some(enc) = utf16_without_bom(bytes) {
        return enc.decode_without_bom_handling(bytes).0;
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => return Cow::Borrowed(s),
        // Файл обрезан посреди символа — это всё ещё UTF-8.
        Err(e) if e.error_len().is_none() => {
            return Cow::Borrowed(std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or(""));
        }
        Err(_) => {}
    }
    // Почти-UTF-8 с единичными битыми байтами лучше читать как UTF-8.
    let lossy = String::from_utf8_lossy(bytes);
    let bad = lossy.matches('\u{FFFD}').count();
    if bad * 100 < lossy.chars().count().max(1) {
        return lossy;
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(&bytes[..bytes.len().min(64 * 1024)], bytes.len() <= 64 * 1024);
    let enc = det.guess(None, chardetng::Utf8Detection::Deny);
    Cow::Owned(enc.decode_without_bom_handling(bytes).0.into_owned())
}

pub fn looks_like_html(text: &str) -> bool {
    let head: String = text.trim_start().chars().take(512).collect::<String>().to_lowercase();
    head.starts_with("<!doctype html")
        || head.starts_with("<html")
        || head.contains("<html")
        || head.contains("<body")
        || head.contains("<table")
}

fn decode_entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "laquo" => '«',
        "raquo" => '»',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "copy" => '©',
        "reg" => '®',
        "deg" => '°',
        _ => return None,
    })
}

fn find_ci(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Грубое удаление разметки: теги, скрипты, стили, комментарии; сущности раскрываются.
pub fn strip_html(s: &str) -> String {
    const BLOCK: [&str; 22] = [
        "p",
        "div",
        "br",
        "li",
        "ul",
        "ol",
        "tr",
        "table",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "section",
        "article",
        "header",
        "footer",
        "title",
        "pre",
        "blockquote",
        "hr",
    ];
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' => {
                if b[i..].starts_with(b"<!--") {
                    i = find_ci(b, i + 4, b"-->").map(|p| p + 3).unwrap_or(b.len());
                    continue;
                }
                let closing = b.get(i + 1) == Some(&b'/');
                let name_start = i + 1 + closing as usize;
                let name_end = (name_start..b.len()).find(|&k| !b[k].is_ascii_alphanumeric()).unwrap_or(b.len());
                let name = s[name_start..name_end].to_ascii_lowercase();
                let tag_end = b[i..].iter().position(|&c| c == b'>').map(|p| i + p + 1).unwrap_or(b.len());
                if !closing && (name == "script" || name == "style") {
                    let close = format!("</{name}");
                    i = find_ci(b, tag_end, close.as_bytes())
                        .and_then(|p| b[p..].iter().position(|&c| c == b'>').map(|q| p + q + 1))
                        .unwrap_or(b.len());
                    continue;
                }
                out.push(if BLOCK.contains(&name.as_str()) { '\n' } else { ' ' });
                i = tag_end;
            }
            b'&' => {
                let end = b[i + 1..].iter().take(10).position(|&c| c == b';');
                if let Some(p) = end
                    && let Some(c) = decode_entity(&s[i + 1..i + 1 + p])
                {
                    out.push(c);
                    i += p + 2;
                    continue;
                }
                out.push('&');
                i += 1;
            }
            _ => {
                let j = b[i..].iter().position(|&c| c == b'<' || c == b'&').map(|p| i + p).unwrap_or(b.len());
                out.push_str(&s[i..j]);
                i = j;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cp1251() {
        let (bytes, _, _) = encoding_rs::WINDOWS_1251
            .encode("Договор аренды помещения № 5 от 12 марта, стороны подписали акт приёма-передачи");
        assert_eq!(
            decode_bytes(&bytes),
            "Договор аренды помещения № 5 от 12 марта, стороны подписали акт приёма-передачи"
        );
    }

    #[test]
    fn detects_utf16_with_and_without_bom() {
        let mut with_bom = vec![0xFF, 0xFE];
        let mut without: Vec<u8> = Vec::new();
        for u in "Привет, мир! Hello".encode_utf16() {
            with_bom.extend(u.to_le_bytes());
            without.extend(u.to_le_bytes());
        }
        assert_eq!(decode_bytes(&with_bom), "Привет, мир! Hello");
        assert_eq!(decode_bytes(&without), "Привет, мир! Hello");
        assert!(!looks_binary(&without));
    }

    #[test]
    fn binary_is_detected() {
        assert!(looks_binary(&[0x4D, 0x5A, 0x90, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00]));
        assert!(!looks_binary("обычный текст\nвторая строка".as_bytes()));
    }

    #[test]
    fn html_is_stripped() {
        let html = "<html><head><style>p{color:red}</style><script>var a='<b>';</script></head><body><p>Привет &amp; пока</p><!-- c --><div>Вторая&nbsp;строка &#1078;</div></body></html>";
        let text = strip_html(html);
        let words: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(words, ["Привет", "&", "пока", "Вторая", "строка", "ж"]);
    }
}
