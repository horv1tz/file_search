//! Минимальный разбор RTF: текст, абзацы, Unicode-символы и 8-битные символы
//! в кодовой странице документа (`\ansicpg1251`).

use encoding_rs::{Encoding, WINDOWS_1252};

use super::doc::Doc;

#[derive(Clone)]
struct Group {
    skip: bool,
    /// Сколько символов после `\uN` — запасной вариант (`\ucN`).
    uc: usize,
}

/// Группы, чьё содержимое не является текстом документа.
const SKIP_DESTINATIONS: [&str; 16] = [
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "object",
    "themedata",
    "colorschememapping",
    "latentstyles",
    "datastore",
    "listtable",
    "listoverridetable",
    "rsidtbl",
    "generator",
    "xmlnstbl",
    "fldinst",
];

pub fn looks_like_rtf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"{\\rtf")
}

fn codepage_encoding(cp: u32) -> &'static Encoding {
    let label = match cp {
        65001 => "utf-8".to_string(),
        10000 => "macintosh".to_string(),
        866 => "ibm866".to_string(),
        cp => format!("windows-{cp}"),
    };
    Encoding::for_label(label.as_bytes()).unwrap_or(WINDOWS_1252)
}

pub fn extract(data: &[u8], out: &mut Doc) {
    let mut stack: Vec<Group> = Vec::new();
    let mut cur = Group { skip: false, uc: 1 };
    let mut enc: &'static Encoding = WINDOWS_1252;
    let mut pending: Vec<u8> = Vec::new();
    let mut skip_chars = 0usize;
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !pending.is_empty() {
                if !cur.skip {
                    out.push(&enc.decode_without_bom_handling(&pending).0);
                }
                pending.clear();
            }
        };
    }

    while i < data.len() && !out.is_full() {
        let c = data[i];
        match c {
            b'{' => {
                flush!();
                stack.push(cur.clone());
                i += 1;
            }
            b'}' => {
                flush!();
                if let Some(g) = stack.pop() {
                    cur = g;
                }
                i += 1;
            }
            b'\r' | b'\n' => i += 1,
            b'\\' => {
                i += 1;
                let Some(&n) = data.get(i) else { break };
                match n {
                    b'\\' | b'{' | b'}' => {
                        if skip_chars > 0 {
                            skip_chars -= 1;
                        } else if !cur.skip {
                            pending.push(n);
                        }
                        i += 1;
                    }
                    b'\'' => {
                        let hex = data.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
                        if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                            if skip_chars > 0 {
                                skip_chars -= 1;
                            } else if !cur.skip {
                                pending.push(b);
                            }
                        }
                        i += 3;
                    }
                    b'*' => {
                        cur.skip = true;
                        i += 1;
                    }
                    b'~' => {
                        flush!();
                        if !cur.skip {
                            out.push(" ");
                        }
                        i += 1;
                    }
                    b'_' => {
                        flush!();
                        if !cur.skip {
                            out.push("-");
                        }
                        i += 1;
                    }
                    b if b.is_ascii_alphabetic() => {
                        flush!();
                        let start = i;
                        while i < data.len() && data[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        let word = std::str::from_utf8(&data[start..i]).unwrap_or("");
                        let num_start = i;
                        if data.get(i) == Some(&b'-') {
                            i += 1;
                        }
                        while i < data.len() && data[i].is_ascii_digit() {
                            i += 1;
                        }
                        let num: Option<i64> =
                            std::str::from_utf8(&data[num_start..i]).ok().and_then(|s| s.parse().ok());
                        if data.get(i) == Some(&b' ') {
                            i += 1;
                        }
                        if SKIP_DESTINATIONS.contains(&word) {
                            cur.skip = true;
                        }
                        match word {
                            "ansicpg" => {
                                if let Some(cp) = num {
                                    enc = codepage_encoding(cp as u32);
                                }
                            }
                            "uc" => cur.uc = num.unwrap_or(1).max(0) as usize,
                            "u" if !cur.skip => {
                                if let Some(n) = num {
                                    let code = if n < 0 { (n + 65536) as u32 } else { n as u32 };
                                    if let Some(ch) = char::from_u32(code) {
                                        out.push_char(ch);
                                    }
                                }
                                skip_chars = cur.uc;
                            }
                            "par" | "line" | "sect" | "page" | "row" if !cur.skip => out.newline(),
                            "tab" | "cell" if !cur.skip => out.tab(),
                            "emdash" if !cur.skip => out.push("—"),
                            "endash" if !cur.skip => out.push("–"),
                            "bullet" if !cur.skip => out.push("•"),
                            "lquote" | "rquote" if !cur.skip => out.push("'"),
                            "ldblquote" | "rdblquote" if !cur.skip => out.push("\""),
                            "bin" => i += num.unwrap_or(0).max(0) as usize,
                            _ => {}
                        }
                    }
                    _ => i += 1,
                }
            }
            _ => {
                if skip_chars > 0 {
                    skip_chars -= 1;
                } else if !cur.skip {
                    pending.push(c);
                }
                i += 1;
            }
        }
    }
    flush!();
    out.newline();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(rtf: &[u8]) -> String {
        let mut d = Doc::new(10_000);
        extract(rtf, &mut d);
        d.finish().text
    }

    #[test]
    fn plain_and_unicode() {
        let rtf = r"{@rtf1@ansi@deff0{@fonttbl{@f0 Arial;}}@pard Hello @u1055?@u1088?@u1080?@u1074?@u1077?@u1090? world@par Next line}".replace('@', "\\");
        let t = run(rtf.as_bytes());
        assert_eq!(t, "Hello Привет world\nNext line");
    }

    #[test]
    fn cp1251_bytes() {
        let t =
            run(b"{\\rtf1\\ansi\\ansicpg1251 {\\colortbl;\\red0\\green0\\blue0;}\\'e4\\'ee\\'e3\\'ee\\'e2\\'ee\\'f0}");
        assert_eq!(t, "договор");
    }

    #[test]
    fn ignorable_destinations_are_skipped() {
        let t = run(br"{\rtf1{\*\generator Word;}{\info{\title Secret}}Visible}");
        assert_eq!(t, "Visible");
    }
}
