//! PDF: текст постранично (страница = юнит).

use std::path::Path;

use anyhow::{Result, anyhow};

use super::Encrypted;
use super::doc::Doc;

pub fn extract(path: &Path, out: &mut Doc) -> Result<()> {
    let pages = pdf_extract::extract_text_by_pages(path).map_err(|e| {
        let msg = e.to_string();
        let lower = msg.to_lowercase();
        if lower.contains("password") || lower.contains("encrypt") || lower.contains("decrypt") {
            anyhow::Error::from(Encrypted)
        } else {
            anyhow!("{msg}")
        }
    })?;
    for (i, page) in pages.iter().enumerate() {
        if out.is_full() {
            break;
        }
        out.begin_unit(format!("Стр. {}", i + 1));
        for line in page.lines() {
            let line = line.trim();
            if !line.is_empty() {
                out.push(line);
                out.newline();
            }
        }
    }
    Ok(())
}
