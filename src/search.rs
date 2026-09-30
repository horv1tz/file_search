//! Поиск по индексу: запрос, фильтры, сортировка, сниппеты с подсветкой.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{IndexRecordOption, OwnedValue, Term, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{DocAddress, Index, IndexReader, Order, ReloadPolicy, Searcher as TantivySearcher, TantivyDocument};

use crate::extract::UNIT_SEP;
use crate::query::{self, Compiler, Filters, Mode};
use crate::schema::{Fields, open_index, path_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Relevance,
    Newest,
    Oldest,
    Largest,
    Smallest,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub mode: Mode,
    pub exts: Vec<String>,
    pub dirs: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<u64>,
    pub modified_before: Option<u64>,
    pub limit: usize,
    pub offset: usize,
    pub sort: Sort,
    pub snippets: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            query: String::new(),
            mode: Mode::All,
            exts: Vec::new(),
            dirs: Vec::new(),
            min_size: None,
            max_size: None,
            modified_after: None,
            modified_before: None,
            limit: 20,
            offset: 0,
            sort: Sort::Relevance,
            snippets: true,
        }
    }
}

/// Кусок текста сниппета; `hit` — слово из запроса.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Part {
    pub text: String,
    pub hit: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Snippet {
    pub parts: Vec<Part>,
    pub more_before: bool,
    pub more_after: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    /// Время изменения, миллисекунды с 1970 года.
    pub modified_ms: u64,
    pub score: f32,
    /// Лист, слайд или страница, где найдено совпадение.
    pub location: Option<String>,
    pub snippet: Option<Snippet>,
    pub status: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub total: usize,
    pub hits: Vec<Hit>,
    #[serde(serialize_with = "serialize_duration_ms")]
    pub took: Duration,
}

fn serialize_duration_ms<S: serde::Serializer>(d: &Duration, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64() * 1000.0)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct IndexStats {
    pub documents: u64,
    pub segments: usize,
    pub size_on_disk: u64,
    pub by_ext: Vec<(String, u64)>,
    pub by_status: Vec<(String, u64)>,
}

pub struct Searcher {
    index: Index,
    fields: Fields,
    reader: IndexReader,
}

fn str_of(doc: &TantivyDocument, field: tantivy::schema::Field) -> &str {
    doc.get_first(field).and_then(|v| v.as_str()).unwrap_or("")
}

fn u64_of(doc: &TantivyDocument, field: tantivy::schema::Field) -> u64 {
    match doc.get_first(field).map(OwnedValue::from) {
        Some(OwnedValue::U64(v)) => v,
        _ => 0,
    }
}

impl Searcher {
    /// `live` — следить за коммитами индекса (для долгоживущего сервера).
    pub fn open(dir: &Path, live: bool) -> Result<Searcher> {
        let (index, fields) = open_index(dir)?;
        let policy = if live { ReloadPolicy::OnCommitWithDelay } else { ReloadPolicy::Manual };
        let reader = index.reader_builder().reload_policy(policy).try_into()?;
        Ok(Searcher { index, fields, reader })
    }

    fn analyzers(&self) -> (TextAnalyzer, TextAnalyzer) {
        let tokenizers = self.index.tokenizers();
        (
            tokenizers.get(crate::analyzer::TOKENIZER_NAME).expect("токенайзер зарегистрирован при открытии индекса"),
            tokenizers
                .get(crate::analyzer::RAW_TOKENIZER_NAME)
                .expect("токенайзер зарегистрирован при открытии индекса"),
        )
    }

    pub fn search(&self, opts: &SearchOptions) -> Result<SearchResult> {
        let started = Instant::now();
        let searcher = self.reader.searcher();
        let parsed = query::parse(&opts.query);
        let filters = Filters {
            exts: opts.exts.clone(),
            dirs: opts.dirs.clone(),
            min_size: opts.min_size,
            max_size: opts.max_size,
            modified_after: opts.modified_after,
            modified_before: opts.modified_before,
        };
        let (mut analyzer, mut raw_analyzer) = self.analyzers();
        let q = Compiler {
            fields: &self.fields,
            analyzer: &mut analyzer,
            raw_analyzer: &mut raw_analyzer,
            mode: opts.mode,
        }
        .compile(&parsed, &filters)?;

        let limit = opts.limit.clamp(1, 1000);
        let sort = if opts.sort == Sort::Relevance && !parsed.has_positive() { Sort::Newest } else { opts.sort };
        let collector = TopDocs::with_limit(limit).and_offset(opts.offset);
        let (addresses, total): (Vec<(f32, DocAddress)>, usize) = match sort {
            Sort::Relevance => searcher.search(&q, &(collector.order_by_score(), Count))?,
            Sort::Newest | Sort::Oldest | Sort::Largest | Sort::Smallest => {
                let (field, order) = match sort {
                    Sort::Newest => ("mtime", Order::Desc),
                    Sort::Oldest => ("mtime", Order::Asc),
                    Sort::Largest => ("size", Order::Desc),
                    _ => ("size", Order::Asc),
                };
                let (docs, total) =
                    searcher.search(&q, &(collector.order_by_fast_field::<u64>(field, order), Count))?;
                (docs.into_iter().map(|(_, a)| (0.0, a)).collect(), total)
            }
        };

        let highlighter = if opts.snippets {
            self.highlighter(&searcher, &parsed, &mut analyzer, &mut raw_analyzer, opts.mode)
        } else {
            None
        };

        let mut hits = Vec::with_capacity(addresses.len());
        for (score, addr) in addresses {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let f = &self.fields;
            let path = str_of(&doc, f.path).to_string();
            let name =
                Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
            let (snippet, location) = match &highlighter {
                Some(g) => snippet_for(g, str_of(&doc, f.content), str_of(&doc, f.units)),
                None => (None, None),
            };
            hits.push(Hit {
                path,
                name,
                ext: str_of(&doc, f.ext).to_string(),
                size: u64_of(&doc, f.size),
                modified_ms: u64_of(&doc, f.mtime),
                score,
                location,
                snippet,
                status: str_of(&doc, f.status).to_string(),
            });
        }
        Ok(SearchResult { total, hits, took: started.elapsed() })
    }

    /// Генератор сниппетов по словам запроса (с раскрытием префиксов по словарю индекса).
    fn highlighter(
        &self,
        searcher: &TantivySearcher,
        parsed: &query::Parsed,
        analyzer: &mut TextAnalyzer,
        raw_analyzer: &mut TextAnalyzer,
        mode: Mode,
    ) -> Option<SnippetGenerator> {
        let (mut words, prefixes) = query::highlight_words(parsed, analyzer, raw_analyzer, mode);
        let mut set: BTreeSet<String> = words.drain(..).collect();
        for prefix in &prefixes {
            for seg in searcher.segment_readers() {
                let Ok(inv) = seg.inverted_index(self.fields.content) else { continue };
                let Ok(mut stream) = inv.terms().range().ge(prefix.as_bytes()).into_stream() else { continue };
                let mut found = 0;
                while stream.advance() && found < 64 {
                    if !stream.key().starts_with(prefix.as_bytes()) {
                        break;
                    }
                    set.insert(String::from_utf8_lossy(stream.key()).into_owned());
                    found += 1;
                }
            }
        }
        if set.is_empty() {
            return None;
        }
        let clauses: Vec<(Occur, Box<dyn Query>)> = set
            .into_iter()
            .map(|w| {
                let q = TermQuery::new(Term::from_field_text(self.fields.content, &w), IndexRecordOption::Basic);
                (Occur::Should, Box::new(q) as Box<dyn Query>)
            })
            .collect();
        let mut generator =
            SnippetGenerator::create(searcher, &BooleanQuery::new(clauses), self.fields.content).ok()?;
        generator.set_max_num_chars(240);
        Some(generator)
    }

    pub fn stats(&self, dir: Option<&Path>) -> Result<IndexStats> {
        let searcher = self.reader.searcher();
        let mut stats = IndexStats {
            documents: searcher.num_docs(),
            segments: searcher.segment_readers().len(),
            size_on_disk: dir.map(dir_size).unwrap_or(0),
            ..Default::default()
        };
        let mut by_ext: HashMap<String, u64> = HashMap::new();
        let mut by_status: HashMap<String, u64> = HashMap::new();
        for seg in searcher.segment_readers() {
            let ff = seg.fast_fields();
            for (name, map) in [("ext", &mut by_ext), ("status", &mut by_status)] {
                let Some(col) = ff.str(name)? else { continue };
                let mut ords: HashMap<u64, u64> = HashMap::new();
                let mut missing = 0u64;
                for doc in 0..seg.max_doc() {
                    if seg.is_deleted(doc) {
                        continue;
                    }
                    match col.term_ords(doc).next() {
                        Some(o) => *ords.entry(o).or_default() += 1,
                        None => missing += 1,
                    }
                }
                let mut buf = String::new();
                for (ord, n) in ords {
                    buf.clear();
                    col.ord_to_str(ord, &mut buf)?;
                    *map.entry(buf.clone()).or_default() += n;
                }
                if missing > 0 && name == "ext" {
                    *map.entry(String::new()).or_default() += missing;
                }
            }
        }
        stats.by_ext = sorted_counts(by_ext);
        stats.by_status = sorted_counts(by_status);
        Ok(stats)
    }

    /// Есть ли путь в индексе (для безопасного открытия файлов из интерфейса).
    pub fn contains_path(&self, path: &str) -> bool {
        let searcher = self.reader.searcher();
        let term = Term::from_field_text(self.fields.key, &path_key(path));
        searcher.doc_freq(&term).map(|n| n > 0).unwrap_or(false)
    }
}

fn sorted_counts(map: HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// Сжимает пробелы и переводы строк внутри кусков сниппета.
fn collapse(parts: Vec<(String, bool)>) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::new();
    let mut last_space = true;
    for (text, hit) in parts {
        let mut s = String::with_capacity(text.len());
        for c in text.chars() {
            if c.is_whitespace() || c == UNIT_SEP {
                if !last_space {
                    s.push(' ');
                }
                last_space = true;
            } else {
                s.push(c);
                last_space = false;
            }
        }
        if !s.is_empty() {
            out.push(Part { text: s, hit });
        }
    }
    if let Some(last) = out.last_mut() {
        let trimmed = last.text.trim_end().len();
        last.text.truncate(trimmed);
    }
    out.retain(|p| !p.text.is_empty());
    out
}

fn snippet_for(generator: &SnippetGenerator, content: &str, units: &str) -> (Option<Snippet>, Option<String>) {
    if content.is_empty() {
        return (None, None);
    }
    let snippet = generator.snippet(content);
    let ranges: &[Range<usize>] = snippet.highlighted();
    let Some(first) = ranges.first() else { return (None, None) };
    let fragment = snippet.fragment();

    // Фрагмент может захватить соседний лист/слайд — оставляем только тот, где первое совпадение.
    let unit_start = fragment[..first.start].rfind(UNIT_SEP).map_or(0, |i| i + UNIT_SEP.len_utf8());
    let unit_end = fragment[first.start..].find(UNIT_SEP).map_or(fragment.len(), |i| first.start + i);

    let mut parts = Vec::new();
    let mut pos = unit_start;
    for r in ranges.iter().filter(|r| r.start >= unit_start && r.end <= unit_end) {
        if r.start > pos {
            parts.push((fragment[pos..r.start].to_string(), false));
        }
        parts.push((fragment[r.clone()].to_string(), true));
        pos = r.end;
    }
    if pos < unit_end {
        parts.push((fragment[pos..unit_end].to_string(), false));
    }

    // Положение фрагмента в тексте → подпись юнита и признаки «есть текст до/после».
    let (mut location, mut more_before, mut more_after) = (None, true, true);
    if let Some(offset) = content.find(fragment) {
        let (abs_start, abs_end) = (offset + unit_start, offset + unit_end);
        let blank = |c: char| c.is_whitespace() && c != UNIT_SEP;
        let before = content[..abs_start].trim_end_matches(blank);
        let after = content[abs_end..].trim_start_matches(blank);
        more_before = !before.is_empty() && !before.ends_with(UNIT_SEP);
        more_after = !after.is_empty() && !after.starts_with(UNIT_SEP);
        if !units.is_empty() {
            let index = content[..offset + first.start].matches(UNIT_SEP).count();
            location = units.lines().nth(index).map(str::to_string);
        }
    }
    (Some(Snippet { parts: collapse(parts), more_before, more_after }), location)
}
