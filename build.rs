//! Встраивает иконку и сведения о программе в .exe под Windows.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
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
