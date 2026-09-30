//! Мелкие платформенные вещи: открыть файл, показать его в проводнике, форматирование.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use chrono::{Local, TimeZone};

fn spawn(mut cmd: Command) -> io::Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// Открывает файл программой по умолчанию.
pub fn open_path(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("explorer");
        cmd.arg(path);
        spawn(cmd)
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        cmd.arg(path);
        spawn(cmd)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(path);
        spawn(cmd)
    }
}

/// Открывает адрес в браузере по умолчанию.
pub fn open_url(url: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("explorer");
        cmd.arg(url);
        spawn(cmd)
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        cmd.arg(url);
        spawn(cmd)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        spawn(cmd)
    }
}

/// Показывает файл в проводнике (с выделением, где это поддерживается).
pub fn reveal_path(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("explorer");
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        spawn(cmd)
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        cmd.arg("-R").arg(path);
        spawn(cmd)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(path.parent().unwrap_or(path));
        spawn(cmd)
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    if bytes < 1024 {
        return format!("{bytes} Б");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit]).replace('.', ",")
}

pub fn format_time(ms: u64) -> String {
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(t) => t.format("%d.%m.%Y %H:%M").to_string(),
        None => String::new(),
    }
}

/// «10K», «5M», «1,5G», «2048» → байты.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().replace(',', ".").to_lowercase();
    let s = s.trim_end_matches('b');
    let (num, mult) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1u64 << 10),
        'm' => (&s[..s.len() - 1], 1 << 20),
        'g' => (&s[..s.len() - 1], 1 << 30),
        't' => (&s[..s.len() - 1], 1 << 40),
        _ => (s, 1),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v >= 0.0).then(|| (v * mult as f64) as u64)
}

/// «2024-03-15», «15.03.2024», «2024-03-15 14:30» → миллисекунды с 1970 (местное время).
pub fn parse_date_ms(s: &str, end_of_day: bool) -> Option<u64> {
    use chrono::{NaiveDate, NaiveDateTime};
    let s = s.trim();
    for fmt in ["%Y-%m-%d %H:%M", "%d.%m.%Y %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Local.from_local_datetime(&dt).earliest().map(|t| t.timestamp_millis().max(0) as u64);
        }
    }
    for fmt in ["%Y-%m-%d", "%d.%m.%Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            let dt = if end_of_day { d.and_hms_opt(23, 59, 59)? } else { d.and_hms_opt(0, 0, 0)? };
            return Local.from_local_datetime(&dt).earliest().map(|t| t.timestamp_millis().max(0) as u64);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("10K"), Some(10 * 1024));
        assert_eq!(parse_size("1,5M"), Some(1_572_864));
        assert_eq!(parse_size("2048"), Some(2048));
        assert_eq!(parse_size("abc"), None);
        assert_eq!(format_size(1536), "1,5 КБ");
        assert_eq!(format_size(12), "12 Б");
    }

    #[test]
    fn dates() {
        assert!(parse_date_ms("2024-03-15", false).unwrap() < parse_date_ms("2024-03-15", true).unwrap());
        assert_eq!(parse_date_ms("15.03.2024", false), parse_date_ms("2024-03-15", false));
        assert!(parse_date_ms("вчера", false).is_none());
    }
}
