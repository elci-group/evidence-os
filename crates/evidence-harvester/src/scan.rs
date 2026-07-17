//! File classification and best-effort content-date extraction.
//!
//! Content dates are found for: `.txt` (first 2KB), `.docx` (document.xml
//! with tags stripped, first 2KB of text), `.pdf` (first two pages via
//! `/usr/bin/pdftotext`, when installed). Everything is a graceful no-op on
//! error — a missing tool or a corrupt file simply yields no content date.

use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

use evidence_core::util;
use regex::Regex;

/// Roughly how much extracted text we scan for a date.
const DATE_SCAN_CHARS: usize = 2048;

/// Map a lowercase extension (no dot) to the chronology document type.
pub(crate) fn classify(ext: &str) -> &'static str {
    match ext {
        "eml" => "Email",
        "pdf" => "PDF",
        "docx" => "Word",
        "png" | "jpg" | "jpeg" | "tif" | "tiff" => "Image",
        "txt" => "Text",
        _ => "Other",
    }
}

/// Date found in the file's own content, if the format is one we read.
pub(crate) fn content_date(path: &Path, ext: &str) -> Option<String> {
    match ext {
        "txt" => txt_date(path),
        "docx" => docx_date(path),
        "pdf" => pdf_date(path),
        _ => None,
    }
}

/// Date found in in-memory attachment bytes. PDFs are skipped here (no
/// on-disk file to hand to pdftotext); docx and txt work from memory.
pub(crate) fn content_date_bytes(data: &[u8], ext: &str) -> Option<String> {
    match ext {
        "txt" => util::extract_date_from_text(&head_text(data)),
        "docx" => docx_date_from(Cursor::new(data)),
        _ => None,
    }
}

/// File modification time as an ISO date — last-resort fallback.
pub(crate) fn mtime_date(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let dt: chrono::DateTime<chrono::Local> = modified.into();
    Some(dt.format("%Y-%m-%d").to_string())
}

fn txt_date(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    util::extract_date_from_text(&head_text(&bytes))
}

fn docx_date(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    docx_date_from(file)
}

/// A .docx is a zip; the body text lives in word/document.xml.
fn docx_date_from<R: Read + std::io::Seek>(reader: R) -> Option<String> {
    let mut archive = zip::ZipArchive::new(reader).ok()?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .ok()?
        .read_to_string(&mut xml)
        .ok()?;
    util::extract_date_from_text(&head_text(strip_tags(&xml).as_bytes()))
}

fn pdf_date(path: &Path) -> Option<String> {
    let pdftotext = Path::new("/usr/bin/pdftotext");
    if !pdftotext.exists() {
        return None;
    }
    let out = std::process::Command::new(pdftotext)
        .arg("-l")
        .arg("2") // first two pages are enough for a date
        .arg(path)
        .arg("-") // text to stdout
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    util::extract_date_from_text(&head_text(&out.stdout))
}

/// Replace XML tags with spaces; dates survive in the remaining text.
fn strip_tags(xml: &str) -> String {
    let re = Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(xml, " ").into_owned()
}

/// First ~2KB of bytes as lossy text, cut on a char boundary.
fn head_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.chars().take(DATE_SCAN_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension() {
        assert_eq!(classify("eml"), "Email");
        assert_eq!(classify("pdf"), "PDF");
        assert_eq!(classify("docx"), "Word");
        assert_eq!(classify("png"), "Image");
        assert_eq!(classify("jpeg"), "Image");
        assert_eq!(classify("tiff"), "Image");
        assert_eq!(classify("txt"), "Text");
        assert_eq!(classify("xyz"), "Other");
    }

    #[test]
    fn finds_date_in_txt_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("note.txt");
        fs::write(&p, "Statement taken 18.06.2026 at the clinic.").unwrap();
        assert_eq!(content_date(&p, "txt").as_deref(), Some("2026-06-18"));
    }

    #[test]
    fn finds_date_in_docx_content() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("letter.docx");
        let xml = r#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>Dear Sally, following our meeting on 03/07/2026 we write</w:t></w:r></w:p></w:body></w:document>"#;
        let file = fs::File::create(&p).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        zw.start_file("word/document.xml", zip::write::FileOptions::default())
            .unwrap();
        zw.write_all(xml.as_bytes()).unwrap();
        zw.finish().unwrap();
        assert_eq!(content_date(&p, "docx").as_deref(), Some("2026-07-03"));
    }

    #[test]
    fn corrupt_docx_yields_none_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("broken.docx");
        fs::write(&p, b"not a zip at all").unwrap();
        assert_eq!(content_date(&p, "docx"), None);
    }

    #[test]
    fn finds_date_in_pdf_content() {
        if !Path::new("/usr/bin/pdftotext").exists() {
            return; // graceful no-op path is covered by the other fallbacks
        }
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/discharge-summary.pdf");
        assert_eq!(content_date(&p, "pdf").as_deref(), Some("2026-06-18"));
    }
}
