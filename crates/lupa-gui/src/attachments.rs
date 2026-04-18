//! Attachment decoding and temp-file lifecycle for Thunderbird attachments.

use anyhow::Result;

pub(crate) fn decode_attachment(raw: &[u8], encoding: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    match encoding.to_ascii_lowercase().as_str() {
        "base64" => {
            let filtered: Vec<u8> = raw
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            Ok(base64::engine::general_purpose::STANDARD.decode(filtered)?)
        }
        "quoted-printable" => Ok(quoted_printable::decode(
            raw,
            quoted_printable::ParseMode::Robust,
        )?),
        "7bit" | "8bit" | "binary" | "" => Ok(raw.to_vec()),
        other => anyhow::bail!("unsupported transfer encoding: {other}"),
    }
}

pub(crate) fn sanitize_filename(s: &str) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| !matches!(*c, '/' | '\\' | '\0'))
        .collect();
    out = out.trim_start_matches('.').to_string();
    if out.is_empty() {
        out = "attachment".to_string();
    }
    if out.len() > 200 {
        let mut cut = 200;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

/// Return the secure per-user runtime directory. If `XDG_RUNTIME_DIR` is not set,
/// refuse the `/tmp` fallback (world-writable and vulnerable to symlink attacks)
/// and return an informative error instead.
pub(crate) fn secure_runtime_dir_from_env(
    xdg_runtime_dir: Option<&std::path::Path>,
) -> Result<std::path::PathBuf> {
    let dir = xdg_runtime_dir.ok_or_else(|| {
        anyhow::anyhow!(
            "XDG_RUNTIME_DIR is not set; refusing to use /tmp fallback (security: \
             /tmp is world-writable and vulnerable to symlink attacks). \
             If running outside a systemd-logind session, set XDG_RUNTIME_DIR to a \
             private directory you own (e.g. ~/.cache/lupa-runtime with 0700 perms)."
        )
    })?;
    Ok(dir.to_path_buf())
}

pub(crate) fn sweep_stale_attachments(dir: &std::path::Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if now.duration_since(mtime).unwrap_or_default() > max_age {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_decode_attachment_base64() {
        use base64::Engine;
        let plain = b"Hello World";
        let encoded = base64::engine::general_purpose::STANDARD.encode(plain);
        let decoded = decode_attachment(encoded.as_bytes(), "base64").unwrap();
        assert_eq!(decoded, plain);
    }

    #[test]
    fn test_decode_attachment_base64_strips_whitespace() {
        let raw = b"SGVs\r\nbG8=";
        let decoded = decode_attachment(raw, "base64").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_decode_attachment_base64_case_insensitive_encoding_label() {
        let raw = b"SGVsbG8=";
        let decoded = decode_attachment(raw, "BASE64").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_decode_attachment_qp() {
        let decoded = decode_attachment(b"Hello=20World", "quoted-printable").unwrap();
        assert_eq!(decoded, b"Hello World");
    }

    #[test]
    fn test_decode_attachment_passthrough_variants() {
        for enc in ["7bit", "8bit", "binary", ""] {
            let decoded = decode_attachment(b"Hello World", enc).unwrap();
            assert_eq!(decoded, b"Hello World", "encoding={enc}");
        }
    }

    #[test]
    fn test_decode_attachment_unknown_encoding_errors() {
        let result = decode_attachment(b"data", "rot13");
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_filename_strips_path_traversal() {
        let s = sanitize_filename("../../../etc/passwd");
        assert!(!s.contains('/'));
        assert!(!s.contains('\\'));
    }

    #[test]
    fn test_sanitize_filename_strips_backslash_and_nul() {
        let s = sanitize_filename("foo\\bar\0baz");
        assert_eq!(s, "foobarbaz");
    }

    #[test]
    fn test_sanitize_filename_dot_prefix_stripped() {
        assert_eq!(sanitize_filename(".hidden"), "hidden");
        assert_eq!(sanitize_filename("....multi"), "multi");
    }

    #[test]
    fn test_sanitize_filename_empty_falls_back() {
        assert_eq!(sanitize_filename(""), "attachment");
        assert_eq!(sanitize_filename("///"), "attachment");
        assert_eq!(sanitize_filename("..."), "attachment");
    }

    #[test]
    fn test_sanitize_filename_length_cap() {
        let long = "a".repeat(500);
        let out = sanitize_filename(&long);
        assert!(out.len() <= 200);
    }

    #[test]
    fn test_sweep_stale_attachments() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lupa-sweep-test-{ts}"));
        std::fs::create_dir_all(&dir).unwrap();

        let old_path = dir.join("old.bin");
        let fresh_path = dir.join("fresh.bin");
        std::fs::write(&old_path, b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        std::fs::write(&fresh_path, b"fresh").unwrap();

        sweep_stale_attachments(&dir, std::time::Duration::from_millis(75));

        let old_exists = old_path.exists();
        let fresh_exists = fresh_path.exists();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!old_exists, "stale file should be swept");
        assert!(fresh_exists, "fresh file should survive");
    }

    #[test]
    fn test_sweep_stale_attachments_nonexistent_dir_is_noop() {
        let p = std::path::Path::new("/nonexistent/lupa-sweep-test-path");
        sweep_stale_attachments(p, std::time::Duration::from_secs(1));
    }

    #[test]
    fn test_sanitize_filename_multibyte_no_panic_at_boundary() {
        let input = "あ".repeat(100);
        assert_eq!(input.len(), 300);
        let out = sanitize_filename(&input);
        assert!(out.len() <= 200);
    }

    #[test]
    fn test_secure_runtime_dir_returns_xdg_when_set() {
        let p = Path::new("/run/user/1000");
        let result = secure_runtime_dir_from_env(Some(p)).unwrap();
        assert_eq!(result, p);
    }

    #[test]
    fn test_secure_runtime_dir_refuses_fallback() {
        let result = secure_runtime_dir_from_env(None);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.to_lowercase().contains("xdg_runtime_dir"),
            "error message should mention XDG_RUNTIME_DIR, got: {err}"
        );
    }

    #[test]
    fn test_sanitize_filename_emoji_no_panic_at_boundary() {
        let input = "😀".repeat(60);
        assert!(input.len() > 200);
        let out = sanitize_filename(&input);
        assert!(out.len() <= 200);
    }
}
