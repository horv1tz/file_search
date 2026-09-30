//! Язык запросов: разбор строки и построение запроса tantivy.
//!
//! ```text
//! договор аренды        оба слова (в имени, содержимом или метаданных)
//! "срок действия"       точная фраза
//! отчёт -черновик       исключить слово
//! догов*                префикс
//! договр~               нечёткий поиск (опечатки), ~2 — до двух правок
//! аренда OR лизинг      любое из двух
//! name:отчёт            только в имени файла (подстрока)
//! content:аренда        только в содержимом
//! path:бухгалтерия      в пути к файлу
//! ext:docx,xlsx         только файлы этих типов
//! ```

use std::ops::Bound;

use anyhow::Result;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, RangeQuery,
    RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Term};
use tantivy::tokenizer::TextAnalyzer;

use crate::analyzer::analyze;
use crate::schema::{Fields, path_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Имя файла, метаданные и содержимое.
    #[default]
    All,
    /// Только имя файла.
    Name,
    /// Только содержимое.
    Content,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Any,
    Name,
    Content,
    Path,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    Word(String),
    Phrase(String),
    Prefix(String),
    Fuzzy(String, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    pub negate: bool,
    pub scope: Scope,
    pub text: Text,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// Условия соединены через И; внутри группы — через ИЛИ.
    pub groups: Vec<Vec<Clause>>,
    pub negatives: Vec<Clause>,
    pub exts: Vec<String>,
}

impl Parsed {
    pub fn has_positive(&self) -> bool {
        !self.groups.is_empty()
    }
}

/// Делит строку на «слова» по пробелам, не разрывая кавычки.
fn split_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in input.chars() {
        match c {
            '"' | '«' | '»' | '“' | '”' => {
                in_quotes = !in_quotes;
                cur.push('"');
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

pub fn parse(input: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut or_next = false;

    for raw in split_tokens(input) {
        if raw == "OR" || raw == "||" {
            or_next = !parsed.groups.is_empty();
            continue;
        }
        let mut tok = raw.as_str();
        let mut negate = false;
        if let Some(rest) = tok.strip_prefix('-').filter(|r| !r.is_empty()) {
            negate = true;
            tok = rest;
        } else if let Some(rest) = tok.strip_prefix('+').filter(|r| !r.is_empty()) {
            tok = rest;
        }

        let mut scope = Scope::Any;
        for (prefix, s) in [
            ("name:", Scope::Name),
            ("имя:", Scope::Name),
            ("content:", Scope::Content),
            ("text:", Scope::Content),
            ("текст:", Scope::Content),
            ("path:", Scope::Path),
            ("путь:", Scope::Path),
            ("meta:", Scope::Meta),
            ("title:", Scope::Meta),
        ] {
            if let Some(rest) = tok.strip_prefix(prefix) {
                scope = s;
                tok = rest;
                break;
            }
        }
        if let Some(list) = tok.strip_prefix("ext:").or_else(|| tok.strip_prefix("тип:")) {
            if !negate {
                parsed.exts.extend(
                    list.split(',')
                        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
                        .filter(|e| !e.is_empty()),
                );
            }
            continue;
        }

        let text = if let Some(inner) = tok.strip_prefix('"') {
            let phrase = inner.trim_end_matches('"').trim().to_string();
            if phrase.is_empty() {
                continue;
            }
            Text::Phrase(phrase)
        } else if let Some((word, dist)) = tok.rsplit_once('~').filter(|(w, _)| !w.is_empty()) {
            match dist {
                "" => Text::Fuzzy(word.to_string(), 1),
                d => match d.parse::<u8>() {
                    Ok(n) => Text::Fuzzy(word.to_string(), n.clamp(1, 2)),
                    Err(_) => Text::Word(tok.to_string()),
                },
            }
        } else if let Some(prefix) = tok.strip_suffix('*').filter(|p| !p.is_empty()) {
            Text::Prefix(prefix.to_string())
        } else {
            Text::Word(tok.to_string())
        };

        let clause = Clause { negate, scope, text };
        if negate {
            parsed.negatives.push(clause);
        } else if or_next {
            if let Some(last) = parsed.groups.last_mut() {
                last.push(clause);
            }
            or_next = false;
        } else {
            parsed.groups.push(vec![clause]);
        }
    }
    parsed
}

/// Экранирует спецсимволы регулярных выражений.
pub fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub struct Filters {
    pub exts: Vec<String>,
    /// Папки; совпадение по префиксу пути.
    pub dirs: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Границы времени изменения, миллисекунды с 1970 года.
    pub modified_after: Option<u64>,
    pub modified_before: Option<u64>,
}

pub struct Compiler<'a> {
    pub fields: &'a Fields,
    pub analyzer: &'a mut TextAnalyzer,
    /// Тот же разбор без стемминга.
    pub raw_analyzer: &'a mut TextAnalyzer,
    pub mode: Mode,
}

type BoxQuery = Box<dyn Query>;

impl Compiler<'_> {
    fn effective_scope(&self, scope: Scope) -> Scope {
        match (scope, self.mode) {
            (Scope::Any, Mode::Name) => Scope::Name,
            (Scope::Any, Mode::Content) => Scope::Content,
            (s, _) => s,
        }
    }

    /// `alt` — исходная (нестеммированная) форма одиночного слова: стеммер Snowball иногда
    /// «съедает» окончание у слов в начальной форме («гиппопотам» → «гиппопот»), и тогда
    /// оно не совпало бы со своими же формами («гиппопотама» → «гиппопотам»).
    fn tokens_query(&self, field: Field, tokens: &[String], alt: Option<&str>, positions: bool, boost: f32) -> Option<BoxQuery> {
        let terms: Vec<Term> = tokens.iter().map(|t| Term::from_field_text(field, t)).collect();
        let q: BoxQuery = match terms.len() {
            0 => return None,
            1 => {
                let term_query = |t: Term| Box::new(TermQuery::new(t, IndexRecordOption::WithFreqs)) as BoxQuery;
                let stem = terms.into_iter().next().unwrap();
                match alt.filter(|a| *a != tokens[0]) {
                    Some(a) => Box::new(BooleanQuery::new(vec![
                        (Occur::Should, term_query(stem)),
                        (Occur::Should, term_query(Term::from_field_text(field, a))),
                    ])),
                    None => term_query(stem),
                }
            }
            _ if positions => Box::new(PhraseQuery::new(terms)),
            _ => Box::new(BooleanQuery::new(
                terms
                    .into_iter()
                    .map(|t| (Occur::Must, Box::new(TermQuery::new(t, IndexRecordOption::Basic)) as BoxQuery))
                    .collect(),
            )),
        };
        Some(boost_query(q, boost))
    }

    fn regex_query(&self, field: Field, pattern: &str, boost: f32) -> Option<BoxQuery> {
        let q = RegexQuery::from_pattern(pattern, field).ok()?;
        Some(Box::new(ConstScoreQuery::new(Box::new(q), boost)))
    }

    fn clause_query(&mut self, clause: &Clause) -> Option<BoxQuery> {
        let f = *self.fields;
        let scope = self.effective_scope(clause.scope);
        let mut should: Vec<BoxQuery> = Vec::new();

        match &clause.text {
            Text::Word(w) | Text::Phrase(w) => {
                let tokens = analyze(self.analyzer, w);
                let raw = analyze(self.raw_analyzer, w);
                let alt = (tokens.len() == 1 && raw.len() == 1).then(|| raw[0].as_str());
                if matches!(scope, Scope::Any | Scope::Content) {
                    should.extend(self.tokens_query(f.content, &tokens, alt, true, 1.0));
                }
                if matches!(scope, Scope::Any | Scope::Name | Scope::Path) {
                    should.extend(self.tokens_query(f.name, &tokens, alt, true, 4.0));
                }
                if matches!(scope, Scope::Any | Scope::Meta) {
                    should.extend(self.tokens_query(f.meta, &tokens, alt, true, 2.5));
                }
                if matches!(scope, Scope::Any | Scope::Path) {
                    should.extend(self.tokens_query(f.folder, &tokens, alt, false, 1.0));
                }
                // Подстрока в имени: «отч» найдёт «квартальный_отчёт.xlsx».
                let lc = w.to_lowercase();
                if matches!(scope, Scope::Any | Scope::Name) && matches!(clause.text, Text::Word(_)) && lc.chars().count() >= 2 {
                    should.extend(self.regex_query(f.name_lc, &format!(".*{}.*", regex_escape(&lc)), 3.0));
                }
            }
            Text::Prefix(p) => {
                let p = p.to_lowercase().replace('ё', "е");
                let pattern = format!("{}.*", regex_escape(&p));
                if matches!(scope, Scope::Any | Scope::Content) {
                    should.extend(self.regex_query(f.content, &pattern, 1.0));
                }
                if matches!(scope, Scope::Any | Scope::Name | Scope::Path) {
                    should.extend(self.regex_query(f.name, &pattern, 3.0));
                }
                if matches!(scope, Scope::Any | Scope::Meta) {
                    should.extend(self.regex_query(f.meta, &pattern, 2.0));
                }
                if matches!(scope, Scope::Any | Scope::Path) {
                    should.extend(self.regex_query(f.folder, &pattern, 1.0));
                }
                if matches!(scope, Scope::Any | Scope::Name) {
                    should.extend(self.regex_query(f.name_lc, &format!("{}.*", regex_escape(&p)), 2.0));
                }
            }
            Text::Fuzzy(w, dist) => {
                let tokens = analyze(self.analyzer, w);
                let token = tokens.into_iter().next()?;
                let mut add = |field: Field, boost: f32| {
                    let q = FuzzyTermQuery::new(Term::from_field_text(field, &token), *dist, true);
                    should.push(boost_query(Box::new(q), boost));
                };
                if matches!(scope, Scope::Any | Scope::Content) {
                    add(f.content, 1.0);
                }
                if matches!(scope, Scope::Any | Scope::Name | Scope::Path) {
                    add(f.name, 3.0);
                }
                if matches!(scope, Scope::Any | Scope::Meta) {
                    add(f.meta, 2.0);
                }
            }
        }

        match should.len() {
            0 => None,
            1 => should.pop(),
            _ => Some(Box::new(BooleanQuery::new(should.into_iter().map(|q| (Occur::Should, q)).collect()))),
        }
    }

    pub fn compile(&mut self, parsed: &Parsed, filters: &Filters) -> Result<BoxQuery> {
        let f = *self.fields;
        let mut clauses: Vec<(Occur, BoxQuery)> = Vec::new();

        for group in &parsed.groups {
            let alternatives: Vec<BoxQuery> = group.iter().filter_map(|c| self.clause_query(c)).collect();
            match alternatives.len() {
                0 => {}
                1 => clauses.push((Occur::Must, alternatives.into_iter().next().unwrap())),
                _ => clauses.push((
                    Occur::Must,
                    Box::new(BooleanQuery::new(alternatives.into_iter().map(|q| (Occur::Should, q)).collect())),
                )),
            }
        }
        for neg in &parsed.negatives {
            if let Some(q) = self.clause_query(neg) {
                clauses.push((Occur::MustNot, q));
            }
        }
        if !clauses.iter().any(|(o, _)| *o == Occur::Must) {
            clauses.push((Occur::Must, Box::new(AllQuery)));
        }

        let mut exts: Vec<String> = parsed.exts.clone();
        exts.extend(filters.exts.iter().map(|e| e.trim_start_matches('.').to_lowercase()));
        if !exts.is_empty() {
            let alts = exts
                .iter()
                .map(|e| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(Term::from_field_text(f.ext, e), IndexRecordOption::Basic)) as BoxQuery,
                    )
                })
                .collect();
            clauses.push((Occur::Must, Box::new(BooleanQuery::new(alts))));
        }

        if !filters.dirs.is_empty() {
            let alts: Vec<(Occur, BoxQuery)> = filters
                .dirs
                .iter()
                .filter_map(|d| {
                    let mut key = path_key(d.trim_end_matches(['/', '\\']));
                    key.push('/');
                    RegexQuery::from_pattern(&format!("{}.*", regex_escape(&key)), f.key)
                        .ok()
                        .map(|q| (Occur::Should, Box::new(q) as BoxQuery))
                })
                .collect();
            if !alts.is_empty() {
                clauses.push((Occur::Must, Box::new(BooleanQuery::new(alts))));
            }
        }

        for (field, lo, hi) in [
            (f.size, filters.min_size, filters.max_size),
            (f.mtime, filters.modified_after, filters.modified_before),
        ] {
            if lo.is_some() || hi.is_some() {
                let bound = |v: Option<u64>| v.map_or(Bound::Unbounded, |v| Bound::Included(Term::from_field_u64(field, v)));
                clauses.push((Occur::Must, Box::new(RangeQuery::new(bound(lo), bound(hi)))));
            }
        }

        Ok(Box::new(BooleanQuery::new(clauses)))
    }
}

fn boost_query(q: BoxQuery, boost: f32) -> BoxQuery {
    if (boost - 1.0).abs() < f32::EPSILON { q } else { Box::new(BoostQuery::new(q, boost)) }
}

/// Слова запроса, которые нужно подсвечивать в содержимом (после нормализации).
pub fn highlight_words(
    parsed: &Parsed,
    analyzer: &mut TextAnalyzer,
    raw_analyzer: &mut TextAnalyzer,
    mode: Mode,
) -> (Vec<String>, Vec<String>) {
    let mut words = Vec::new();
    let mut prefixes = Vec::new();
    for clause in parsed.groups.iter().flatten() {
        let scope = match (clause.scope, mode) {
            (Scope::Any, Mode::Name) => Scope::Name,
            (Scope::Any, Mode::Content) => Scope::Content,
            (s, _) => s,
        };
        if !matches!(scope, Scope::Any | Scope::Content) {
            continue;
        }
        match &clause.text {
            Text::Word(w) | Text::Phrase(w) | Text::Fuzzy(w, _) => {
                words.extend(analyze(analyzer, w));
                if matches!(clause.text, Text::Word(_)) {
                    words.extend(analyze(raw_analyzer, w));
                }
            }
            Text::Prefix(p) => prefixes.push(p.to_lowercase().replace('ё', "е")),
        }
    }
    words.sort();
    words.dedup();
    (words, prefixes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(s: &str) -> Clause {
        Clause { negate: false, scope: Scope::Any, text: Text::Word(s.into()) }
    }

    #[test]
    fn parses_basic_syntax() {
        let p = parse(r#"договор "срок действия" -черновик догов* аренд~ name:отчёт ext:docx,.XLSX"#);
        assert_eq!(p.groups.len(), 5);
        assert_eq!(p.groups[0], [word("договор")]);
        assert_eq!(p.groups[1][0].text, Text::Phrase("срок действия".into()));
        assert_eq!(p.groups[2][0].text, Text::Prefix("догов".into()));
        assert_eq!(p.groups[3][0].text, Text::Fuzzy("аренд".into(), 1));
        assert_eq!(p.groups[4][0].scope, Scope::Name);
        assert_eq!(p.negatives.len(), 1);
        assert_eq!(p.negatives[0].text, Text::Word("черновик".into()));
        assert_eq!(p.exts, ["docx", "xlsx"]);
    }

    #[test]
    fn or_joins_groups() {
        let p = parse("аренда OR лизинг счёт");
        assert_eq!(p.groups.len(), 2);
        assert_eq!(p.groups[0].len(), 2);
        assert_eq!(p.groups[1], [word("счёт")]);
    }

    #[test]
    fn guillemets_act_as_quotes() {
        let p = parse("«срок действия»");
        assert_eq!(p.groups[0][0].text, Text::Phrase("срок действия".into()));
    }

    #[test]
    fn only_negatives_is_not_positive() {
        let p = parse("-черновик");
        assert!(!p.has_positive());
    }

    #[test]
    fn escapes_regex() {
        assert_eq!(regex_escape("a.b(c)"), "a\\.b\\(c\\)");
    }
}
