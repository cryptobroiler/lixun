//! Lupa Extract — text extraction from various file formats.

use anyhow::Result;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

pub mod shell;

pub trait Extractor {
    fn extract(&self, bytes: &[u8]) -> Result<String>;
    fn mime_types(&self) -> &'static [&'static str];
}

#[derive(Clone, Debug)]
pub struct ExtractorCapabilities {
    pub timeout: Duration,
    pub has_pdftotext: bool,
    pub has_antiword: bool,
    pub has_catdoc: bool,
    pub has_libreoffice: bool,
}

impl ExtractorCapabilities {
    /// All tools assumed available, no timeout. Safe default for tests and
    /// any call site that runs before `init_capabilities` is invoked.
    pub fn all_available_no_timeout() -> Self {
        Self {
            timeout: Duration::ZERO,
            has_pdftotext: true,
            has_antiword: true,
            has_catdoc: true,
            has_libreoffice: true,
        }
    }

    /// Probe the environment for available extractors via `which::which`.
    /// Missing tools are logged at `warn` and disable their corresponding
    /// extractor; found tools log at `info` with the resolved path.
    pub fn probe(timeout: Duration) -> Self {
        fn probe_tool(name: &str) -> bool {
            match which::which(name) {
                Ok(p) => {
                    tracing::info!("extractor tool {}: found at {}", name, p.display());
                    true
                }
                Err(_) => {
                    tracing::warn!(
                        "extractor tool {}: NOT found — related content search disabled",
                        name
                    );
                    false
                }
            }
        }
        Self {
            timeout,
            has_pdftotext: probe_tool("pdftotext"),
            has_antiword: probe_tool("antiword"),
            has_catdoc: probe_tool("catdoc"),
            has_libreoffice: probe_tool("libreoffice"),
        }
    }
}

static EXTRACTOR_CAPS: OnceLock<ExtractorCapabilities> = OnceLock::new();

/// Initialize global extractor capabilities. Must be called once at daemon
/// startup before any extraction. A second call is ignored (OnceLock semantics)
/// and logged to stderr rather than panicking so test harnesses can coexist.
pub fn init_capabilities(caps: ExtractorCapabilities) {
    if EXTRACTOR_CAPS.set(caps).is_err() {
        eprintln!("lupa-extract: init_capabilities called more than once; keeping first value");
    }
}

fn capabilities() -> ExtractorCapabilities {
    EXTRACTOR_CAPS
        .get()
        .cloned()
        .unwrap_or_else(ExtractorCapabilities::all_available_no_timeout)
}

pub fn extractor_for_ext(ext: &str) -> Option<Box<dyn Extractor>> {
    extractor_for_ext_with_caps(ext, &capabilities())
}

pub(crate) fn extractor_for_ext_with_caps(
    ext: &str,
    caps: &ExtractorCapabilities,
) -> Option<Box<dyn Extractor>> {
    match ext {
        "pdf" if caps.has_pdftotext => Some(Box::new(PdfExtractor::new(caps.timeout))),
        "docx" | "xlsx" | "pptx" => Some(Box::new(OoxmlExtractor)),
        "odt" => Some(Box::new(OdtExtractor)),
        "rtf" => Some(Box::new(RtfExtractor)),
        "doc" if caps.has_antiword => Some(Box::new(ShellDocExtractor::new(caps.timeout))),
        "xls" if caps.has_catdoc => Some(Box::new(ShellXlsExtractor::new(caps.timeout))),
        "ppt" if caps.has_libreoffice => Some(Box::new(ShellPptExtractor::new(caps.timeout))),
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
        _ => {
            // Magic sniff for extension-less text files
            if ext.is_empty() {
                if let Ok(mut f) = std::fs::File::open(path) {
                    let mut buf = [0u8; 8192];
                    let n = std::io::Read::read(&mut f, &mut buf).unwrap_or(0);
                    let sniff = &buf[..n];
                    if n > 0 && !sniff.contains(&0) && std::str::from_utf8(sniff).is_ok() {
                        return extract_text_file(path);
                    }
                }
            }
            Ok(String::new())
        }
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

pub struct PdfExtractor {
    runner: shell::SystemRunner,
}

impl PdfExtractor {
    pub fn new(timeout: Duration) -> Self {
        Self {
            runner: shell::SystemRunner { timeout },
        }
    }
}

impl Extractor for PdfExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        shell::extract_pdf(bytes, &self.runner)
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

pub struct ShellDocExtractor {
    runner: shell::SystemRunner,
}

impl ShellDocExtractor {
    pub fn new(timeout: Duration) -> Self {
        Self {
            runner: shell::SystemRunner { timeout },
        }
    }
}

impl Extractor for ShellDocExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        shell::extract_doc(bytes, &self.runner)
    }
    fn mime_types(&self) -> &'static [&'static str] {
        &["application/msword"]
    }
}

pub struct ShellXlsExtractor {
    runner: shell::SystemRunner,
}

impl ShellXlsExtractor {
    pub fn new(timeout: Duration) -> Self {
        Self {
            runner: shell::SystemRunner { timeout },
        }
    }
}

impl Extractor for ShellXlsExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        shell::extract_xls(bytes, &self.runner)
    }
    fn mime_types(&self) -> &'static [&'static str] {
        &["application/vnd.ms-excel"]
    }
}

pub struct ShellPptExtractor {
    runner: shell::SystemRunner,
}

impl ShellPptExtractor {
    pub fn new(timeout: Duration) -> Self {
        Self {
            runner: shell::SystemRunner { timeout },
        }
    }
}

impl Extractor for ShellPptExtractor {
    fn extract(&self, bytes: &[u8]) -> Result<String> {
        shell::extract_ppt(bytes, &self.runner)
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

    #[test]
    fn test_magic_sniff_readme_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("README");
        std::fs::write(&path, "Hello README content").unwrap();
        let content = extract_path(&path).unwrap();
        assert!(content.contains("Hello README"));
    }

    #[test]
    fn test_magic_sniff_makefile_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Makefile");
        std::fs::write(&path, "all:\n\techo hi\n").unwrap();
        let content = extract_path(&path).unwrap();
        assert!(content.contains("echo hi"));
    }

    #[test]
    fn test_magic_sniff_binary_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("blob");
        std::fs::write(&path, b"abc\0def\0binary").unwrap();
        let content = extract_path(&path).unwrap();
        assert_eq!(content, "");
    }

    #[test]
    fn test_magic_sniff_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("EMPTY");
        std::fs::write(&path, b"").unwrap();
        let content = extract_path(&path).unwrap();
        assert_eq!(content, "");
    }

    #[test]
    fn test_magic_sniff_non_utf8_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("latin1bin");
        std::fs::write(&path, b"\xff\xfe\xfd").unwrap();
        let content = extract_path(&path).unwrap();
        assert_eq!(content, "");
    }

    #[test]
    fn test_extractor_for_ext_respects_caps_struct() {
        let caps = ExtractorCapabilities {
            timeout: std::time::Duration::from_secs(15),
            has_pdftotext: false,
            has_antiword: false,
            has_catdoc: false,
            has_libreoffice: false,
        };
        assert!(extractor_for_ext_with_caps("pdf", &caps).is_none());
        assert!(extractor_for_ext_with_caps("doc", &caps).is_none());
        assert!(extractor_for_ext_with_caps("xls", &caps).is_none());
        assert!(extractor_for_ext_with_caps("ppt", &caps).is_none());
        assert!(extractor_for_ext_with_caps("docx", &caps).is_some());
        assert!(extractor_for_ext_with_caps("odt", &caps).is_some());
        assert!(extractor_for_ext_with_caps("rtf", &caps).is_some());
    }

    #[test]
    fn test_extractor_for_ext_all_available_no_timeout() {
        let caps = ExtractorCapabilities::all_available_no_timeout();
        assert!(extractor_for_ext_with_caps("pdf", &caps).is_some());
        assert!(extractor_for_ext_with_caps("doc", &caps).is_some());
        assert!(extractor_for_ext_with_caps("rtf", &caps).is_some());
        assert!(extractor_for_ext_with_caps("xyz", &caps).is_none());
    }
}
