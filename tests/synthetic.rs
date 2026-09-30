//! Крайние случаи: файлы, собранные вручную, битые, зашифрованные и с «чужим» расширением.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use file_search::extract::{Extracted, Limits, Status, UNIT_SEP, extract_file};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

fn run(path: &Path) -> Extracted {
    let size = fs::metadata(path).unwrap().len();
    extract_file(path, size, &Limits::default())
}

fn zip_file(dir: &TempDir, name: &str, parts: &[(&str, &str)]) -> PathBuf {
    let path = dir.path().join(name);
    let mut z = zip::ZipWriter::new(File::create(&path).unwrap());
    for (part, content) in parts {
        z.start_file(*part, SimpleFileOptions::default()).unwrap();
        z.write_all(content.as_bytes()).unwrap();
    }
    z.finish().unwrap();
    path
}

const ROOT_RELS_DOC: &str = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const ROOT_RELS_XL: &str = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
const ROOT_RELS_PPT: &str = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;

#[test]
fn docx_entities_tabs_breaks_and_deleted_text() {
    let dir = TempDir::new().unwrap();
    let doc = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
        <w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>Tom &amp; Jerry &#1078;&#x44B;</w:t></w:r><w:r><w:tab/><w:t>после табуляции</w:t></w:r></w:p>
        <w:p><w:r><w:t>строка</w:t><w:br/><w:t>вторая</w:t></w:r><w:del><w:r><w:delText>удалённое</w:delText></w:r></w:del><w:r><w:instrText> HYPERLINK "http://x" </w:instrText></w:r></w:p>
        </w:body></w:document>"#;
    let path = zip_file(&dir, "a.docx", &[("_rels/.rels", ROOT_RELS_DOC), ("word/document.xml", doc)]);
    let e = run(&path);
    assert_eq!(e.status, Status::Ok);
    assert!(e.text.contains("Tom & Jerry жы\tпосле табуляции"), "{:?}", e.text);
    assert!(e.text.contains("строка\nвторая"), "{:?}", e.text);
    assert!(!e.text.contains("удалённое"));
    assert!(!e.text.contains("HYPERLINK"));
    assert!(!e.text.contains("720"), "позиции табуляции из pPr не должны попадать в текст");
}

#[test]
fn xlsx_value_types_and_date_styles() {
    let dir = TempDir::new().unwrap();
    let wb = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><workbookPr date1904="0"/><sheets><sheet name="Итоги" sheetId="7" r:id="rId5"/></sheets></workbook>"#;
    let wb_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/data.xml"/><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let sst = r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>Простая</t></si><si><r><t>Состав</t></r><r><t xml:space="preserve">ная </t></r><r><t>строка</t></r><rPh><t>ФУРИГАНА</t></rPh></si></sst>"#;
    let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="164" formatCode="dd.mm.yyyy"/></numFmts><cellStyleXfs count="1"><xf numFmtId="14"/></cellStyleXfs><cellXfs count="3"><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="9"/></cellXfs></styleSheet>"#;
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
        <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" s="1"><v>45366</v></c><c r="D1" s="2"><v>0.25</v></c></row>
        <row r="2"><c r="A2" t="inlineStr"><is><t>встроенная</t></is></c><c r="B2" t="b"><v>1</v></c><c r="C2" t="e"><v>#DIV/0!</v></c><c r="D2" t="str"><f>A1&amp;"x"</f><v>формула-результат</v></c><c r="E2"><v>12345.0</v></c></row>
        </sheetData></worksheet>"#;
    let path = zip_file(
        &dir,
        "b.xlsx",
        &[
            ("_rels/.rels", ROOT_RELS_XL),
            ("xl/workbook.xml", wb),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/sharedStrings.xml", sst),
            ("xl/styles.xml", styles),
            ("xl/worksheets/data.xml", sheet),
        ],
    );
    let e = run(&path);
    assert_eq!(e.status, Status::Ok);
    assert_eq!(e.units, ["Лист «Итоги»"]);
    assert!(e.text.contains("Простая\tСоставная строка\t15.03.2024 2024-03-15\t0.25"), "{:?}", e.text);
    assert!(e.text.contains("встроенная\tформула-результат\t12345"), "{:?}", e.text);
    assert!(!e.text.contains("ФУРИГАНА"), "фонетическая подпись не должна индексироваться");
    assert!(!e.text.contains("DIV"), "ошибки ячеек не индексируются");
    assert!(e.meta.contains("Итоги"), "имя листа должно попасть в метаданные");
}

#[test]
fn pptx_slides_follow_presentation_order_not_file_names() {
    let dir = TempDir::new().unwrap();
    let pres = r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="300" r:id="rId9"/><p:sldId id="256" r:id="rId2"/></p:sldIdLst></p:presentation>"#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#;
    let slide = |t: &str| {
        format!(
            r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><a:p><a:r><a:t>{t}</a:t></a:r></a:p><a:fld type="slidenum"><a:t>‹#›</a:t></a:fld></p:sld>"#
        )
    };
    let (s1, s2) = (slide("файл первый"), slide("файл второй"));
    let path = zip_file(
        &dir,
        "c.pptx",
        &[
            ("_rels/.rels", ROOT_RELS_PPT),
            ("ppt/presentation.xml", pres),
            ("ppt/_rels/presentation.xml.rels", rels),
            ("ppt/slides/slide1.xml", &s1),
            ("ppt/slides/slide2.xml", &s2),
        ],
    );
    let e = run(&path);
    assert_eq!(e.units, ["Слайд 1", "Слайд 2"]);
    let parts: Vec<&str> = e.text.split(UNIT_SEP).collect();
    assert!(parts[0].contains("файл второй"), "первым в показе идёт slide2.xml: {:?}", e.text);
    assert!(parts[1].contains("файл первый"));
    assert!(!e.text.contains('‹'), "поле номера слайда не индексируется");
}

#[test]
fn dispatch_is_by_content_not_extension() {
    let dir = TempDir::new().unwrap();
    let doc = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>внутри docx</w:t></w:r></w:p></w:body></w:document>"#;
    let path = zip_file(&dir, "really_docx.xlsx", &[("_rels/.rels", ROOT_RELS_DOC), ("word/document.xml", doc)]);
    assert!(run(&path).text.contains("внутри docx"));

    // HTML, сохранённый Excel'ом-выгрузкой как .xls, и обычный текст с расширением .doc
    let html = dir.path().join("export.xls");
    fs::write(&html, "<html><body><table><tr><td>ячейка-выгрузки</td></tr></table></body></html>").unwrap();
    assert!(run(&html).text.contains("ячейка-выгрузки"));
    let plain = dir.path().join("plain.doc");
    fs::write(&plain, "просто текст").unwrap();
    assert!(run(&plain).text.contains("просто текст"));
}

#[test]
fn broken_files_never_panic_and_report_failure() {
    let dir = TempDir::new().unwrap();
    let good = fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract.docx")).unwrap();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated.docx", good[..good.len() / 2].to_vec()),
        ("garbage.xlsx", (0..4000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()),
        ("zip_header_only.pptx", b"PK\x03\x04garbage".to_vec()),
        ("ole_header_only.doc", [&[0xD0u8, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1][..], &[0u8; 600]].concat()),
        ("bad.pdf", b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec()),
        ("bad.odt", b"PK\x03\x04nothing".to_vec()),
    ];
    for (name, bytes) in cases {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        let e = run(&path);
        assert!(matches!(e.status, Status::Failed(_) | Status::Empty), "{name}: {:?}", e.status);
    }
    assert_eq!(
        run(&{
            let p = dir.path().join("empty.docx");
            File::create(&p).unwrap();
            p
        })
        .status,
        Status::Empty
    );
}

#[test]
fn password_protected_office_files_are_reported_as_encrypted() {
    let dir = TempDir::new().unwrap();
    // Зашифрованный OOXML — это OLE-контейнер с потоками EncryptionInfo и EncryptedPackage.
    let path = dir.path().join("secret.docx");
    let mut cf = cfb::create(&path).unwrap();
    cf.create_stream("/EncryptionInfo").unwrap().write_all(&[4, 0, 4, 0, 0, 0, 0, 0]).unwrap();
    cf.create_stream("/EncryptedPackage").unwrap().write_all(&[0u8; 64]).unwrap();
    cf.flush().unwrap();
    drop(cf);
    assert_eq!(run(&path).status, Status::Encrypted);

    // Старый .doc с флагом шифрования в FIB.
    let path = dir.path().join("secret.doc");
    let mut fib = vec![0u8; 1024];
    fib[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    fib[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
    fib[0x0A..0x0C].copy_from_slice(&0x0100u16.to_le_bytes());
    let mut cf = cfb::create(&path).unwrap();
    cf.create_stream("/WordDocument").unwrap().write_all(&fib).unwrap();
    cf.flush().unwrap();
    drop(cf);
    assert_eq!(run(&path).status, Status::Encrypted);
}

/// Запись PPT: заголовок (версия/экземпляр, тип, длина) + тело.
fn ppt_record(ver: u16, ty: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(ver.to_le_bytes());
    v.extend(ty.to_le_bytes());
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    v
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

#[test]
fn legacy_ppt_text_atoms_master_slides_are_skipped() {
    let dir = TempDir::new().unwrap();
    let slide_body = [
        ppt_record(0x0000, 0x0FA0, &utf16("Заголовок слайда")),
        ppt_record(0x0000, 0x0FA8, b"Latin body text"),
        ppt_record(0x0000, 0x0FA0, &utf16("*")),
    ]
    .concat();
    let master_body = ppt_record(0x0000, 0x0FA0, &utf16("Click to edit Master title style"));
    let stream = [ppt_record(0x000F, 0x03F8, &master_body), ppt_record(0x000F, 0x03EE, &slide_body)].concat();

    let path = dir.path().join("s.ppt");
    let mut cf = cfb::create(&path).unwrap();
    cf.create_stream("/PowerPoint Document").unwrap().write_all(&stream).unwrap();
    cf.flush().unwrap();
    drop(cf);

    let e = run(&path);
    assert_eq!(e.status, Status::Ok);
    assert!(e.text.contains("Заголовок слайда"));
    assert!(e.text.contains("Latin body text"));
    assert!(!e.text.contains("Master"), "образец слайдов не индексируется");
    assert!(!e.text.contains('*'));
}

#[test]
fn unknown_extensions_are_read_only_when_they_look_like_text() {
    let dir = TempDir::new().unwrap();
    let text = dir.path().join("notes.xyz");
    fs::write(&text, "файл с неизвестным расширением").unwrap();
    assert!(run(&text).text.contains("неизвестным"));

    let binary = dir.path().join("blob.xyz");
    fs::write(&binary, [0u8, 1, 2, 3, 0, 0, 255, 254, 0, 9, 0, 0]).unwrap();
    assert!(matches!(run(&binary).status, Status::Skipped(_)), "двоичный файл — не ошибка");

    let exe = dir.path().join("prog.exe");
    fs::write(&exe, b"MZ\x90\x00").unwrap();
    assert!(matches!(run(&exe).status, Status::Skipped(_)));
}

#[test]
fn text_limit_truncates_and_reports_it() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("big.txt");
    fs::write(&path, "слово ".repeat(100_000)).unwrap();
    let e = extract_file(
        &path,
        fs::metadata(&path).unwrap().len(),
        &Limits { max_text_bytes: 10_000, max_file_size: 1 << 30 },
    );
    assert!(e.truncated);
    assert!(e.text.len() <= 10_000);
    assert!(e.text.ends_with("слово") || e.text.ends_with("слов") || e.text.len() > 9_000);

    let too_big = extract_file(&path, 10, &Limits { max_text_bytes: 1000, max_file_size: 5 });
    assert!(matches!(too_big.status, Status::Skipped(_)));
}

#[test]
fn cp1251_and_utf16_text_files_are_decoded() {
    let dir = TempDir::new().unwrap();
    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode("Заявление на отпуск с двадцатого числа");
    let a = dir.path().join("a.txt");
    fs::write(&a, &bytes).unwrap();
    assert!(run(&a).text.contains("Заявление на отпуск"));

    let mut u16 = vec![0xFF, 0xFE];
    u16.extend("Юникод шестнадцать бит".encode_utf16().flat_map(|u| u.to_le_bytes()));
    let b = dir.path().join("b.txt");
    fs::write(&b, u16).unwrap();
    assert!(run(&b).text.contains("шестнадцать"));
}
