//! Мелкие платформенные вещи: открыть файл, показать его в проводнике, форматирование.

use std::io;
use std::path::Path;
#[cfg(not(windows))]
use std::process::{Command, Stdio};

use chrono::{Local, TimeZone};

#[cfg(not(windows))]
fn spawn(mut cmd: Command) -> io::Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// Открывает файл, адрес или папку средствами оболочки Windows (как двойной щелчок в проводнике),
/// без запуска посторонних процессов через командную строку.
#[cfg(windows)]
fn shell_execute(file: &std::ffi::OsStr, params: Option<&str>) -> io::Result<()> {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }
    const SW_SHOWNORMAL: i32 = 1;
    // Коды ошибок ShellExecute: 2 и 3 — нет файла, 5 — доступ запрещён, 31 — нет программы для этого типа файлов.
    const SE_ERR_NOASSOC: isize = 31;

    let wide = |s: &OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let operation = wide(OsStr::new("open"));
    let file = wide(file);
    let params = params.map(|p| wide(OsStr::new(p)));
    // SAFETY: все строки оканчиваются нулём и живут до конца вызова; окно-владелец и каталог не заданы.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            params.as_ref().map_or(null(), |p| p.as_ptr()),
            null(),
            SW_SHOWNORMAL,
        )
    };
    match result {
        r if r > 32 => Ok(()),
        SE_ERR_NOASSOC => Err(io::Error::other("для этого типа файлов не назначена программа")),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

/// Снижает (или возвращает) приоритет всего процесса: индексация уступает процессор другим программам.
/// В Windows действует сразу на все потоки; в Linux и macOS — на потоки, созданные после вызова.
pub fn set_low_priority(low: bool) {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn SetPriorityClass(process: isize, class: u32) -> i32;
        }
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;
        const NORMAL_PRIORITY_CLASS: u32 = 0x20;
        let class = if low { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS };
        // SAFETY: псевдодескриптор текущего процесса всегда допустим.
        unsafe {
            SetPriorityClass(GetCurrentProcess(), class);
        }
    }
    #[cfg(unix)]
    {
        // Вернуть обычный приоритет без прав администратора нельзя — тогда просто ничего не делаем.
        // SAFETY: обычный системный вызов без указателей.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS as _, 0, if low { 10 } else { 0 });
        }
    }
    #[cfg(not(any(windows, unix)))]
    let _ = low;
}

/// Фоновый режим для текущего потока: низкий приоритет и процессора, и диска (в Windows). Так чтение тысяч файлов
/// не заставляет остальные программы ждать диск.
pub fn thread_background_mode() {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetCurrentThread() -> isize;
            fn SetThreadPriority(thread: isize, priority: i32) -> i32;
        }
        const THREAD_MODE_BACKGROUND_BEGIN: i32 = 0x0001_0000;
        // SAFETY: псевдодескриптор текущего потока всегда допустим.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
    }
}

/// Такие файлы из интерфейса можно только показать в папке, но не запустить.
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

/// Можно ли открывать файл программой по умолчанию (исполняемые и скриптовые — нельзя).
pub fn is_safe_to_open(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    !NEVER_LAUNCH.contains(&ext.as_str())
}

/// Открывает файл программой по умолчанию.
pub fn open_path(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        shell_execute(path.as_os_str(), None)
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
        shell_execute(std::ffi::OsStr::new(url), None)
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
        // Проводник с выделенным файлом: `explorer.exe /select,"путь"`.
        shell_execute(std::ffi::OsStr::new("explorer.exe"), Some(&format!("/select,\"{}\"", path.display())))
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
    (v >= 0.0).then_some((v * mult as f64) as u64)
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
    fn executables_are_not_launched() {
        assert!(!is_safe_to_open(Path::new(r"D:\Games\setup.EXE")));
        assert!(!is_safe_to_open(Path::new("script.ps1")));
        assert!(is_safe_to_open(Path::new("report.docx")));
        assert!(is_safe_to_open(Path::new("noext")));
    }

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

    #[cfg(target_os = "linux")]
    #[test]
    fn low_priority_raises_the_nice_value() {
        fn nice() -> i32 {
            let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
            // Название процесса может содержать пробелы, поэтому считаем поля после последней скобки: nice — 17-е.
            let rest = stat.rsplit_once(')').unwrap().1;
            rest.split_whitespace().nth(16).unwrap().parse().unwrap()
        }
        // Приоритет меняется только у потока, который вызвал функцию, — проверяем в отдельном потоке.
        let (before, after) = std::thread::spawn(|| {
            let before = nice();
            set_low_priority(true);
            (before, nice())
        })
        .join()
        .unwrap();
        assert!(after >= before.max(10), "{before} -> {after}");
    }
}
