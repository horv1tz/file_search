//! Потоковое извлечение текста из XML (OOXML, ODF).
//!
//! Один универсальный обход, настраиваемый набором [`Rules`] по локальным именам
//! элементов: какие элементы содержат текст, где заканчивается абзац или ячейка,
//! какие поддеревья пропускать.

use std::io::BufRead;

use anyhow::Result;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::doc::Doc;

pub struct Rules {
    /// Элементы, символьные данные которых — текст (`w:t`, `a:t`).
    pub text: &'static [&'static str],
    /// Элементы, внутри которых любой текст считается содержимым (ODF `text:p`).
    pub all_text_in: &'static [&'static str],
    /// Конец элемента — перевод строки.
    pub block_end: &'static [&'static str],
    /// Конец элемента — разделитель ячеек.
    pub cell_end: &'static [&'static str],
    /// Начало элемента — табуляция.
    pub tab: &'static [&'static str],
    /// Начало элемента — перевод строки.
    pub br: &'static [&'static str],
    /// Начало элемента — пробел.
    pub space: &'static [&'static str],
    /// Поддерево целиком пропускается.
    pub skip: &'static [&'static str],
    /// Внутри этих элементов (ячейки таблиц) конец абзаца — пробел, а не перевод строки.
    pub inline_in: &'static [&'static str],
}

impl Rules {
    pub const EMPTY: Rules = Rules {
        text: &[],
        all_text_in: &[],
        block_end: &[],
        cell_end: &[],
        tab: &[],
        br: &[],
        space: &[],
        skip: &[],
        inline_in: &[],
    };
}

/// Реакция на открывающие теги (например, начало листа или слайда).
pub trait Hooks {
    fn start(&mut self, _local_name: &str, _e: &BytesStart<'_>, _out: &mut Doc) {}
}

pub struct NoHooks;
impl Hooks for NoHooks {}

fn is(name: &str, set: &[&str]) -> bool {
    set.contains(&name)
}

/// Значение атрибута по локальному имени (`r:id` → `id`).
pub fn attr(e: &BytesStart<'_>, local: &str) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if a.key.local_name().as_ref() == local {
            return a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned());
        }
    }
    None
}

/// Идентификатор связи: атрибут с префиксом пространства имён (`r:id`),
/// в отличие от числового `id` без префикса.
pub fn rel_id_attr(e: &BytesStart<'_>) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        let key: &str = a.key.as_ref();
        if key.contains(':') && !key.starts_with("xml:") && a.key.local_name().as_ref() == "id" {
            return a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned());
        }
    }
    None
}

pub fn new_reader<R: BufRead>(src: R) -> Reader<R> {
    let mut reader = Reader::from_reader(src);
    reader.config_mut().allow_dangling_amp = true;
    reader
}

/// Подставляет стандартную сущность (`&amp;` и т. п.) или числовую ссылку.
pub fn resolve_ref(r: &quick_xml::events::BytesRef<'_>, out: &mut String) {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        out.push(c);
        return;
    }
    match &**r {
        "amp" => out.push('&'),
        "lt" => out.push('<'),
        "gt" => out.push('>'),
        "quot" => out.push('"'),
        "apos" => out.push('\''),
        _ => {}
    }
}

pub fn extract_xml<R: BufRead, H: Hooks>(
    src: R,
    rules: &Rules,
    hooks: &mut H,
    out: &mut Doc,
) -> Result<()> {
    let mut reader = new_reader(src);
    let mut buf = Vec::new();
    let mut scratch = String::new();
    let (mut text_depth, mut all_depth, mut skip_depth, mut inline_depth) = (0u32, 0u32, 0u32, 0u32);

    loop {
        if out.is_full() {
            break;
        }
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.local_name();
                let n = name.as_ref();
                if skip_depth > 0 {
                    skip_depth += 1;
                } else if is(n, rules.skip) {
                    skip_depth = 1;
                } else {
                    hooks.start(n, &e, out);
                    if is(n, rules.text) {
                        text_depth += 1;
                    }
                    if is(n, rules.all_text_in) {
                        all_depth += 1;
                    }
                    if is(n, rules.inline_in) {
                        inline_depth += 1;
                    }
                    inline_start(n, rules, out);
                }
            }
            Event::Empty(e) => {
                let name = e.local_name();
                let n = name.as_ref();
                if skip_depth == 0 && !is(n, rules.skip) {
                    hooks.start(n, &e, out);
                    inline_start(n, rules, out);
                    block_end(n, rules, inline_depth > 0, out);
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                let n = name.as_ref();
                if skip_depth > 0 {
                    skip_depth -= 1;
                } else {
                    if is(n, rules.text) {
                        text_depth = text_depth.saturating_sub(1);
                    }
                    if is(n, rules.all_text_in) {
                        all_depth = all_depth.saturating_sub(1);
                    }
                    if is(n, rules.inline_in) {
                        inline_depth = inline_depth.saturating_sub(1);
                    }
                    block_end(n, rules, inline_depth > 0, out);
                }
            }
            Event::Text(t) => {
                if skip_depth == 0 && (text_depth > 0 || all_depth > 0) {
                    out.push(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if skip_depth == 0 && (text_depth > 0 || all_depth > 0) {
                    out.push(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if skip_depth == 0 && (text_depth > 0 || all_depth > 0) {
                    scratch.clear();
                    resolve_ref(&r, &mut scratch);
                    out.push(&scratch);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn inline_start(n: &str, rules: &Rules, out: &mut Doc) {
    if is(n, rules.tab) {
        out.tab();
    } else if is(n, rules.br) {
        out.newline();
    } else if is(n, rules.space) {
        out.space();
    }
}

fn block_end(n: &str, rules: &Rules, inline: bool, out: &mut Doc) {
    if is(n, rules.block_end) {
        if inline { out.space() } else { out.newline() }
    } else if is(n, rules.cell_end) {
        out.tab();
    }
}

/// Быстрое извлечение всего текста из небольшой XML-части (для метаданных).
pub fn collect_texts<R: BufRead>(src: R, names: &'static [&'static str]) -> Vec<String> {
    let rules = Rules {
        text: names,
        block_end: names,
        ..Rules::EMPTY
    };
    let mut d = Doc::new(64 * 1024);
    let _ = extract_xml(src, &rules, &mut NoHooks, &mut d);
    d.text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}
