//! Lupa Extract — text extraction from various file formats.

use anyhow::Result;
use std::path::Path;

/// Extractor trait: takes bytes, returns text.
pub trait Extractor {
    fn extract(&self, bytes: &[u8]) -> Result<String>;
    fn mime_types(&self) -> &'static [&'static str];
}

/// Registry of extractors by extension.
pub fn extractor_for_ext(ext: &str) -> Option<Box<dyn Extractor>> {
    match ext {
        "pdf" => Some(Box::new(PdfExtractor)),
        "docx" | "xlsx" | "pptx" => Some(Box::new(OoxmlExtractor)),
        "odt" => Some(Box::new(OdtExtractor)),
        "rtf" => None, // rtf-grimoire later
        "doc" => None,  // antiword later
        "xls" => None,  // catdoc later
        "ppt" => None,  // libreoffice later
        _ => None,
    }
}

/// Try to extract text from a file path.
pub fn extract_path(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if let Some(extractor) = extractor_for_ext(&ext) {
        let bytes = std::fs::read(path)?;
        return extractor.extract(&bytes);
    }

    // Text-like: try encoding detection
    match ext.as_str() {
        "txt" | "md" | "log" | "csv" | "json" | "xml" | "html" | "htm"
        | "yaml" | "yml" | "toml" | "ini" | "cfg" | "rs" | "py" | "js"
        | "ts" | "c" | "h" | "cpp" | "hpp" | "java" | "go" | "sh"
        | "css" | "scss" | "sql" | "rb" => extract_text_file(path),
        _ => Ok(String::new()),
    }
}

fn extract_text_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    if let Ok(text) = String::from_utf8(bytes.clone()) {
        return Ok(text);
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(&bytes, true);
    let encoding = detector.guess(None, true);
    let (text, _, _) = encoding.decode(&bytes);
    Ok(text.to_string())
}

// --- PDF ---

pub struct PdfExtractor;

impl Extractor for PdfExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        // pdf-extract works with paths; write to temp file
        let tmp = std::env::temp_dir().join(format!("lupa-pdf-{}.pdf", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        let text = pdf_extract::extract_text(&tmp).map_err(|e| anyhow::anyhow!("PDF extraction: {}", e))?;
        let _ = std::fs::remove_file(&tmp);
        Ok(text)
    }

    fn mime_types(&self) -> &'static [&'static str] {
        &["application/pdf"]
    }
}

// --- OOXML (DOCX/XLSX/PPTX) ---

pub struct OoxmlExtractor;

impl Extractor for OoxmlExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;

        // Try document.xml (DOCX), sheet XMLs (XLSX), slide XMLs (PPTX)
        let mut text = String::new();

        // DOCX: word/document.xml
        if let Ok(mut file) = archive.by_name("word/document.xml") {
            let mut content = String::new();
            std::io::Read::read_to_string(&mut file, &mut content)?;
            text.push_str(&strip_xml_tags(&content));
        }

        // XLSX: xl/worksheets/*.xml
        if text.is_empty() {
            let indices: Vec<_> = (0..archive.len()).collect();
            for i in indices {
                if let Ok(mut file) = archive.by_index(i) {
                    if file.name().starts_with("xl/worksheets/") && file.name().ends_with(".xml") {
                        let mut content = String::new();
                        std::io::Read::read_to_string(&mut file, &mut content)?;
                        text.push_str(&strip_xml_tags(&content));
                    }
                }
            }
        }

        // PPTX: ppt/slides/*.xml
        if text.is_empty() {
            let indices: Vec<_> = (0..archive.len()).collect();
            for i in indices {
                if let Ok(mut file) = archive.by_index(i) {
                    if file.name().starts_with("ppt/slides/") && file.name().ends_with(".xml") {
                        let mut content = String::new();
                        std::io::Read::read_to_string(&mut file, &mut content)?;
                        text.push_str(&strip_xml_tags(&content));
                    }
                }
            }
        }

        Ok(text.trim().to_string())
    }

    fn mime_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ]
    }
}

// --- ODT ---

pub struct OdtExtractor;

impl Extractor for OdtExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;
        let mut file = archive.by_name("content.xml")
            .map_err(|_| anyhow::anyhow!("ODT: content.xml not found"))?;
        let mut content = String::new();
        std::io::Read::read_to_string(&mut file, &mut content)?;
        Ok(strip_xml_tags(&content).trim().to_string())
    }

    fn mime_types(&self) -> &'static [&'static str] {
        &["application/vnd.oasis.opendocument.text"]
    }
}

fn strip_xml_tags(input: &str) -> String {
    let mut in_tag = false;
    let mut clean = String::new();
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => clean.push(c),
            _ => {}
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_xml_tags() {
        assert_eq!(strip_xml_tags("<root>hello <b>world</b></root>"), "hello world");
    }
}
