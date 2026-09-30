use std::path::Path;

use anyhow::{Result, bail};

pub fn serve(_dir: &Path, _port: u16, _open: bool) -> Result<()> {
    bail!("веб-интерфейс ещё не готов")
}
