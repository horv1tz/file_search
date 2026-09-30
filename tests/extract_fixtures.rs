//! Извлечение текста из настоящих файлов (созданных Word/Excel/PowerPoint-совместимыми
//! библиотеками и LibreOffice) во всех поддерживаемых форматах.

use std::path::PathBuf;

use file_search::extract::{Extracted, Limits, Status, UNIT_SEP, extract_file};

fn extract(name: &str) -> Extracted {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let size = std::fs::metadata(&path).unwrap().len();
    extract_file(&path, size, &Limits::default())
}

/// Подпись юнита (листа/слайда/страницы), в котором встречается фрагмент.
fn unit_of(e: &Extracted, needle: &str) -> Option<String> {
    e.text.split(UNIT_SEP).position(|u| u.contains(needle)).and_then(|i| e.units.get(i).cloned())
}

#[test]
fn word_documents_in_every_format() {
    for name in ["contract.docx", "contract.doc", "contract.odt", "contract.rtf", "contract.pdf"] {
        let e = extract(name);
        assert_eq!(e.status, Status::Ok, "{name}");
        for needle in ["Договор аренды № 42", "Кракозябровая", "150000 рублей", "Депозит", "300000", "Zeppelin"] {
            assert!(e.text.contains(needle), "{name}: нет «{needle}»");
        }
    }
}

#[test]
fn word_headers_and_footers_are_included() {
    for name in ["contract.docx", "contract.doc", "contract.rtf", "contract.pdf"] {
        let e = extract(name);
        assert!(e.text.contains("Ромашка"), "{name}: нет верхнего колонтитула");
        assert!(e.text.contains("Лютик"), "{name}: нет нижнего колонтитула");
    }
}

#[test]
fn word_tables_keep_row_structure() {
    for name in ["contract.docx", "contract.doc"] {
        let e = extract(name);
        assert!(e.text.contains("Показатель\tЗначение\tПримечание"), "{name}: строка таблицы разорвана\n{}", e.text);
    }
}

#[test]
fn docx_metadata_is_extracted() {
    let e = extract("contract.docx");
    assert!(e.meta.contains("Договор аренды нежилого помещения"));
    assert!(e.meta.contains("Иванов"));
}

#[test]
fn excel_workbooks_in_every_format() {
    for name in ["sales.xlsx", "sales.xls", "sales.ods"] {
        let e = extract(name);
        assert_eq!(e.status, Status::Ok, "{name}");
        assert_eq!(e.units, ["Лист «Продажи»", "Лист «Sheet2»"], "{name}");
        for needle in ["Тюльпаны голландские", "INV-774411", "45678.5", "Quarterly revenue", "7654321"] {
            assert!(e.text.contains(needle), "{name}: нет «{needle}»");
        }
        assert_eq!(unit_of(&e, "Тюльпаны").as_deref(), Some("Лист «Продажи»"), "{name}");
        assert_eq!(unit_of(&e, "Quarterly").as_deref(), Some("Лист «Sheet2»"), "{name}");
    }
}

#[test]
fn excel_dates_are_searchable_in_both_notations() {
    for name in ["sales.xlsx", "sales.xls"] {
        let e = extract(name);
        assert!(e.text.contains("15.03.2024"), "{name}");
        assert!(e.text.contains("2024-03-15"), "{name}");
        assert!(e.text.contains("2024-05-20 14:30"), "{name}");
        assert!(!e.text.contains("45366"), "{name}: серийный номер даты попал в текст");
    }
}

#[test]
fn excel_rows_are_tab_separated() {
    let e = extract("sales.xlsx");
    assert!(e.text.contains("ООО Ромашка\tТюльпаны голландские\t120"), "{}", e.text);
}

#[test]
fn excel_comments_are_included() {
    for name in ["sales.xlsx", "sales.ods"] {
        assert!(extract(name).text.contains("Нептуновой"), "{name}");
    }
}

#[test]
fn powerpoint_in_every_format() {
    for name in ["strategy.pptx", "strategy.ppt", "strategy.odp", "strategy.pdf"] {
        let e = extract(name);
        assert_eq!(e.status, Status::Ok, "{name}");
        for needle in ["Стратегия развития 2025", "Ключевые метрики", "Ландыш", "Roadmap and milestones", "Хабибуллин"] {
            assert!(e.text.contains(needle), "{name}: нет «{needle}»");
        }
    }
}

#[test]
fn powerpoint_text_is_attributed_to_slides() {
    for (name, first, second, third) in [
        ("strategy.pptx", "Слайд 1", "Слайд 2", "Слайд 3"),
        ("strategy.odp", "Слайд 1", "Слайд 2", "Слайд 3"),
        ("strategy.pdf", "Стр. 1", "Стр. 2", "Стр. 3"),
    ] {
        let e = extract(name);
        assert_eq!(e.units, [first, second, third], "{name}");
        assert_eq!(unit_of(&e, "Стратегия развития").as_deref(), Some(first), "{name}");
        assert_eq!(unit_of(&e, "Ландыш").as_deref(), Some(second), "{name}");
        assert_eq!(unit_of(&e, "Хабибуллин").as_deref(), Some(third), "{name}");
    }
}

#[test]
fn speaker_notes_are_included() {
    for name in ["strategy.pptx", "strategy.ppt", "strategy.odp"] {
        let e = extract(name);
        assert!(e.text.contains("Стругацкого"), "{name}");
    }
    assert_eq!(unit_of(&extract("strategy.pptx"), "Стругацкого").as_deref(), Some("Слайд 2"));
}

#[test]
fn powerpoint_placeholders_are_not_indexed() {
    let ppt = extract("strategy.ppt");
    assert!(!ppt.text.lines().any(|l| l.trim() == "*"), "служебные «*» в .ppt");
    let odp = extract("strategy.odp");
    assert!(!odp.text.contains("<number>"), "заглушка номера страницы в .odp");
}

#[test]
fn legacy_word_headers_are_not_duplicated() {
    let e = extract("contract.doc");
    assert_eq!(e.text.matches("Ромашка").count(), 1);
    assert_eq!(e.text.matches("Лютик").count(), 1);
}
