//! Распознавание текста на картинках (сканы PDF).
//!
//! В Windows используется встроенное распознавание (`Windows.Media.Ocr`): оно не требует установки программ и
//! работает без интернета, но нуждается в языковом компоненте «Распознавание текста» нужного языка (русский ставится
//! в Параметры → Время и язык → Язык и регион). На других системах распознавание пока недоступно.
//!
//! Всё, что умеет только система, спрятано за [`Recognizer`]: остальной код (выбор страниц, лимиты, статусы)
//! от платформы не зависит и проверяется подменой распознавателя.

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
    /// Языки, для которых в системе есть распознавание (по-русски, как их называет система).
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

/// Сколько страниц распознаётся одновременно: распознавание занимает процессор, а страницы ещё и отрисовываются.
const MAX_PARALLEL: usize = 2;
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

/// Распознавание средствами системы, с ограничением числа одновременных страниц и защитой от серии сбоев.
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

/// Распознаватель системы, если он есть на этом компьютере.
pub fn system_recognizer() -> Option<&'static Recognizer<'static>> {
    available().then_some(&system)
}

#[cfg(windows)]
mod backend {
    use std::cell::{Cell, RefCell};

    use anyhow::{Context, Result, anyhow, bail};
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::core::HSTRING;

    use super::{EngineInfo, Frame};

    /// Каждому потоку нужна «квартира» COM, иначе любой вызов Windows Runtime завершается ошибкой.
    fn init_apartment() {
        thread_local! {
            static DONE: Cell<bool> = const { Cell::new(false) };
        }
        DONE.with(|done| {
            if !done.replace(true) {
                // Ошибка «уже инициализировано другим режимом» нам не мешает: поток и так в квартире.
                unsafe {
                    let _ = RoInitialize(RO_INIT_MULTITHREADED);
                }
            }
        });
    }

    fn create_engine() -> Result<OcrEngine> {
        // Сначала русский (он читает и латиницу), иначе — языки профиля пользователя Windows.
        if let Ok(language) = Language::CreateLanguage(&HSTRING::from("ru"))
            && let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language)
        {
            return Ok(engine);
        }
        OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| anyhow!("нет языка распознавания: {e}"))
    }

    pub fn info() -> EngineInfo {
        init_apartment();
        let languages: Vec<String> = OcrEngine::AvailableRecognizerLanguages()
            .map(|list| list.into_iter().filter_map(|l| l.DisplayName().ok()).map(|n| n.to_string()).collect())
            .unwrap_or_default();
        let available = create_engine().is_ok();
        let note = if available {
            String::new()
        } else {
            "В Windows не установлен язык распознавания текста. Добавьте русский язык: Параметры → Время и язык → \
             Язык и регион → Добавить язык (достаточно компонента «Распознавание текста»), затем перезапустите программу."
                .to_string()
        };
        EngineInfo { available, languages, note }
    }

    pub fn recognize(frame: &Frame) -> Result<String> {
        init_apartment();
        thread_local! {
            static ENGINE: RefCell<Option<OcrEngine>> = const { RefCell::new(None) };
        }
        ENGINE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(create_engine()?);
            }
            let engine = slot.as_ref().expect("движок создан выше");
            let max = OcrEngine::MaxImageDimension().unwrap_or(2600);
            if frame.width > max || frame.height > max {
                bail!("страница {}×{} больше предела распознавания ({max})", frame.width, frame.height);
            }
            // Windows ждёт порядок каналов BGRA.
            let mut bgra = frame.rgba.to_vec();
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            let writer = DataWriter::new().context("буфер картинки")?;
            writer.WriteBytes(&bgra).context("запись картинки")?;
            let buffer = writer.DetachBuffer().context("буфер картинки")?;
            let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
                &buffer,
                BitmapPixelFormat::Bgra8,
                frame.width as i32,
                frame.height as i32,
            )
            .context("картинка для распознавания")?;
            let result =
                engine.RecognizeAsync(&bitmap).context("запуск распознавания")?.join().context("распознавание")?;
            let mut text = String::new();
            for line in result.Lines().context("строки текста")? {
                text.push_str(&line.Text().context("текст строки")?.to_string());
                text.push('\n');
            }
            Ok(text)
        })
    }
}

#[cfg(not(windows))]
mod backend {
    use anyhow::{Result, bail};

    use super::{EngineInfo, Frame};

    pub fn info() -> EngineInfo {
        EngineInfo {
            available: false,
            languages: Vec::new(),
            note: "Распознавание текста на сканах пока работает только в Windows.".to_string(),
        }
    }

    pub fn recognize(_frame: &Frame) -> Result<String> {
        bail!("распознавание текста недоступно на этой системе")
    }
}
