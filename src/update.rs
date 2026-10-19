//! Обновления программы: проверка последнего выпуска на GitHub, загрузка и установка.
//!
//! Выпуски публикуются в GitHub Releases ([`RELEASES_URL`]). Для Windows там лежит готовый `fsearch-gui-windows-x64.exe`
//! и файл контрольных сумм `SHA256SUMS.txt`: загруженный файл проверяется по нему, и только после этого подменяет
//! работающую программу (запущенный exe в Windows нельзя перезаписать, но можно переименовать). На других системах
//! обновление сводится к открытию страницы выпуска.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Репозиторий, из которого берутся выпуски.
pub const REPO: &str = "horv1tz/file_search";
pub const RELEASES_URL: &str = "https://github.com/horv1tz/file_search/releases";
const API_BASE: &str = "https://api.github.com";
/// Имя файла окна для Windows в выпуске (без номера версии, чтобы ссылка не зависела от неё).
const WINDOWS_GUI_ASSET: &str = "fsearch-gui-windows-x64.exe";
const CHECKSUMS_ASSET: &str = "SHA256SUMS.txt";
/// Больше этого выпуск весить не может: защита от бесконечного ответа.
const MAX_DOWNLOAD: u64 = 200 << 20;

/// Номер версии вида `0.1.6`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Разбирает `0.1.6`, `v0.1.6`, `0.1.6-beta` (хвост после номера отбрасывается).
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let core = text.split(['-', '+', ' ']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let major = parts.next()??;
        let minor = parts.next().unwrap_or(Some(0))?;
        let patch = parts.next().unwrap_or(Some(0))?;
        Some(Version(major, minor, patch))
    }

    /// Версия этой сборки.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("версия пакета задана в Cargo.toml")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// Опубликованный выпуск.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// Страница выпуска.
    pub url: String,
    /// Описание выпуска (Markdown).
    pub notes: String,
    /// Дата публикации, `ГГГГ-ММ-ДД`.
    pub published: String,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// Файл, которым можно заменить эту программу на этом компьютере (пока — только окно для Windows x64).
    pub fn installable_asset(&self) -> Option<&Asset> {
        self.asset(&self_update_asset()?)
    }
}

/// Имя файла выпуска, которым программа может заменить себя на этой системе: окно для Windows x64.
/// Служебная переменная `FSEARCH_UPDATE_ASSET` задаёт другое имя — для проверок на других системах.
fn self_update_asset() -> Option<String> {
    if let Ok(name) = std::env::var("FSEARCH_UPDATE_ASSET")
        && !name.is_empty()
    {
        return Some(name);
    }
    can_self_update().then(|| WINDOWS_GUI_ASSET.to_string())
}

/// Может ли программа обновить сама себя (Windows x64 с готовым exe в выпуске).
pub fn can_self_update() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

/// Клиент GitHub Releases.
pub struct Client {
    api_base: String,
    agent: ureq::Agent,
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

impl Client {
    pub fn new() -> Client {
        // Служебная переменная для проверок: адрес другого сервера вместо GitHub.
        match std::env::var("FSEARCH_UPDATE_API") {
            Ok(base) if !base.is_empty() => Client::with_base(&base),
            _ => Client::with_base(API_BASE),
        }
    }

    /// Клиент с другим адресом API (для проверок на локальном сервере).
    pub fn with_base(api_base: &str) -> Client {
        let config = ureq::Agent::config_builder()
            .user_agent(format!("fsearch/{} (+https://github.com/{REPO})", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(20)))
            .build();
        Client { api_base: api_base.trim_end_matches('/').to_string(), agent: config.into() }
    }

    /// Последний опубликованный выпуск.
    pub fn latest(&self) -> Result<Release> {
        let url = format!("{}/repos/{REPO}/releases/latest", self.api_base);
        let mut response = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| anyhow!("{}", friendly(&e)))?;
        let api: ApiRelease = response.body_mut().read_json().context("ответ сервера непонятен")?;
        let version = Version::parse(&api.tag_name)
            .ok_or_else(|| anyhow!("непонятный номер версии в выпуске: {}", api.tag_name))?;
        Ok(Release {
            version,
            tag: api.tag_name,
            url: api.html_url,
            notes: api.body.unwrap_or_default(),
            published: api.published_at.unwrap_or_default().chars().take(10).collect(),
            assets: api
                .assets
                .into_iter()
                .map(|a| Asset { name: a.name, url: a.browser_download_url, size: a.size })
                .collect(),
        })
    }

    /// Выпуск новее этой сборки, если он есть.
    pub fn check(&self) -> Result<Option<Release>> {
        let latest = self.latest()?;
        Ok((latest.version > Version::current()).then_some(latest))
    }

    /// Контрольные суммы файлов выпуска: имя файла → SHA-256 в шестнадцатеричном виде.
    pub fn checksums(&self, release: &Release) -> Result<HashMap<String, String>> {
        let asset = release.asset(CHECKSUMS_ASSET).ok_or_else(|| anyhow!("в выпуске нет файла контрольных сумм"))?;
        let mut response = self.agent.get(&asset.url).call().map_err(|e| anyhow!("{}", friendly(&e)))?;
        let text = response.body_mut().with_config().limit(1 << 20).read_to_string()?;
        Ok(parse_checksums(&text))
    }

    /// Загружает файл выпуска в `to`, сообщая о ходе (`(загружено, всего)`) через `progress`.
    pub fn download(&self, asset: &Asset, to: &Path, progress: &dyn Fn(u64, u64)) -> Result<()> {
        if asset.size > MAX_DOWNLOAD {
            bail!("файл выпуска подозрительно большой ({} байт)", asset.size);
        }
        let mut response = self.agent.get(&asset.url).call().map_err(|e| anyhow!("{}", friendly(&e)))?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(asset.size);
        let mut reader = response.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
        let mut file = File::create(to).with_context(|| format!("не удалось создать {}", to.display()))?;
        let mut buf = vec![0u8; 64 << 10];
        let mut done = 0u64;
        progress(0, total);
        loop {
            let n = reader.read(&mut buf).context("обрыв загрузки")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).context("не удалось записать файл")?;
            done += n as u64;
            progress(done, total);
        }
        file.flush()?;
        if asset.size > 0 && done != asset.size {
            bail!("загружено {done} байт вместо {}", asset.size);
        }
        Ok(())
    }

    /// Загружает файл для этой системы и проверяет его контрольную сумму. Возвращает путь к загруженному файлу,
    /// лежащему рядом с программой (чтобы подмена была переименованием на том же диске).
    pub fn download_update(&self, release: &Release, progress: &dyn Fn(u64, u64)) -> Result<PathBuf> {
        let asset = release.installable_asset().ok_or_else(|| anyhow!("для этой системы нет готового файла"))?;
        let sums = self.checksums(release)?;
        let expected = sums
            .get(&asset.name)
            .ok_or_else(|| anyhow!("для {} нет контрольной суммы в выпуске", asset.name))?
            .to_lowercase();
        let exe = std::env::current_exe().context("не удалось узнать, где лежит программа")?;
        let target = exe.with_file_name(format!("{}.update.tmp", exe_stem(&exe)));
        let _ = fs::remove_file(&target);
        if let Err(e) = self.download(asset, &target, progress) {
            let _ = fs::remove_file(&target);
            return Err(e);
        }
        let actual = sha256_file(&target)?;
        if actual != expected {
            let _ = fs::remove_file(&target);
            bail!("контрольная сумма загруженного файла не совпала — файл повреждён или подменён");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o755));
        }
        Ok(target)
    }
}

fn exe_stem(exe: &Path) -> String {
    exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "fsearch-gui".into())
}

/// Сообщение об ошибке сети по-русски.
fn friendly(e: &ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(404) => "выпуск не найден".to_string(),
        ureq::Error::StatusCode(403) | ureq::Error::StatusCode(429) => {
            "сервер GitHub просит подождать (слишком много запросов)".to_string()
        }
        ureq::Error::StatusCode(code) => format!("сервер ответил ошибкой {code}"),
        ureq::Error::Timeout(_) => "сервер не отвечает".to_string(),
        ureq::Error::HostNotFound => "нет связи с github.com (проверьте интернет)".to_string(),
        ureq::Error::Tls(_) => "не удалось установить защищённое соединение с github.com".to_string(),
        ureq::Error::Io(io) => format!("нет связи: {io}"),
        other => other.to_string(),
    }
}

/// Разбирает `SHA256SUMS.txt`: строки вида `<hex>  <имя файла>`.
pub fn parse_checksums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| (name.to_string(), hash.to_lowercase()))
        })
        .collect()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("не удалось открыть {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 << 10];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Имя, под которым прежняя программа остаётся рядом после подмены (удаляется при следующем запуске).
pub fn old_binary_path(exe: &Path) -> PathBuf {
    exe.with_file_name(format!("{}.old.exe", exe_stem(exe)))
}

/// Ставит загруженный файл на место работающей программы: прежний файл переименовывается в `*.old.exe`
/// (запущенный exe нельзя удалить, но можно переименовать), новый занимает его имя. При сбое всё возвращается
/// как было. Возвращает путь, по которому нужно запустить новую программу.
pub fn install_binary(current: &Path, downloaded: &Path) -> Result<PathBuf> {
    let old = old_binary_path(current);
    let _ = fs::remove_file(&old);
    fs::rename(current, &old)
        .with_context(|| format!("не удалось освободить {} (нет прав на папку?)", current.display()))?;
    if let Err(e) = fs::rename(downloaded, current) {
        let _ = fs::rename(&old, current);
        return Err(anyhow!("не удалось поставить новый файл на место: {e}"));
    }
    Ok(current.to_path_buf())
}

/// Подменяет эту программу загруженным файлом (см. [`install_binary`]).
pub fn install_self(downloaded: &Path) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("не удалось узнать, где лежит программа")?;
    install_binary(&exe, downloaded)
}

/// Убирает недокачанный или не поставленный файл обновления рядом с программой.
pub fn remove_stale_download() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(exe.with_file_name(format!("{}.update.tmp", exe_stem(&exe))));
    }
}

/// Убирает прежний exe, оставшийся после обновления. Возвращает `false`, если он ещё есть: прежний процесс
/// может закрываться и держать файл — тогда стоит повторить позже.
pub fn remove_old_binary() -> bool {
    let Ok(exe) = std::env::current_exe() else { return true };
    let old = old_binary_path(&exe);
    let _ = fs::remove_file(&old);
    !old.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(Version::parse("v0.1.6"), Some(Version(0, 1, 6)));
        assert_eq!(Version::parse("0.2"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("1.0.0-beta.1"), Some(Version(1, 0, 0)));
        assert_eq!(Version::parse("latest"), None);
        assert!(Version(0, 1, 10) > Version(0, 1, 9));
        assert!(Version(0, 2, 0) > Version(0, 1, 99));
        assert_eq!(Version(0, 1, 6).to_string(), "0.1.6");
        assert!(Version::current() >= Version(0, 1, 5));
    }

    #[test]
    fn checksum_file_is_parsed() {
        let text = "dd35  bad\n\
                    0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  fsearch-gui-windows-x64.exe\n\
                    ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789 *fsearch-v0.1.5-windows-x64.zip\n";
        let sums = parse_checksums(text);
        assert_eq!(sums.len(), 2);
        assert_eq!(
            sums["fsearch-gui-windows-x64.exe"],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(sums["fsearch-v0.1.5-windows-x64.zip"].starts_with("abcdef"));
    }

    #[test]
    fn installing_swaps_files_and_rolls_back_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("fsearch-gui.exe");
        let downloaded = dir.path().join("fsearch-gui.update.tmp");
        fs::write(&current, b"old").unwrap();
        fs::write(&downloaded, b"new").unwrap();
        assert_eq!(install_binary(&current, &downloaded).unwrap(), current);
        assert_eq!(fs::read(&current).unwrap(), b"new");
        assert_eq!(fs::read(old_binary_path(&current)).unwrap(), b"old");
        assert!(!downloaded.exists());

        // Загруженного файла нет — прежняя программа возвращается на место.
        let err = install_binary(&current, &downloaded).unwrap_err();
        assert!(err.to_string().contains("на место"), "{err}");
        assert_eq!(fs::read(&current).unwrap(), b"new");
    }

    /// Крошечный сервер, изображающий GitHub: отдаёт описание выпуска, контрольные суммы и сам файл.
    fn fake_github(payload: Vec<u8>, sums: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        let release = format!(
            r#"{{"tag_name":"v9.9.9","html_url":"{base}/releases/tag/v9.9.9","body":"Новое","published_at":"2026-10-11T10:00:00Z",
               "assets":[{{"name":"{WINDOWS_GUI_ASSET}","browser_download_url":"{base}/dl/exe","size":{}}},
                         {{"name":"{CHECKSUMS_ASSET}","browser_download_url":"{base}/dl/sums","size":{}}}]}}"#,
            payload.len(),
            sums.len()
        );
        let payload = Arc::new(payload);
        thread::spawn(move || {
            for request in server.incoming_requests() {
                let url = request.url().to_string();
                let response = if url.ends_with("/releases/latest") {
                    tiny_http::Response::from_string(release.clone())
                } else if url.ends_with("/dl/sums") {
                    tiny_http::Response::from_string(sums.clone())
                } else if url.ends_with("/dl/exe") {
                    tiny_http::Response::from_data(payload.to_vec())
                } else {
                    tiny_http::Response::from_string("нет").with_status_code(404)
                };
                let _ = request.respond(response);
            }
        });
        base
    }

    #[test]
    fn latest_release_is_fetched_and_compared() {
        let base = fake_github(b"exe".to_vec(), String::new());
        let client = Client::with_base(&base);
        let release = client.latest().unwrap();
        assert_eq!(release.version, Version(9, 9, 9));
        assert_eq!(release.published, "2026-10-11");
        assert_eq!(release.notes, "Новое");
        assert_eq!(release.asset(WINDOWS_GUI_ASSET).map(|a| a.size), Some(3));
        assert!(client.check().unwrap().is_some(), "9.9.9 новее текущей версии");
        let err = Client::with_base("http://127.0.0.1:1").latest().unwrap_err();
        assert!(err.to_string().contains("нет связи"), "{err}");
    }

    #[test]
    fn downloads_are_verified_against_checksums() {
        let payload = b"pretend this is an exe".to_vec();
        let good = format!("{:x}", Sha256::digest(&payload));
        let base = fake_github(payload.clone(), format!("{good}  {WINDOWS_GUI_ASSET}\n"));
        let client = Client::with_base(&base);
        let release = client.latest().unwrap();
        assert_eq!(client.checksums(&release).unwrap()[WINDOWS_GUI_ASSET], good);

        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("got.bin");
        let seen = std::sync::Mutex::new(Vec::new());
        client
            .download(release.asset(WINDOWS_GUI_ASSET).unwrap(), &to, &|done, total| {
                seen.lock().unwrap().push((done, total))
            })
            .unwrap();
        assert_eq!(fs::read(&to).unwrap(), payload);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.first(), Some(&(0, payload.len() as u64)));
        assert_eq!(seen.last(), Some(&(payload.len() as u64, payload.len() as u64)));
        assert_eq!(sha256_file(&to).unwrap(), good);

        // Подменённый файл не проходит проверку.
        let base = fake_github(b"tampered".to_vec(), format!("{good}  {WINDOWS_GUI_ASSET}\n"));
        let client = Client::with_base(&base);
        let release = client.latest().unwrap();
        if can_self_update() {
            let err = client.download_update(&release, &|_, _| {}).unwrap_err();
            assert!(err.to_string().contains("контрольная сумма"), "{err}");
        } else {
            assert!(release.installable_asset().is_none());
        }
    }
}
