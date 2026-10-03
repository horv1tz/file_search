//! Сетевые папки: адреса вида `\\192.168.1.10\документы`, проверка доступности с таймаутом
//! и понятные объяснения сбоев вместо кодов Windows.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

/// Сколько ждём ответа сетевой папки. Недоступный компьютер отвечает таймаутом Windows только через полминуты,
/// и всё это время нельзя ни искать, ни индексировать остальное.
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(10);

/// Путь вида `\\сервер\ресурс\…` (в том числе `\\?\UNC\сервер\ресурс`). Диск в «длинной» записи `\\?\C:\` — не сетевой.
pub fn is_unc(path: &Path) -> bool {
    is_unc_str(&path.to_string_lossy())
}

pub fn is_unc_str(text: &str) -> bool {
    let s = text.replace('/', "\\");
    if let Some(rest) = s.strip_prefix("\\\\?\\") {
        return rest.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("unc\\"));
    }
    // `\\.\` — имена устройств, не сетевые папки.
    s.starts_with("\\\\") && !s.starts_with("\\\\.\\") && s.len() > 2 && !s[2..].starts_with('\\')
}

/// Папка лежит на другом компьютере: UNC-путь или сетевой диск Windows (`Z:` → `\\сервер\ресурс`).
pub fn is_network_path(path: &Path) -> bool {
    is_unc(path) || is_remote_drive(path)
}

#[cfg(windows)]
fn is_remote_drive(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
    }
    const DRIVE_REMOTE: u32 = 4;
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    let root: Vec<u16> =
        std::ffi::OsStr::new(&format!("{}:\\", bytes[0] as char)).encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: `root` — строка с нулевым окончанием, живущая до конца вызова.
    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
}

#[cfg(not(windows))]
fn is_remote_drive(_path: &Path) -> bool {
    false
}

/// Разбирает адрес, введённый пользователем: убирает кавычки и пробелы, приводит `//сервер/папка`
/// и `smb://сервер/папка` к виду Windows. Для `\\сервер` без папки возвращает подсказку.
pub fn parse_user_path(input: &str) -> Result<PathBuf, String> {
    parse_user_path_for(input, cfg!(windows))
}

pub fn parse_user_path_for(input: &str, windows: bool) -> Result<PathBuf, String> {
    let mut text = input.trim().trim_matches(['"', '\'']).trim().to_string();
    if text.is_empty() {
        return Err("Введите путь к папке, например \\\\192.168.1.10\\документы".to_string());
    }
    for scheme in ["smb://", "file://", "cifs://"] {
        if text.get(..scheme.len()).is_some_and(|p| p.eq_ignore_ascii_case(scheme)) {
            text = format!("//{}", &text[scheme.len()..]);
            break;
        }
    }
    let network_like = text.starts_with("\\\\") || (windows && text.starts_with("//"));
    if network_like && windows {
        text = text.replace('/', "\\");
        // `\\сервер\ресурс\` → без завершающего разделителя.
        while text.len() > 3 && text.ends_with('\\') {
            text.pop();
        }
        if !text.starts_with("\\\\?\\") && !text.starts_with("\\\\.\\") {
            let parts: Vec<&str> = text.trim_start_matches('\\').split('\\').filter(|p| !p.is_empty()).collect();
            if parts.len() < 2 {
                return Err(format!(
                    "Указан только компьютер: \\\\{}. Добавьте имя общей папки, например \\\\{}\\документы",
                    parts.first().copied().unwrap_or(""),
                    parts.first().copied().unwrap_or("сервер")
                ));
            }
        }
    }
    if !windows && text.starts_with("//") && !Path::new(&text).exists() {
        return Err("Адреса вида \\\\сервер\\папка работают только в Windows. На Linux и macOS смонтируйте \
                    папку и укажите путь к точке монтирования"
            .to_string());
    }
    Ok(PathBuf::from(text))
}

/// Пишет по-русски, что случилось при обращении к папке.
pub fn describe_io_error(err: &io::Error, path: &Path) -> String {
    let network = is_unc(path);
    let place = path.display();
    let hint = match raw_hint(err) {
        Some(hint) => hint,
        None => match err.kind() {
            io::ErrorKind::NotFound if network => {
                "сетевой путь не найден: проверьте адрес, имя общей папки и что компьютер включён".to_string()
            }
            io::ErrorKind::NotFound => "папка не найдена".to_string(),
            io::ErrorKind::PermissionDenied => "нет доступа: проверьте права на папку".to_string(),
            io::ErrorKind::TimedOut => "компьютер не отвечает".to_string(),
            _ => err.to_string(),
        },
    };
    format!("{place}: {hint}")
}

#[cfg(windows)]
fn raw_hint(err: &io::Error) -> Option<String> {
    let text = match err.raw_os_error()? {
        2 | 3 => "папка не найдена",
        5 => {
            "нет доступа. Откройте эту папку в проводнике и введите логин и пароль с галочкой «Запомнить», \
             или проверьте права пользователя на сервере"
        }
        53 | 67 => "сетевой путь не найден: проверьте адрес, имя общей папки и что компьютер включён",
        64 | 59 => "сеть оборвалась или компьютер недоступен",
        121 | 1231 | 1222 | 1460 => "компьютер не отвечает (истекло время ожидания)",
        1219 => {
            "к этому серверу уже подключились под другим именем: отключите прежнее подключение \
             (команда `net use * /delete`) и повторите"
        }
        1326 | 1244 => {
            "сервер не принял имя и пароль. Откройте папку в проводнике, введите верные данные \
             с галочкой «Запомнить»"
        }
        1203 | 1208 => "некорректный сетевой путь",
        _ => return None,
    };
    Some(text.to_string())
}

#[cfg(not(windows))]
fn raw_hint(_err: &io::Error) -> Option<String> {
    None
}

/// Проверяет, что путь — доступная папка, но не ждёт дольше `timeout`: обращение к выключенному компьютеру
/// в Windows зависает на десятки секунд. Для локальных путей поток не заводится.
pub fn check_dir(path: &Path, timeout: Duration) -> Result<(), String> {
    if !is_network_path(path) {
        return match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(format!("{} — не папка", path.display())),
            Err(e) => Err(describe_io_error(&e, path)),
        };
    }
    let (tx, rx) = channel();
    let owned = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(std::fs::metadata(&owned).map(|m| m.is_dir()));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err(format!("{} — не папка", path.display())),
        Ok(Err(e)) => Err(describe_io_error(&e, path)),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "{}: компьютер не ответил за {} с — проверьте адрес и что он включён",
            path.display(),
            timeout.as_secs()
        )),
        Err(RecvTimeoutError::Disconnected) => Err(format!("{}: не удалось проверить папку", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_paths_are_recognised() {
        assert!(is_unc_str(r"\\192.168.1.10\docs"));
        assert!(is_unc_str(r"\\server\share\dir\file.txt"));
        assert!(is_unc_str("//server/share"));
        assert!(is_unc_str(r"\\?\UNC\server\share"));
        assert!(!is_unc_str(r"\\?\C:\Users"));
        assert!(!is_unc_str(r"\\.\PhysicalDrive0"));
        assert!(!is_unc_str(r"C:\Users"));
        assert!(!is_unc_str("/home/user"));
        assert!(!is_unc_str(r"\\\"));
    }

    #[test]
    fn typed_addresses_are_normalised_for_windows() {
        let p = |s: &str| parse_user_path_for(s, true);
        assert_eq!(p(r"\\192.168.1.10\docs").unwrap(), PathBuf::from(r"\\192.168.1.10\docs"));
        assert_eq!(p(r#"  "\\srv\Общая папка\"  "#).unwrap(), PathBuf::from(r"\\srv\Общая папка"));
        assert_eq!(p("//srv/share/sub").unwrap(), PathBuf::from(r"\\srv\share\sub"));
        assert_eq!(p("smb://10.0.0.5/archive").unwrap(), PathBuf::from(r"\\10.0.0.5\archive"));
        assert_eq!(p(r"D:\Документы").unwrap(), PathBuf::from(r"D:\Документы"));
        assert!(p(r"\\192.168.1.10").unwrap_err().contains("общей папки"));
        assert!(p(r"\\192.168.1.10\").unwrap_err().contains("общей папки"));
        assert!(p("   ").is_err());
    }

    #[test]
    fn unc_addresses_are_refused_politely_outside_windows() {
        let err = parse_user_path_for(r"//server/share", false).unwrap_err();
        assert!(err.contains("Windows"), "{err}");
        assert_eq!(parse_user_path_for("/mnt/share", false).unwrap(), PathBuf::from("/mnt/share"));
    }

    #[test]
    fn local_directory_check() {
        let dir = std::env::temp_dir();
        assert!(check_dir(&dir, Duration::from_secs(1)).is_ok());
        let missing = dir.join("fsearch-no-such-dir-xyz");
        assert!(check_dir(&missing, Duration::from_secs(1)).unwrap_err().contains("fsearch-no-such-dir-xyz"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_error_codes_are_explained() {
        let unc = Path::new(r"\\srv\share");
        for (code, word) in [(53, "сетевой путь"), (5, "нет доступа"), (1326, "пароль"), (1219, "другим именем")]
        {
            let text = describe_io_error(&io::Error::from_raw_os_error(code), unc);
            assert!(text.contains(word), "{code}: {text}");
        }
    }
}
