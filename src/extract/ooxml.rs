//! Office Open XML: .docx/.docm, .xlsx/.xlsm, .pptx/.pptm (и шаблоны/показы).
//!
//! Файлы читаются потоково прямо из zip-архива, без загрузки документа целиком.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration, NaiveDate};
use quick_xml::events::Event;
use zip::ZipArchive;
use zip::result::ZipError;

use super::Encrypted;
use super::doc::Doc;
use super::xml::{self, NoHooks, Rules, attr, extract_xml, new_reader, rel_id_attr, resolve_ref};

/// Верхняя граница на распакованный размер одной части (защита от zip-бомб).
const PART_CAP: u64 = 4 << 30;

pub struct Pkg {
    zip: ZipArchive<BufReader<File>>,
    names: Vec<String>,
}

type PartReader<'a> = BufReader<std::io::Take<zip::read::ZipFile<'a, BufReader<File>>>>;

impl Pkg {
    pub fn open(path: &Path) -> Result<Pkg> {
        let file = File::open(path).with_context(|| format!("не удалось открыть {}", path.display()))?;
        let zip = ZipArchive::new(BufReader::with_capacity(64 * 1024, file)).context("некорректный zip-архив")?;
        let names = zip.file_names().map(str::to_string).collect();
        Ok(Pkg { zip, names })
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// Имена частей, подходящие под предикат, в стабильном порядке.
    pub fn names_where(&self, pred: impl Fn(&str) -> bool) -> Vec<String> {
        let mut v: Vec<String> = self.names.iter().filter(|n| pred(n)).cloned().collect();
        v.sort_by_key(|a| natural_key(a));
        v
    }

    pub fn part(&mut self, name: &str) -> Result<PartReader<'_>> {
        match self.zip.by_name(name) {
            Ok(f) => Ok(BufReader::with_capacity(64 * 1024, f.take(PART_CAP))),
            Err(ZipError::UnsupportedArchive(m)) if m.to_lowercase().contains("password") => Err(Encrypted.into()),
            Err(e) => bail!("часть {name}: {e}"),
        }
    }

    fn part_opt(&mut self, name: &str) -> Option<PartReader<'_>> {
        if self.has(name) { self.part(name).ok() } else { None }
    }
}

/// Ключ для «естественной» сортировки: slide2 < slide10.
fn natural_key(s: &str) -> (String, u64) {
    let stem = s.trim_end_matches(".xml");
    let digits: String =
        stem.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    let prefix = &stem[..stem.len() - digits.len()];
    (prefix.to_string(), digits.parse().unwrap_or(0))
}

// ---------------------------------------------------------------- связи (.rels)

struct Rel {
    id: String,
    ty: String,
    target: String,
}

fn dir_of(part: &str) -> &str {
    part.rfind('/').map(|i| &part[..i]).unwrap_or("")
}

fn file_of(part: &str) -> &str {
    part.rfind('/').map(|i| &part[i + 1..]).unwrap_or(part)
}

/// Разрешает относительную ссылку из .rels в полное имя части архива.
fn resolve(base_part: &str, target: &str) -> String {
    let joined = match target.strip_prefix('/') {
        Some(abs) => abs.to_string(),
        None => {
            let dir = dir_of(base_part);
            if dir.is_empty() { target.to_string() } else { format!("{dir}/{target}") }
        }
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

fn read_rels(pkg: &mut Pkg, part: &str) -> Vec<Rel> {
    let rels_name = if part.is_empty() {
        "_rels/.rels".to_string()
    } else {
        let d = dir_of(part);
        let prefix = if d.is_empty() { String::new() } else { format!("{d}/") };
        format!("{prefix}_rels/{}.rels", file_of(part))
    };
    let Some(r) = pkg.part_opt(&rels_name) else { return Vec::new() };
    let mut reader = new_reader(r);
    let mut buf = Vec::new();
    let mut rels = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                if attr(&e, "TargetMode").as_deref() == Some("External") {
                    buf.clear();
                    continue;
                }
                if let (Some(id), Some(ty), Some(target)) = (attr(&e, "Id"), attr(&e, "Type"), attr(&e, "Target")) {
                    rels.push(Rel { id, ty, target: resolve(part, &target) });
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    rels
}

fn rel_type_is(rel: &Rel, suffix: &str) -> bool {
    rel.ty.rsplit('/').next() == Some(suffix)
}

/// Часть-«главный документ» пакета (`word/document.xml`, `xl/workbook.xml`, ...).
fn main_part(pkg: &mut Pkg) -> Option<String> {
    let rels = read_rels(pkg, "");
    rels.into_iter().find(|r| rel_type_is(r, "officeDocument")).map(|r| r.target)
}

// ---------------------------------------------------------------- общий код

fn process(pkg: &mut Pkg, name: &str, rules: &Rules, out: &mut Doc) -> Result<()> {
    let r = pkg.part(name)?;
    extract_xml(r, rules, &mut NoHooks, out)?;
    out.newline();
    Ok(())
}

fn core_meta(pkg: &mut Pkg, out: &mut Doc) {
    if let Some(r) = pkg.part_opt("docProps/core.xml") {
        for s in xml::collect_texts(r, &["title", "subject", "creator", "keywords", "description", "category"]) {
            out.add_meta(&s);
        }
    }
}

/// Определяет тип пакета по главной части и извлекает текст.
pub fn extract_pkg(pkg: &mut Pkg, out: &mut Doc) -> Result<()> {
    let main = match main_part(pkg) {
        Some(m) => m,
        // Нет корневых связей — пробуем угадать по стандартным именам.
        None => ["word/document.xml", "xl/workbook.xml", "ppt/presentation.xml"]
            .into_iter()
            .find(|n| pkg.has(n))
            .map(str::to_string)
            .context("не найдена главная часть документа")?,
    };
    core_meta(pkg, out);
    match dir_of(&main) {
        "word" => docx(pkg, &main, out),
        "xl" => xlsx(pkg, &main, out),
        "ppt" => pptx(pkg, &main, out),
        _ if main.ends_with("workbook.xml") => xlsx(pkg, &main, out),
        _ if main.ends_with("presentation.xml") => pptx(pkg, &main, out),
        _ => docx(pkg, &main, out),
    }
}

// ---------------------------------------------------------------- Word

const DOCX_RULES: Rules = Rules {
    text: &["t"],
    block_end: &["p", "tr"],
    cell_end: &["tc"],
    inline_in: &["tc"],
    tab: &["tab"],
    br: &["br", "cr"],
    // pPr содержит w:tabs/w:tab — это позиции табуляции, а не символы;
    // Fallback дублирует содержимое mc:Choice (текстовые рамки).
    skip: &["pPr", "Fallback"],
    ..Rules::EMPTY
};

fn docx(pkg: &mut Pkg, main: &str, out: &mut Doc) -> Result<()> {
    process(pkg, main, &DOCX_RULES, out)?;
    let dir = dir_of(main).to_string();
    let extras = pkg.names_where(|n| {
        let Some(rest) = n.strip_prefix(&format!("{dir}/")) else { return false };
        if rest.contains('/') || !rest.ends_with(".xml") {
            return false;
        }
        let stem = rest.trim_end_matches(".xml").trim_end_matches(|c: char| c.is_ascii_digit());
        matches!(stem, "header" | "footer" | "footnotes" | "endnotes" | "comments")
    });
    for name in extras {
        let _ = process(pkg, &name, &DOCX_RULES, out);
    }
    // Текст схем SmartArt лежит в отдельных частях, на которые ссылается главный документ.
    for rel in read_rels(pkg, main) {
        if rel_type_is(&rel, "diagramData") {
            let _ = process(pkg, &rel.target, &PPTX_RULES, out);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- PowerPoint

const PPTX_RULES: Rules = Rules {
    text: &["t"],
    block_end: &["p", "tr"],
    cell_end: &["tc"],
    inline_in: &["tc"],
    br: &["br"],
    // fld — номер слайда/дата, Fallback — дубли и картинки.
    skip: &["fld", "Fallback"],
    ..Rules::EMPTY
};

const PPTX_COMMENT_RULES: Rules = Rules { text: &["t", "text"], block_end: &["p", "cm"], br: &["br"], ..Rules::EMPTY };

fn pptx(pkg: &mut Pkg, main: &str, out: &mut Doc) -> Result<()> {
    let rels = read_rels(pkg, main);
    let slide_rids = {
        let mut rids = Vec::new();
        if let Ok(r) = pkg.part(main) {
            let mut reader = new_reader(r);
            let mut buf = Vec::new();
            while let Ok(ev) = reader.read_event_into(&mut buf) {
                match ev {
                    Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "sldId" => {
                        if let Some(rid) = rel_id_attr(&e) {
                            rids.push(rid);
                        }
                    }
                    Event::Eof => break,
                    _ => {}
                }
                buf.clear();
            }
        }
        rids
    };
    let by_id: HashMap<&str, &Rel> = rels.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut slides: Vec<String> = slide_rids
        .iter()
        .filter_map(|rid| by_id.get(rid.as_str()))
        .filter(|r| rel_type_is(r, "slide"))
        .map(|r| r.target.clone())
        .collect();
    if slides.is_empty() {
        slides = pkg.names_where(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"));
    }

    for (i, slide) in slides.iter().enumerate() {
        if out.is_full() {
            break;
        }
        out.begin_unit(format!("Слайд {}", i + 1));
        let _ = process(pkg, slide, &PPTX_RULES, out);
        for rel in read_rels(pkg, slide) {
            if rel_type_is(&rel, "notesSlide") {
                let _ = process(pkg, &rel.target, &PPTX_RULES, out);
            } else if rel_type_is(&rel, "comments") || rel_type_is(&rel, "diagramData") {
                let _ = process(pkg, &rel.target, &PPTX_COMMENT_RULES, out);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- Excel

#[derive(Default)]
struct Styles {
    /// Для каждого xf (индекс стиля ячейки): это формат даты/времени?
    xf_is_date: Vec<bool>,
}

fn is_date_format(id: u32, code: Option<&str>) -> bool {
    if matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58) {
        return true;
    }
    let Some(code) = code else { return false };
    let mut in_quote = false;
    let mut in_bracket = false;
    let mut skip_next = false;
    for c in code.chars() {
        if skip_next {
            skip_next = false;
            continue;
        }
        match c {
            '"' => in_quote = !in_quote,
            '[' if !in_quote => in_bracket = true,
            ']' if !in_quote => in_bracket = false,
            '\\' | '_' | '*' if !in_quote => skip_next = true,
            c if !in_quote && !in_bracket && matches!(c.to_ascii_lowercase(), 'y' | 'd' | 'm' | 'h' | 's') => {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn read_styles<R: BufRead>(src: R) -> Styles {
    let mut reader = new_reader(src);
    let mut buf = Vec::new();
    let mut custom: HashMap<u32, String> = HashMap::new();
    let mut xf_ids: Vec<u32> = Vec::new();
    let mut in_cell_xfs = false;
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "numFmt" => {
                    if let (Some(id), Some(code)) = (attr(&e, "numFmtId"), attr(&e, "formatCode"))
                        && let Ok(id) = id.parse()
                    {
                        custom.insert(id, code);
                    }
                }
                "cellXfs" => in_cell_xfs = true,
                "xf" if in_cell_xfs => {
                    xf_ids.push(attr(&e, "numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0));
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "cellXfs" => in_cell_xfs = false,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Styles { xf_is_date: xf_ids.iter().map(|id| is_date_format(*id, custom.get(id).map(String::as_str))).collect() }
}

fn read_shared_strings<R: BufRead>(src: R, limit: usize) -> Vec<String> {
    let mut reader = new_reader(src);
    let mut buf = Vec::new();
    let mut strings = Vec::new();
    let mut cur = String::new();
    let (mut in_t, mut phonetic) = (false, 0u32);
    let mut total = 0usize;
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) => match e.local_name().as_ref() {
                "si" => cur.clear(),
                "t" if phonetic == 0 => in_t = true,
                "rPh" => phonetic += 1,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "si" => {
                    total += cur.len();
                    strings.push(std::mem::take(&mut cur));
                    if total > limit {
                        break;
                    }
                }
                "t" => in_t = false,
                "rPh" => phonetic = phonetic.saturating_sub(1),
                _ => {}
            },
            Event::Text(t) if in_t => cur.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_t => resolve_ref(&r, &mut cur),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    strings
}

struct Workbook {
    /// (имя листа, r:id)
    sheets: Vec<(String, String)>,
    date1904: bool,
}

fn read_workbook<R: BufRead>(src: R) -> Workbook {
    let mut reader = new_reader(src);
    let mut buf = Vec::new();
    let mut wb = Workbook { sheets: Vec::new(), date1904: false };
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "sheet" => {
                    let rid = rel_id_attr(&e);
                    if let (Some(name), Some(rid)) = (attr(&e, "name"), rid) {
                        wb.sheets.push((name, rid));
                    }
                }
                "workbookPr" => {
                    wb.date1904 = matches!(attr(&e, "date1904").as_deref(), Some("1" | "true"));
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    wb
}

#[derive(Clone, Copy, PartialEq)]
enum CellType {
    Number,
    Shared,
    Text,
    Skip,
}

pub(super) fn excel_serial_to_string(serial: f64, date1904: bool) -> Option<String> {
    if !serial.is_finite() || serial < 0.0 {
        return None;
    }
    let days = serial.floor();
    let secs = ((serial - days) * 86_400.0).round() as i64;
    let (hh, mm) = (secs / 3600 % 24, secs / 60 % 60);
    if days < 1.0 && !date1904 {
        return Some(format!("{hh:02}:{mm:02}"));
    }
    let epoch = if date1904 { NaiveDate::from_ymd_opt(1904, 1, 1)? } else { NaiveDate::from_ymd_opt(1899, 12, 30)? };
    let date = epoch.checked_add_signed(Duration::days(days as i64))?;
    if !(1900..=2200).contains(&date.year()) {
        return None;
    }
    let (y, m, d) = (date.year(), date.month(), date.day());
    // Обе записи: по ним ищут и «15.03.2024», и «2024-03-15».
    let mut s = format!("{d:02}.{m:02}.{y} {y}-{m:02}-{d:02}");
    if secs > 0 {
        s.push_str(&format!(" {hh:02}:{mm:02}"));
    }
    Some(s)
}

fn format_number(raw: &str, is_date: bool, date1904: bool) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed: Option<f64> = raw.parse().ok();
    if is_date && let Some(s) = parsed.and_then(|v| excel_serial_to_string(v, date1904)) {
        return Some(s);
    }
    match parsed {
        Some(v) if v.fract() == 0.0 && v.abs() < 1e15 => Some(format!("{}", v as i64)),
        _ => Some(raw.to_string()),
    }
}

fn read_sheet<R: BufRead>(src: R, shared: &[String], styles: &Styles, date1904: bool, out: &mut Doc) -> Result<()> {
    let mut reader = new_reader(src);
    let mut buf = Vec::new();
    let mut ty = CellType::Number;
    let mut style = 0usize;
    let mut value = String::new();
    let (mut in_v, mut in_t, mut in_is, mut phonetic) = (false, false, false, 0u32);

    loop {
        if out.is_full() {
            break;
        }
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "c" => {
                    value.clear();
                    style = attr(&e, "s").and_then(|s| s.parse().ok()).unwrap_or(0);
                    ty = match attr(&e, "t").as_deref() {
                        Some("s") => CellType::Shared,
                        Some("str" | "inlineStr" | "d") => CellType::Text,
                        Some("b" | "e") => CellType::Skip,
                        _ => CellType::Number,
                    };
                }
                "v" => in_v = true,
                "is" => in_is = true,
                "t" if in_is && phonetic == 0 => in_t = true,
                "rPh" => phonetic += 1,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "c" => {
                    let text: Option<String> = match ty {
                        CellType::Shared => value.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).cloned(),
                        CellType::Text => Some(value.clone()),
                        CellType::Number => {
                            let is_date = styles.xf_is_date.get(style).copied().unwrap_or(false);
                            format_number(&value, is_date, date1904)
                        }
                        CellType::Skip => None,
                    };
                    if let Some(t) = text {
                        let t = t.trim();
                        if !t.is_empty() {
                            out.push(t);
                            out.tab();
                        }
                    }
                }
                "v" => in_v = false,
                "t" => in_t = false,
                "is" => in_is = false,
                "rPh" => phonetic = phonetic.saturating_sub(1),
                "row" => out.newline(),
                _ => {}
            },
            Event::Text(t) if in_v || in_t => value.push_str(&t.xml10_content()),
            Event::CData(t) if in_v || in_t => value.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_v || in_t => resolve_ref(&r, &mut value),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out.newline();
    Ok(())
}

const COMMENT_RULES: Rules = Rules { text: &["t"], block_end: &["comment"], skip: &["rPh"], ..Rules::EMPTY };

const THREADED_RULES: Rules = Rules { text: &["text"], block_end: &["threadedComment"], ..Rules::EMPTY };

const DRAWING_RULES: Rules =
    Rules { text: &["t"], block_end: &["p"], br: &["br"], skip: &["Fallback"], ..Rules::EMPTY };

fn xlsx(pkg: &mut Pkg, main: &str, out: &mut Doc) -> Result<()> {
    let wb_rels = read_rels(pkg, main);
    let wb = read_workbook(pkg.part(main)?);

    let shared = {
        let target = wb_rels
            .iter()
            .find(|r| rel_type_is(r, "sharedStrings"))
            .map(|r| r.target.clone())
            .unwrap_or_else(|| format!("{}/sharedStrings.xml", dir_of(main)));
        match pkg.part_opt(&target) {
            Some(r) => read_shared_strings(r, 256 << 20),
            None => Vec::new(),
        }
    };
    let styles = {
        let target = wb_rels
            .iter()
            .find(|r| rel_type_is(r, "styles"))
            .map(|r| r.target.clone())
            .unwrap_or_else(|| format!("{}/styles.xml", dir_of(main)));
        pkg.part_opt(&target).map(read_styles).unwrap_or_default()
    };

    let by_id: HashMap<&str, &Rel> = wb_rels.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut sheets: Vec<(String, String)> = wb
        .sheets
        .iter()
        .filter_map(|(name, rid)| {
            by_id.get(rid.as_str()).filter(|r| rel_type_is(r, "worksheet")).map(|r| (name.clone(), r.target.clone()))
        })
        .collect();
    if sheets.is_empty() {
        sheets = pkg
            .names_where(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
            .into_iter()
            .enumerate()
            .map(|(i, n)| (format!("Лист{}", i + 1), n))
            .collect();
    }

    for (name, target) in sheets {
        if out.is_full() {
            break;
        }
        out.begin_unit(format!("Лист «{name}»"));
        out.add_meta(&name);
        match pkg.part(&target) {
            Ok(r) => {
                let _ = read_sheet(r, &shared, &styles, wb.date1904, out);
            }
            Err(e) if e.downcast_ref::<Encrypted>().is_some() => return Err(e),
            Err(_) => continue,
        }
        let rels = read_rels(pkg, &target);
        let has_threaded = rels.iter().any(|r| rel_type_is(r, "threadedComment"));
        for rel in &rels {
            if rel_type_is(rel, "threadedComment") {
                let _ = process(pkg, &rel.target, &THREADED_RULES, out);
            } else if rel_type_is(rel, "comments") && !has_threaded {
                // В «старых» комментариях к цепочкам лежит заглушка про новую версию Excel.
                let _ = process(pkg, &rel.target, &COMMENT_RULES, out);
            } else if rel_type_is(rel, "drawing") {
                let _ = process(pkg, &rel.target, &DRAWING_RULES, out);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_targets() {
        assert_eq!(resolve("xl/workbook.xml", "worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(
            resolve("ppt/slides/slide1.xml", "../notesSlides/notesSlide1.xml"),
            "ppt/notesSlides/notesSlide1.xml"
        );
        assert_eq!(resolve("", "/word/document.xml"), "word/document.xml");
        assert_eq!(resolve("", "word/document.xml"), "word/document.xml");
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["ppt/slides/slide10.xml", "ppt/slides/slide2.xml", "ppt/slides/slide1.xml"];
        v.sort_by_key(|a| natural_key(a));
        assert_eq!(v, ["ppt/slides/slide1.xml", "ppt/slides/slide2.xml", "ppt/slides/slide10.xml"]);
    }

    #[test]
    fn date_formats() {
        assert!(is_date_format(14, None));
        assert!(is_date_format(164, Some("dd\\.mm\\.yyyy")));
        assert!(is_date_format(165, Some("[$-409]d-mmm-yy;@")));
        assert!(!is_date_format(166, Some("#,##0.00\" р.\"")));
        assert!(!is_date_format(0, Some("General")));
        assert!(!is_date_format(9, None));
    }

    #[test]
    fn serial_dates() {
        assert_eq!(excel_serial_to_string(45366.0, false).unwrap(), "15.03.2024 2024-03-15");
        assert_eq!(excel_serial_to_string(45366.5, false).unwrap(), "15.03.2024 2024-03-15 12:00");
        assert_eq!(excel_serial_to_string(0.75, false).unwrap(), "18:00");
    }

    #[test]
    fn numbers_lose_trailing_zero_fraction() {
        assert_eq!(format_number("12345.0", false, false).unwrap(), "12345");
        assert_eq!(format_number("3.14", false, false).unwrap(), "3.14");
    }
}
