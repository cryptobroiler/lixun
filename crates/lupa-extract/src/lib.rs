//! Lupa Extract — text extraction from various file formats.

use anyhow::Result;
use std::path::Path;

pub mod shell;

/// Extractor trait: takes bytes, returns text.
pub trait Extractor {
    fn extract(&self, bytes: &[u8]) -> Result<String>;
    fn mime_types(&self) -> &'static [&'static str];
}

/// Registry of extractors by extension.
pub fn extractor_for_ext(ext: &str) -> Option<Box<dyn Extractor>> {
    match ext {
        "pdf" if shell::command_exists("pdftotext") => Some(Box::new(PdfExtractor)),
        "docx" | "xlsx" | "pptx" => Some(Box::new(OoxmlExtractor)),
        "odt" => Some(Box::new(OdtExtractor)),
        "rtf" => Some(Box::new(RtfExtractor)),
        "doc" if shell::command_exists("antiword") => Some(Box::new(ShellDocExtractor)),
        "xls" if shell::command_exists("catdoc") => Some(Box::new(ShellXlsExtractor)),
        "ppt" if shell::command_exists("libreoffice") => Some(Box::new(ShellPptExtractor)),
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
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extractor.extract(&bytes)));
        return match result {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("extractor panicked"),
        };
    }

    // Text-like: try encoding detection
    match ext.as_str() {
        "txt" | "md" | "log" | "csv" | "json" | "xml" | "html" | "htm" | "yaml" | "yml"
        | "toml" | "ini" | "cfg" | "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "hpp"
        | "java" | "go" | "sh" | "css" | "scss" | "sql" | "rb" => extract_text_file(path),
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
        shell::extract_pdf(bytes, &shell::SystemRunner)
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
        let mut file = archive
            .by_name("content.xml")
            .map_err(|_| anyhow::anyhow!("ODT: content.xml not found"))?;
        let mut content = String::new();
        std::io::Read::read_to_string(&mut file, &mut content)?;
        Ok(strip_xml_tags(&content).trim().to_string())
    }

    fn mime_types(&self) -> &'static [&'static str] {
        &["application/vnd.oasis.opendocument.text"]
    }
}

// --- RTF ---

pub struct RtfExtractor;

impl Extractor for RtfExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        let (_, tokens) = rtf_grimoire::tokenizer::parse(bytes)
            .map_err(|e| anyhow::anyhow!("RTF parse error: {:?}", e))?;

        let mut text_bytes: Vec<u8> = Vec::new();
        for token in &tokens {
            match token {
                rtf_grimoire::tokenizer::Token::Text(data) => {
                    text_bytes.extend_from_slice(data);
                }
                rtf_grimoire::tokenizer::Token::ControlWord { name, .. } if name == "par" => {
                    text_bytes.push(b'\n');
                }
                rtf_grimoire::tokenizer::Token::ControlWord { name, .. } if name == "line" => {
                    text_bytes.push(b'\n');
                }
                rtf_grimoire::tokenizer::Token::ControlSymbol('~') => {
                    text_bytes.push(b' ');
                }
                rtf_grimoire::tokenizer::Token::Newline(data) => {
                    text_bytes.extend_from_slice(data);
                }
                _ => {}
            }
        }

        // Try UTF-8 first, fallback to latin-1 (common RTF encoding)
        if let Ok(text) = String::from_utf8(text_bytes.clone()) {
            return Ok(text.trim().to_string());
        }
        // latin-1: each byte maps directly to Unicode code point U+0000..U+00FF
        let text: String = text_bytes.into_iter().map(|b| b as char).collect();
        Ok(text.trim().to_string())
    }

    fn mime_types(&self) -> &'static [&'static str] {
        &["application/rtf"]
    }
}

pub struct ShellDocExtractor;

impl Extractor for ShellDocExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        shell::extract_doc(bytes, &shell::SystemRunner)
    }
    fn mime_types(&self) -> &'static [&'static str] {
        &["application/msword"]
    }
}

pub struct ShellXlsExtractor;

impl Extractor for ShellXlsExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        shell::extract_xls(bytes, &shell::SystemRunner)
    }
    fn mime_types(&self) -> &'static [&'static str] {
        &["application/vnd.ms-excel"]
    }
}

pub struct ShellPptExtractor;

impl Extractor for ShellPptExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        shell::extract_ppt(bytes, &shell::SystemRunner)
    }
    fn mime_types(&self) -> &'static [&'static str] {
        &["application/vnd.ms-powerpoint"]
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
        assert_eq!(
            strip_xml_tags("<root>hello <b>world</b></root>"),
            "hello world"
        );
    }

    #[test]
    fn test_strip_xml_tags_empty() {
        assert_eq!(strip_xml_tags(""), "");
    }

    #[test]
    fn test_strip_xml_tags_only_tags() {
        assert_eq!(strip_xml_tags("<a><b><c></c></b></a>"), "");
    }

    #[test]
    fn test_strip_xml_tags_no_tags() {
        assert_eq!(strip_xml_tags("plain text"), "plain text");
    }

    #[test]
    fn test_strip_xml_tags_nested() {
        assert_eq!(
            strip_xml_tags("<p>hello <span>nested <b>deep</b></span> world</p>"),
            "hello nested deep world"
        );
    }

    #[test]
    fn test_strip_xml_tags_with_attributes() {
        assert_eq!(
            strip_xml_tags("<div class='foo' id='bar'>content</div>"),
            "content"
        );
    }

    #[test]
    fn test_extractor_for_ext_known_types() {
        assert!(
            extractor_for_ext("pdf").is_some()
                || std::process::Command::new("pdftotext").output().is_err()
        );
        assert!(extractor_for_ext("docx").is_some());
        assert!(extractor_for_ext("xlsx").is_some());
        assert!(extractor_for_ext("pptx").is_some());
        assert!(extractor_for_ext("odt").is_some());
        assert!(extractor_for_ext("rtf").is_some());
    }

    #[test]
    fn test_extractor_for_ext_unknown() {
        assert!(extractor_for_ext("xyz").is_none());
        assert!(extractor_for_ext("jpg").is_none());
        assert!(extractor_for_ext("png").is_none());
    }

    #[test]
    fn test_extract_text_file_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        std::fs::write(&path, "hello utf8 world").unwrap();
        let result = extract_path(&path).unwrap();
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_extract_path_unknown_ext() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.xyz");
        std::fs::write(&path, "some binary data").unwrap();
        let result = extract_path(&path).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_extract_path_nonexistent() {
        let result = extract_path(std::path::Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
    }
}
