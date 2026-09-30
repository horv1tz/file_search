//! OpenDocument: .odt, .ods, .odp (и шаблоны) — LibreOffice / OpenOffice.

use std::io::Read;

use anyhow::Result;
use quick_xml::events::BytesStart;

use super::doc::Doc;
use super::ooxml::Pkg;
use super::xml::{Hooks, Rules, attr, collect_texts, extract_xml};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Sheet,
    Slides,
}

struct OdfHooks {
    kind: Kind,
    slide: usize,
}

impl Hooks for OdfHooks {
    fn start(&mut self, name: &str, e: &BytesStart<'_>, out: &mut Doc) {
        match (self.kind, name) {
            (Kind::Sheet, "table") => {
                let title = attr(e, "name").unwrap_or_default();
                out.add_meta(&title);
                out.begin_unit(format!("Лист «{title}»"));
            }
            (Kind::Slides, "page") => {
                self.slide += 1;
                out.begin_unit(format!("Слайд {}", self.slide));
            }
            _ => {}
        }
    }
}

const RULES: Rules = Rules {
    all_text_in: &["p", "h"],
    block_end: &["p", "h", "table-row"],
    cell_end: &["table-cell", "covered-table-cell"],
    inline_in: &["table-cell"],
    tab: &["tab"],
    br: &["line-break"],
    space: &["s"],
    // Поля вроде номера страницы содержат заглушки «<number>».
    skip: &["page-number", "page-count", "date", "time", "file-name"],
    ..Rules::EMPTY
};

pub fn extract_pkg(pkg: &mut Pkg, out: &mut Doc) -> Result<()> {
    let mut mime = String::new();
    if pkg.has("mimetype") {
        pkg.part("mimetype")?.read_to_string(&mut mime).ok();
    }
    let kind = if mime.contains("spreadsheet") {
        Kind::Sheet
    } else if mime.contains("presentation") || mime.contains("graphics") {
        Kind::Slides
    } else {
        Kind::Text
    };

    if pkg.has("meta.xml") {
        let r = pkg.part("meta.xml")?;
        for s in collect_texts(r, &["title", "subject", "creator", "initial-creator", "description", "keyword"]) {
            out.add_meta(&s);
        }
    }

    let r = pkg.part("content.xml")?;
    extract_xml(r, &RULES, &mut OdfHooks { kind, slide: 0 }, out)?;
    Ok(())
}
