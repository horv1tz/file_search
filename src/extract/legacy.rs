//! Старые бинарные форматы Office 97–2003: .doc, .ppt, .xls (и .xlsb).
//!
//! Контейнер OLE (CFB) читается крейтом `cfb`; текст достаётся из потоков
//! `WordDocument`/`PowerPoint Document` напрямую, таблицы Excel — через `calamine`.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use cfb::CompoundFile;

use super::Encrypted;
use super::doc::Doc;

const STREAM_CAP: u64 = 512 << 20;

pub fn extract(path: &Path, out: &mut Doc) -> Result<()> {
    let mut cf = cfb::open(path).context("не удалось прочитать OLE-контейнер")?;
    // Зашифрованный пакет OOXML тоже лежит в OLE-контейнере.
    if cf.exists("/EncryptedPackage") || cf.exists("/EncryptionInfo") {
        return Err(Encrypted.into());
    }
    if cf.is_stream("/WordDocument") {
        return word(&mut cf, out);
    }
    if cf.is_stream("/PowerPoint Document") {
        return powerpoint(&mut cf, out);
    }
    if cf.is_stream("/Workbook") || cf.is_stream("/Book") {
        drop(cf);
        return excel(path, false, out);
    }
    bail!("неизвестное содержимое OLE-контейнера")
}

fn read_stream(cf: &mut CompoundFile<File>, name: &str) -> Result<Vec<u8>> {
    let mut s = cf.open_stream(name).with_context(|| format!("нет потока {name}"))?;
    let mut v = Vec::new();
    s.by_ref().take(STREAM_CAP).read_to_end(&mut v)?;
    Ok(v)
}

fn u16le(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

// ---------------------------------------------------------------- Word 97–2003

fn word(cf: &mut CompoundFile<File>, out: &mut Doc) -> Result<()> {
    let wd = read_stream(cf, "/WordDocument")?;
    if u16le(&wd, 0) != Some(0xA5EC) {
        bail!("некорректный заголовок Word-документа");
    }
    let n_fib = u16le(&wd, 2).unwrap_or(0);
    let flags = u16le(&wd, 0x0A).unwrap_or(0);
    if flags & 0x0100 != 0 {
        return Err(Encrypted.into());
    }

    if n_fib < 0x00C1 {
        // Word 6/95: текст лежит одним куском в 8-битной кодировке.
        let (min, mac) = (u32le(&wd, 0x18).unwrap_or(0) as usize, u32le(&wd, 0x1C).unwrap_or(0) as usize);
        let bytes = wd.get(min..mac.min(wd.len())).context("некорректные границы текста")?;
        render_office_text(&decode_8bit_guess(bytes), out);
        return Ok(());
    }

    let table_name = if flags & 0x0200 != 0 { "/1Table" } else { "/0Table" };
    let table = read_stream(cf, table_name)?;
    let (text, ccp_text) = piece_table_text(&wd, &table).context("не удалось разобрать таблицу фрагментов текста")?;

    // Сначала основной текст, затем «прочие истории»: сноски, колонтитулы, примечания,
    // надписи. Колонтитулы хранятся по экземпляру на каждый тип страницы и
    // сильно повторяются, поэтому строки там сворачиваем.
    let split = text.char_indices().nth(ccp_text).map(|(i, _)| i).unwrap_or(text.len());
    let (main, rest) = text.split_at(if ccp_text == 0 { text.len() } else { split });
    render_office_text(main, out);
    if !rest.is_empty() {
        let mut extra = Doc::new(rest.len().min(1 << 22) + 16);
        render_office_text(rest, &mut extra);
        let mut seen = std::collections::HashSet::new();
        for line in extra.text.lines() {
            if seen.insert(line.trim().to_string()) {
                out.push(line);
                out.newline();
            }
        }
    }
    Ok(())
}

struct Fib {
    /// Положение и размер CLX (описание фрагментов текста) в табличном потоке.
    fc_clx: usize,
    lcb_clx: usize,
    /// Длина основного текста в символах; дальше идут сноски, колонтитулы и т. д.
    ccp_text: usize,
}

fn parse_fib(wd: &[u8]) -> Option<Fib> {
    let csw = u16le(wd, 32)? as usize;
    let mut pos = 34 + csw * 2;
    let cslw = u16le(wd, pos)? as usize;
    let rg_lw = pos + 2;
    let ccp_text = u32le(wd, rg_lw + 12)? as usize;
    pos = rg_lw + cslw * 4;
    let cb_rg_fc_lcb = u16le(wd, pos)? as usize;
    let blob = pos + 2;
    // fcClx — 34-я пара (fc, lcb) в FibRgFcLcb97.
    if cb_rg_fc_lcb <= 33 {
        return None;
    }
    Some(Fib { fc_clx: u32le(wd, blob + 33 * 8)? as usize, lcb_clx: u32le(wd, blob + 33 * 8 + 4)? as usize, ccp_text })
}

struct Piece {
    chars: usize,
    fc: usize,
    compressed: bool,
}

fn parse_pieces(clx: &[u8]) -> Option<Vec<Piece>> {
    let mut pos = 0;
    while pos < clx.len() {
        match clx[pos] {
            0x01 => pos += 3 + u16le(clx, pos + 1)? as usize,
            0x02 => {
                let lcb = u32le(clx, pos + 1)? as usize;
                let plc = clx.get(pos + 5..pos + 5 + lcb)?;
                let n = plc.len().checked_sub(4)? / 12;
                let mut pieces = Vec::with_capacity(n);
                for i in 0..n {
                    let (cp0, cp1) = (u32le(plc, 4 * i)?, u32le(plc, 4 * (i + 1))?);
                    let pcd = 4 * (n + 1) + 8 * i;
                    let fc = u32le(plc, pcd + 2)?;
                    let compressed = fc & 0x4000_0000 != 0;
                    let fc = (fc & 0x3FFF_FFFF) as usize;
                    pieces.push(Piece {
                        chars: cp1.saturating_sub(cp0) as usize,
                        fc: if compressed { fc / 2 } else { fc },
                        compressed,
                    });
                }
                return Some(pieces);
            }
            _ => return None,
        }
    }
    None
}

fn piece_table_text(wd: &[u8], table: &[u8]) -> Option<(String, usize)> {
    let fib = parse_fib(wd)?;
    let clx = table.get(fib.fc_clx..fib.fc_clx.checked_add(fib.lcb_clx)?)?;
    let pieces = parse_pieces(clx)?;
    let mut text = String::new();
    for p in pieces {
        if p.compressed {
            let bytes = wd.get(p.fc..(p.fc + p.chars).min(wd.len()))?;
            let (s, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            text.push_str(&s);
        } else {
            let bytes = wd.get(p.fc..(p.fc + p.chars * 2).min(wd.len()))?;
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            text.push_str(&String::from_utf16_lossy(&units));
        }
    }
    Some((text, fib.ccp_text))
}

fn decode_8bit_guess(bytes: &[u8]) -> String {
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(&bytes[..bytes.len().min(64 * 1024)], true);
    let enc = det.guess(None, chardetng::Utf8Detection::Deny);
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

/// Разбирает служебные символы Word/PowerPoint: абзацы, ячейки, коды полей.
fn render_office_text(text: &str, out: &mut Doc) {
    // true — мы внутри кода поля (между 0x13 и 0x14), его пропускаем; результат поля (после 0x14) оставляем.
    let mut fields: Vec<bool> = Vec::new();
    let mut in_code = 0usize;
    let mut after_cell_mark = false;
    for c in text.chars() {
        if out.is_full() {
            break;
        }
        match c {
            '\u{13}' => {
                fields.push(true);
                in_code += 1;
                continue;
            }
            '\u{14}' => {
                if let Some(top) = fields.last_mut()
                    && *top
                {
                    *top = false;
                    in_code -= 1;
                }
                continue;
            }
            '\u{15}' => {
                if fields.pop() == Some(true) {
                    in_code -= 1;
                }
                continue;
            }
            _ if in_code > 0 => continue,
            '\u{07}' => {
                // Конец ячейки; второй подряд знак — конец строки таблицы.
                if after_cell_mark {
                    out.newline();
                } else {
                    out.tab();
                }
                after_cell_mark = !after_cell_mark;
                continue;
            }
            _ => after_cell_mark = false,
        }
        match c {
            '\r' | '\u{0B}' | '\u{0C}' | '\n' => out.newline(),
            '\t' => out.tab(),
            '\u{1E}' => out.push_char('-'),
            c if c < ' ' => {}
            c => out.push_char(c),
        }
    }
    out.newline();
}

// ---------------------------------------------------------------- PowerPoint 97–2003

fn powerpoint(cf: &mut CompoundFile<File>, out: &mut Doc) -> Result<()> {
    let data = read_stream(cf, "/PowerPoint Document")?;
    walk_ppt(&data, out, 0);
    Ok(())
}

/// Текст слайда; блоки без букв и цифр — это поля-заполнители («*», «‹#›»).
fn render_slide_text(text: &str, out: &mut Doc) {
    if text.chars().any(char::is_alphanumeric) {
        render_office_text(text, out);
    }
}

/// Рекурсивный обход записей PPT: `TextCharsAtom` (UTF-16) и `TextBytesAtom` (8 бит).
fn walk_ppt(buf: &[u8], out: &mut Doc, depth: usize) {
    const TEXT_CHARS: u16 = 0x0FA0;
    const TEXT_BYTES: u16 = 0x0FA8;
    // Образцы слайдов, заметки-заготовки и окружение содержат служебные подписи
    // вроде «Click to edit Master title style».
    const SKIP: [u16; 3] = [0x03F8, 0x0FC9, 0x03F2];

    let mut pos = 0;
    while pos + 8 <= buf.len() && !out.is_full() {
        let ver_inst = u16le(buf, pos).unwrap_or(0);
        let ty = u16le(buf, pos + 2).unwrap_or(0);
        let len = u32le(buf, pos + 4).unwrap_or(0) as usize;
        let start = pos + 8;
        let end = start.saturating_add(len).min(buf.len());
        let body = &buf[start..end];

        if ver_inst & 0x000F == 0x000F {
            if depth < 32 && !SKIP.contains(&ty) {
                walk_ppt(body, out, depth + 1);
            }
        } else if ty == TEXT_CHARS {
            let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            render_slide_text(&String::from_utf16_lossy(&units), out);
        } else if ty == TEXT_BYTES {
            let (s, _, _) = encoding_rs::WINDOWS_1252.decode(body);
            render_slide_text(&s, out);
        }
        pos = end;
    }
}

// ---------------------------------------------------------------- Excel 97–2003 / xlsb

fn map_excel_error<E: std::fmt::Display>(e: E) -> anyhow::Error {
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    if lower.contains("password") || lower.contains("encrypt") {
        anyhow::Error::from(Encrypted)
    } else {
        anyhow::anyhow!("{msg}")
    }
}

/// Читает .xls (`xlsb == false`) или .xlsb. Формат задаётся явно — по содержимому, а не по расширению.
pub(super) fn excel(path: &Path, xlsb: bool, out: &mut Doc) -> Result<()> {
    use calamine::{Reader, Xls, Xlsb};
    let file = std::io::BufReader::new(File::open(path)?);
    if xlsb {
        dump_workbook(Xlsb::new(file).map_err(map_excel_error)?, out)
    } else {
        dump_workbook(Xls::new(file).map_err(map_excel_error)?, out)
    }
}

fn dump_workbook<RS, R>(mut wb: R, out: &mut Doc) -> Result<()>
where
    RS: std::io::Read + std::io::Seek,
    R: calamine::Reader<RS>,
{
    use calamine::Data;
    for name in wb.sheet_names() {
        if out.is_full() {
            break;
        }
        let Ok(range) = wb.worksheet_range(&name) else { continue };
        out.add_meta(&name);
        out.begin_unit(format!("Лист «{name}»"));
        for row in range.rows() {
            for cell in row {
                let text = match cell {
                    Data::String(s) => s.trim().to_string(),
                    Data::Int(i) => i.to_string(),
                    Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
                    Data::Float(f) => f.to_string(),
                    Data::DateTime(dt) => super::ooxml::excel_serial_to_string(dt.as_f64(), false).unwrap_or_default(),
                    Data::DateTimeIso(s) => s.clone(),
                    _ => String::new(),
                };
                if !text.is_empty() {
                    out.push(&text);
                    out.tab();
                }
            }
            out.newline();
            if out.is_full() {
                break;
            }
        }
    }
    Ok(())
}
