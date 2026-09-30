//! Накопитель извлечённого текста.
//!
//! Текст файла может состоять из нескольких «юнитов» (лист Excel, слайд,
//! страница). Юниты разделяются символом [`UNIT_SEP`], а их подписи хранятся
//! отдельно — так по позиции фрагмента в тексте можно сказать, на каком листе
//! или слайде найдено совпадение.

/// Разделитель юнитов внутри текста (form feed: токенайзер считает его пробелом).
pub const UNIT_SEP: char = '\u{000C}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// Формат распознан, но текста в файле нет.
    Empty,
    /// Файл зашифрован паролем.
    Encrypted,
    /// Содержимое намеренно не читалось (бинарный, слишком большой, неизвестный формат).
    Skipped(String),
    /// Ошибка разбора; индексируется только имя файла.
    Failed(String),
}

impl Status {
    pub fn as_string(&self) -> String {
        match self {
            Status::Ok => "ok".into(),
            Status::Empty => "empty".into(),
            Status::Encrypted => "encrypted".into(),
            Status::Skipped(r) => format!("skipped: {r}"),
            Status::Failed(e) => format!("error: {e}"),
        }
    }
}

/// Результат извлечения из одного файла.
#[derive(Debug, Clone)]
pub struct Extracted {
    pub text: String,
    /// Подписи юнитов; пусто, если у файла нет деления на листы/слайды.
    pub units: Vec<String>,
    /// Заголовок, автор, ключевые слова, имена листов — то, что стоит искать отдельно от тела.
    pub meta: String,
    pub status: Status,
    pub truncated: bool,
}

impl Extracted {
    pub fn without_content(status: Status) -> Self {
        Extracted {
            text: String::new(),
            units: Vec::new(),
            meta: String::new(),
            status,
            truncated: false,
        }
    }
}

const META_LIMIT: usize = 64 * 1024;

pub struct Doc {
    pub text: String,
    pub units: Vec<String>,
    pub meta: String,
    limit: usize,
    truncated: bool,
    unit_start: usize,
}

impl Doc {
    pub fn new(limit: usize) -> Self {
        Doc {
            text: String::new(),
            units: Vec::new(),
            meta: String::new(),
            limit,
            truncated: false,
            unit_start: 0,
        }
    }

    /// Достигнут лимит текста — читать дальше бессмысленно.
    pub fn is_full(&self) -> bool {
        self.truncated
    }

    /// Начинает новый юнит. Пустой предыдущий юнит не оставляем: его подпись заменяется.
    pub fn begin_unit(&mut self, label: String) {
        if !self.units.is_empty() {
            if self.text.len() == self.unit_start {
                *self.units.last_mut().unwrap() = label;
                return;
            }
            self.newline();
            if self.text.len() + 1 > self.limit {
                self.truncated = true;
                return;
            }
            self.text.push(UNIT_SEP);
        }
        self.units.push(label);
        self.unit_start = self.text.len();
    }

    pub fn push(&mut self, s: &str) {
        if self.truncated || s.is_empty() {
            return;
        }
        let room = self.limit.saturating_sub(self.text.len());
        if s.len() <= room {
            self.text.push_str(s);
        } else {
            let mut cut = room;
            while cut > 0 && !s.is_char_boundary(cut) {
                cut -= 1;
            }
            self.text.push_str(&s[..cut]);
            self.truncated = true;
        }
    }

    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.push(c.encode_utf8(&mut buf));
    }

    fn at_unit_start(&self) -> bool {
        self.text.len() == self.unit_start
    }

    fn last_byte(&self) -> Option<u8> {
        if self.at_unit_start() {
            None
        } else {
            self.text.as_bytes().last().copied()
        }
    }

    /// Перевод строки без пустых строк подряд; хвостовые пробелы/табы отбрасываются.
    pub fn newline(&mut self) {
        while matches!(self.last_byte(), Some(b' ' | b'\t')) {
            self.text.pop();
        }
        if !self.at_unit_start() && self.last_byte() != Some(b'\n') {
            self.push_char('\n');
        }
    }

    /// Разделитель ячеек; не ставится в начале строки и после другого разделителя.
    pub fn tab(&mut self) {
        while self.last_byte() == Some(b' ') {
            self.text.pop();
        }
        if !matches!(self.last_byte(), None | Some(b'\n' | b'\t')) {
            self.push_char('\t');
        }
    }

    pub fn space(&mut self) {
        if !matches!(self.last_byte(), None | Some(b'\n' | b'\t' | b' ')) {
            self.push_char(' ');
        }
    }

    pub fn add_meta(&mut self, s: &str) {
        let s = s.trim();
        if s.is_empty() || self.meta.len() + s.len() + 1 > META_LIMIT {
            return;
        }
        if !self.meta.is_empty() {
            self.meta.push('\n');
        }
        self.meta.push_str(s);
    }

    pub fn finish(mut self) -> Extracted {
        let trimmed = self.text.trim_end().len();
        self.text.truncate(trimmed);
        let has_text = self.text.chars().any(|c| !c.is_whitespace());
        let status = if has_text || !self.meta.is_empty() {
            Status::Ok
        } else {
            Status::Empty
        };
        Extracted {
            text: self.text,
            units: self.units,
            meta: self.meta,
            status,
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_are_separated_and_labelled() {
        let mut d = Doc::new(1000);
        d.begin_unit("A".into());
        d.push("one");
        d.begin_unit("B".into());
        d.push("two");
        let e = d.finish();
        assert_eq!(e.units, ["A", "B"]);
        assert_eq!(e.text, format!("one\n{UNIT_SEP}two"));
    }

    #[test]
    fn empty_unit_label_is_replaced() {
        let mut d = Doc::new(1000);
        d.begin_unit("A".into());
        d.begin_unit("B".into());
        d.push("x");
        let e = d.finish();
        assert_eq!(e.units, ["B"]);
        assert_eq!(e.text, "x");
    }

    #[test]
    fn limit_cuts_on_char_boundary() {
        let mut d = Doc::new(5);
        d.push("аб");
        d.push("вгд");
        assert!(d.is_full());
        assert_eq!(d.text, "аб");
    }

    #[test]
    fn separators_do_not_stack() {
        let mut d = Doc::new(100);
        d.tab();
        d.push("a");
        d.tab();
        d.tab();
        d.push("b");
        d.newline();
        d.newline();
        d.push("c");
        assert_eq!(d.text, "a\tb\nc");
    }
}
