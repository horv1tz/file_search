//! Веб-интерфейс: маленький HTTP-сервер только на localhost.
//!
//! Защита: слушаем 127.0.0.1, проверяем заголовок Host (против DNS rebinding), действия с файлами
//! требуют секретный токен из страницы (против запросов с чужих сайтов) и работают только
//! для путей, которые есть в индексе.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use anyhow::{Result, anyhow};
use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::platform::{self, parse_date_ms, parse_size};
use crate::query::Mode;
use crate::search::{SearchOptions, Searcher, Sort};

const INDEX_HTML: &str = include_str!("index.html");

/// Такие файлы из браузерного интерфейса можно только показать в папке, но не запустить.
const NEVER_LAUNCH: &[&str] = &[
    "exe",
    "bat",
    "cmd",
    "com",
    "msi",
    "msp",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "scr",
    "lnk",
    "reg",
    "jar",
    "sh",
    "app",
    "dll",
    "cpl",
    "hta",
    "pif",
    "appx",
    "msix",
    "url",
    "inf",
    "gadget",
    "appref-ms",
];

struct State {
    searcher: Searcher,
    token: String,
    port: u16,
}

fn random_token() -> String {
    (0..3).map(|_| format!("{:016x}", RandomState::new().build_hasher().finish())).collect()
}

pub fn serve(dir: &Path, port: u16, open_browser: bool) -> Result<()> {
    let searcher = Searcher::open(dir, true)?;
    let addr = format!("127.0.0.1:{port}");
    let server = Arc::new(Server::http(&addr).map_err(|e| anyhow!("не удалось запустить сервер на {addr}: {e}"))?);
    let state = Arc::new(State { searcher, token: random_token(), port });

    let url = format!("http://127.0.0.1:{port}/");
    println!("Веб-интерфейс: {url}");
    println!("Индекс: {} (обновления видны без перезапуска). Остановить: Ctrl+C.", dir.display());
    if open_browser {
        let _ = platform::open_url(&url);
    }

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (server, state) = (server.clone(), state.clone());
            thread::spawn(move || {
                for request in server.incoming_requests() {
                    handle(&state, request);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("корректный заголовок")
}

fn respond(req: Request, status: u16, content_type: &str, body: Vec<u8>) {
    let response = Response::from_data(body)
        .with_status_code(StatusCode(status))
        .with_header(header("Content-Type", content_type))
        .with_header(header("Cache-Control", "no-store"))
        .with_header(header("X-Content-Type-Options", "nosniff"))
        .with_header(header("Referrer-Policy", "no-referrer"))
        .with_header(header(
            "Content-Security-Policy",
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'",
        ));
    let _ = req.respond(response);
}

fn respond_json<T: Serialize>(req: Request, status: u16, value: &T) {
    match serde_json::to_vec(value) {
        Ok(body) => respond(req, status, "application/json; charset=utf-8", body),
        Err(e) => respond(req, 500, "text/plain; charset=utf-8", e.to_string().into_bytes()),
    }
}

fn error_json(req: Request, status: u16, message: &str) {
    respond_json(req, status, &serde_json::json!({ "error": message }));
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(p), String::new()),
        })
        .collect()
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
}

fn header_value(req: &Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

fn host_allowed(req: &Request, port: u16) -> bool {
    match header_value(req, "Host") {
        Some(h) => {
            let h = h.to_lowercase();
            h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
        }
        None => false,
    }
}

fn search_options(params: &[(String, String)]) -> Result<SearchOptions, String> {
    let mut o = SearchOptions { snippets: true, ..SearchOptions::default() };
    o.query = param(params, "q").unwrap_or("").to_string();
    o.mode = match param(params, "mode") {
        Some("name") => Mode::Name,
        Some("content") => Mode::Content,
        _ => Mode::All,
    };
    o.sort = match param(params, "sort") {
        Some("newest") => Sort::Newest,
        Some("oldest") => Sort::Oldest,
        Some("largest") => Sort::Largest,
        Some("smallest") => Sort::Smallest,
        _ => Sort::Relevance,
    };
    if let Some(e) = param(params, "ext") {
        o.exts = e.split(',').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect();
    }
    if let Some(d) = param(params, "dir") {
        o.dirs = vec![d.to_string()];
    }
    o.limit = param(params, "limit").and_then(|v| v.parse().ok()).unwrap_or(20usize).clamp(1, 100);
    o.offset = param(params, "offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    if let Some(v) = param(params, "min") {
        o.min_size = Some(parse_size(v).ok_or_else(|| format!("не понимаю размер: {v}"))?);
    }
    if let Some(v) = param(params, "max") {
        o.max_size = Some(parse_size(v).ok_or_else(|| format!("не понимаю размер: {v}"))?);
    }
    if let Some(v) = param(params, "after") {
        o.modified_after = Some(parse_date_ms(v, false).ok_or_else(|| format!("не понимаю дату: {v}"))?);
    }
    if let Some(v) = param(params, "before") {
        o.modified_before = Some(parse_date_ms(v, true).ok_or_else(|| format!("не понимаю дату: {v}"))?);
    }
    Ok(o)
}

fn handle(state: &State, mut req: Request) {
    if !host_allowed(&req, state.port) {
        return respond(req, 403, "text/plain; charset=utf-8", "Недопустимый Host".as_bytes().to_vec());
    }
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let params = parse_params(query);
    let method = req.method().clone();

    match (&method, path) {
        (Method::Get, "/") => {
            let page = INDEX_HTML.replace("__TOKEN__", &state.token);
            respond(req, 200, "text/html; charset=utf-8", page.into_bytes());
        }
        (Method::Get, "/favicon.ico") => respond(req, 204, "image/x-icon", Vec::new()),
        (Method::Get, "/api/search") => match search_options(&params) {
            Err(msg) => error_json(req, 400, &msg),
            Ok(opts) => match state.searcher.search(&opts) {
                Ok(result) => respond_json(req, 200, &result),
                Err(e) => error_json(req, 500, &format!("{e:#}")),
            },
        },
        (Method::Get, "/api/stats") => match state.searcher.stats(None) {
            Ok(stats) => respond_json(req, 200, &stats),
            Err(e) => error_json(req, 500, &format!("{e:#}")),
        },
        (Method::Post, "/api/open") => {
            let mut body = String::new();
            let _ = req.as_reader().take(1024).read_to_string(&mut body);
            if header_value(&req, "X-Token").as_deref() != Some(state.token.as_str()) {
                return error_json(req, 403, "неверный токен");
            }
            let Some(file) = param(&params, "path") else { return error_json(req, 400, "не указан путь") };
            if !state.searcher.contains_path(file) {
                return error_json(req, 404, "файла нет в индексе");
            }
            let file = PathBuf::from(file);
            let action = param(&params, "action").unwrap_or("open");
            let ext = file.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
            let result = match action {
                "reveal" => platform::reveal_path(&file),
                "open" if NEVER_LAUNCH.contains(&ext.as_str()) => {
                    return error_json(req, 403, "исполняемые файлы отсюда не запускаются — используйте «В папке»");
                }
                "open" => platform::open_path(&file),
                _ => return error_json(req, 400, "неизвестное действие"),
            };
            match result {
                Ok(()) => respond_json(req, 200, &serde_json::json!({ "ok": true })),
                Err(e) => error_json(req, 500, &format!("не удалось выполнить: {e}")),
            }
        }
        _ => respond(req, 404, "text/plain; charset=utf-8", "Не найдено".as_bytes().to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_and_plus() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("%D0%B4%D0%BE%D0%B3%D0%BE%D0%B2%D0%BE%D1%80"), "договор");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn parses_params() {
        let p = parse_params("q=%D0%B0+b&ext=docx,pdf&empty=");
        assert_eq!(param(&p, "q"), Some("а b"));
        assert_eq!(param(&p, "ext"), Some("docx,pdf"));
        assert_eq!(param(&p, "empty"), None);
    }

    #[test]
    fn token_is_random() {
        assert_ne!(random_token(), random_token());
        assert_eq!(random_token().len(), 48);
    }
}
