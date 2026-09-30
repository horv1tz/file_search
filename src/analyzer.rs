//! Анализатор текста для индекса и запросов: токены → нижний регистр → ё→е →
//! стемминг (русский для кириллицы, английский для латиницы).
//!
//! Благодаря стеммингу запрос «договор» находит «договора», «договорами», а
//! «report» — «reports» и «reporting».

use rust_stemmers::{Algorithm, Stemmer};
use tantivy::tokenizer::{
    LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer, Token, TokenFilter, TokenStream, Tokenizer,
};

pub const TOKENIZER_NAME: &str = "ru_en";
/// То же без стемминга: нужен при разборе запросов, чтобы найти слово и в исходной форме.
pub const RAW_TOKENIZER_NAME: &str = "ru_en_raw";

/// Меняется при любом изменении правил токенизации: старые индексы перестают подходить.
pub const ANALYZER_VERSION: u32 = 1;

pub fn build() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(60))
        .filter(LowerCaser)
        .filter(FoldAndStem { stem: true })
        .build()
}

pub fn build_raw() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(60))
        .filter(LowerCaser)
        .filter(FoldAndStem { stem: false })
        .build()
}

#[derive(Clone)]
pub struct FoldAndStem {
    stem: bool,
}

impl TokenFilter for FoldAndStem {
    type Tokenizer<T: Tokenizer> = FoldAndStemFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        FoldAndStemFilter { inner: tokenizer, stem: self.stem }
    }
}

#[derive(Clone)]
pub struct FoldAndStemFilter<T> {
    inner: T,
    stem: bool,
}

impl<T: Tokenizer> Tokenizer for FoldAndStemFilter<T> {
    type TokenStream<'a> = FoldAndStemStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        FoldAndStemStream {
            tail: self.inner.token_stream(text),
            russian: Stemmer::create(Algorithm::Russian),
            english: Stemmer::create(Algorithm::English),
            stem: self.stem,
        }
    }
}

pub struct FoldAndStemStream<T> {
    stem: bool,
    tail: T,
    russian: Stemmer,
    english: Stemmer,
}

impl<T: TokenStream> TokenStream for FoldAndStemStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        normalize(&mut self.tail.token_mut().text, self.stem.then_some((&self.russian, &self.english)));
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

fn normalize(text: &mut String, stemmers: Option<(&Stemmer, &Stemmer)>) {
    let (russian, english) = match stemmers {
        Some(s) => s,
        None => {
            if text.contains('ё') {
                *text = text.replace('ё', "е");
            }
            return;
        }
    };
    let (mut cyrillic, mut latin, mut other) = (false, false, false);
    for c in text.chars() {
        if is_cyrillic(c) {
            cyrillic = true;
        } else if c.is_ascii_alphabetic() {
            latin = true;
        } else {
            // цифры и прочие алфавиты — такие токены не стеммим
            other = true;
        }
    }
    if cyrillic && text.contains('ё') {
        *text = text.replace('ё', "е");
    }
    if cyrillic && !latin && !other {
        let stemmed = russian.stem(text);
        if stemmed.len() != text.len() {
            *text = stemmed.into_owned();
        }
    } else if latin && !cyrillic && !other && text.len() >= 3 {
        let stemmed = english.stem(text);
        if stemmed.len() != text.len() {
            *text = stemmed.into_owned();
        }
    }
}

/// Разбирает строку в список нормализованных токенов (для построения запросов).
pub fn analyze(analyzer: &mut TextAnalyzer, text: &str) -> Vec<String> {
    let mut stream = analyzer.token_stream(text);
    let mut out = Vec::new();
    while let Some(t) = stream.next() {
        out.push(t.text.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morphology_and_folding() {
        let mut a = build();
        assert_eq!(analyze(&mut a, "Договоры, ДОГОВОРАМИ"), analyze(&mut a, "договор договор"));
        assert_eq!(analyze(&mut a, "всё"), analyze(&mut a, "все"));
        assert_eq!(analyze(&mut a, "reports reporting"), analyze(&mut a, "report report"));
        assert_eq!(analyze(&mut a, "INV-774411 v2"), ["inv", "774411", "v2"]);
        assert_eq!(analyze(&mut a, "колонтитул-верх"), analyze(&mut a, "колонтитул верх"));
    }
}
