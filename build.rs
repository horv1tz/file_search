//! Встраивает в exe иконку (Windows) и модели распознавания текста (если они скачаны в assets/ocr).

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/ocr");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(ocr_embedded)");
    // Модели весят около 13 МБ и в репозитории не хранятся: их качает scripts/fetch_ocr_models.py перед сборкой выпуска.
    // Если файлов нет, программа собирается без них (распознавание ищет модели в папке данных).
    if ["det.onnx", "rec.onnx", "dict.txt"].iter().all(|f| Path::new("assets/ocr").join(f).exists()) {
        println!("cargo:rustc-cfg=ocr_embedded");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/icon.ico");
    resource.set("ProductName", "File Search");
    resource.set("FileDescription", "File Search - full-text search for documents");
    // Без иконки программа всё равно работает, поэтому сбой сборки ресурса — только предупреждение.
    if let Err(e) = resource.compile() {
        println!("cargo:warning=не удалось встроить иконку в exe: {e}");
    }
}
