//! Распознавание текста на картинках (сканы PDF).
//!
//! Собственный движок ([`super::ocr_engine`]): модели PaddleOCR (русский и английский) запускает `rten`, чистый Rust.
//! Он не зависит от языковых пакетов системы и работает одинаково в Windows, Linux и macOS, без интернета.
//! Модели (около 13 МБ) встраиваются в exe при сборке выпуска; в обычной сборке их можно положить в папку данных
//! (`<данные>/file_search/ocr/`: `det.onnx`, `rec.onnx`, `dict.txt`).
//!
//! Всё, что умеет только движок, спрятано за [`Recognizer`]: остальной код (выбор страниц, лимиты, статусы)
//! проверяется подменой распознавателя.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

use anyhow::{Result, bail};

/// Картинка для распознавания: RGBA, 8 бит на канал, непрозрачная.
pub struct Frame<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// Распознаватель: возвращает текст картинки, строки разделены переводом строки.
pub type Recognizer<'a> = dyn Fn(&Frame) -> Result<String> + 'a;

/// Что умеет распознавание на этом компьютере.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub available: bool,
    /// Языки, которыми программа читает сканы; пусто, если распознавания нет.
    pub language: String,
    /// Читает ли движок русский текст (с нашими моделями — да).
    pub russian: bool,
    /// Языки распознавания по отдельности.
    pub languages: Vec<String>,
    /// Пояснение для пользователя: что делать, если распознавание недоступно.
    pub note: String,
}

/// Сведения о распознавании. Определяются один раз за запуск (создание движка не бесплатно).
pub fn engine_info() -> &'static EngineInfo {
    static INFO: OnceLock<EngineInfo> = OnceLock::new();
    INFO.get_or_init(backend::info)
}

pub fn available() -> bool {
    engine_info().available
}

/// Сколько страниц распознаётся одновременно: одна. Распознавание и так использует несколько потоков, а каждая
/// одновременная страница добавляет около 100 МБ памяти; компьютер при индексации сканов не должен задыхаться.
const MAX_PARALLEL: usize = 1;
/// После стольких неудач подряд распознавание отключается до перезапуска: один сбойный компонент системы не должен
/// тормозить индексацию остальных файлов.
const MAX_CONSECUTIVE_FAILURES: usize = 3;

static FAILURES: AtomicUsize = AtomicUsize::new(0);

struct Gate {
    busy: Mutex<usize>,
    freed: Condvar,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate { busy: Mutex::new(0), freed: Condvar::new() })
}

struct Permit;

impl Permit {
    fn acquire() -> Permit {
        let gate = gate();
        let mut busy = gate.busy.lock().unwrap();
        while *busy >= MAX_PARALLEL {
            busy = gate.freed.wait(busy).unwrap();
        }
        *busy += 1;
        Permit
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let gate = gate();
        *gate.busy.lock().unwrap() -= 1;
        gate.freed.notify_one();
    }
}

/// Распознавание собственным движком, с ограничением числа одновременных страниц и защитой от серии сбоев.
pub fn system(frame: &Frame) -> Result<String> {
    if FAILURES.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_FAILURES {
        bail!("распознавание отключено до перезапуска после нескольких ошибок подряд");
    }
    let _permit = Permit::acquire();
    match backend::recognize(frame) {
        Ok(text) => {
            FAILURES.store(0, Ordering::Relaxed);
            Ok(text)
        }
        Err(e) => {
            FAILURES.fetch_add(1, Ordering::Relaxed);
            Err(e)
        }
    }
}

/// Распознаватель, если модели найдены.
pub fn system_recognizer() -> Option<&'static Recognizer<'static>> {
    available().then_some(&system)
}

mod backend {
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};

    use anyhow::{Result, anyhow};

    use super::super::ocr_engine::{Engine, Options};
    use super::{EngineInfo, Frame};

    #[cfg(ocr_embedded)]
    const DET: &[u8] = include_bytes!("../../assets/ocr/det.onnx");
    #[cfg(ocr_embedded)]
    const REC: &[u8] = include_bytes!("../../assets/ocr/rec.onnx");
    #[cfg(ocr_embedded)]
    const DICT: &[u8] = include_bytes!("../../assets/ocr/dict.txt");

    /// Папка, где ищутся модели, если они не встроены: `<данные>/file_search/ocr/`.
    pub fn models_dir() -> PathBuf {
        dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("file_search").join("ocr")
    }

    fn load() -> Result<Engine, String> {
        #[cfg(ocr_embedded)]
        let engine = Engine::load_with(DET, REC, DICT, Options::default());
        #[cfg(not(ocr_embedded))]
        let engine = {
            let dir = models_dir();
            let read = |name: &str| {
                std::fs::read(dir.join(name)).map_err(|e| format!("нет файла {} ({e})", dir.join(name).display()))
            };
            let (det, rec, dict) = (read("det.onnx")?, read("rec.onnx")?, read("dict.txt")?);
            Engine::load_with(&det, &rec, &dict, Options::default())
        };
        engine.map_err(|e| format!("не удалось загрузить модели распознавания: {e}"))
    }

    /// Один движок на процесс: у него свой пул потоков, и модели занимают память.
    fn engine() -> Result<&'static Arc<Engine>, &'static str> {
        static ENGINE: OnceLock<Result<Arc<Engine>, String>> = OnceLock::new();
        ENGINE.get_or_init(|| load().map(Arc::new)).as_ref().map_err(|e| e.as_str())
    }

    /// Модели на месте (встроены или лежат в папке данных). Сами модели при этом не загружаются: движок занимает
    /// около 100–250 МБ памяти и поднимается только при первом распознавании.
    fn models_present() -> bool {
        if cfg!(ocr_embedded) {
            return true;
        }
        let dir = models_dir();
        ["det.onnx", "rec.onnx", "dict.txt"].iter().all(|f| dir.join(f).is_file())
    }

    pub fn info() -> EngineInfo {
        if models_present() {
            EngineInfo {
                available: true,
                language: "русский и английский".to_string(),
                russian: true,
                languages: vec!["русский".to_string(), "английский".to_string()],
                note: String::new(),
            }
        } else {
            EngineInfo {
                available: false,
                language: String::new(),
                russian: false,
                languages: Vec::new(),
                note: format!(
                    "Модели распознавания не найдены. Скачайте выпуск программы со встроенными моделями или положите \
                     файлы det.onnx, rec.onnx и dict.txt в папку {}.",
                    models_dir().display()
                ),
            }
        }
    }

    pub fn recognize(frame: &Frame) -> Result<String> {
        let engine = engine().map_err(|e| anyhow!("{e}"))?;
        Ok(engine.recognize(frame.rgba, frame.width, frame.height)?)
    }
}
