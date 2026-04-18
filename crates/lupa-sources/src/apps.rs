//! Applications source — scan .desktop files.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use std::path::{Path, PathBuf};

/// Applications source — scans XDG_DATA_DIRS for .desktop files.
pub struct AppsSource {
    pub search_dirs: Vec<PathBuf>,
}

impl Default for AppsSource {
    fn default() -> Self {
        Self::new()
    }
}

impl AppsSource {
    pub fn new() -> Self {
        let mut dirs = Vec::new();

        if let Ok(xdd) = std::env::var("XDG_DATA_DIRS") {
            for dir in xdd.split(':') {
                dirs.push(PathBuf::from(dir).join("applications"));
            }
        } else {
            dirs.push(PathBuf::from("/usr/share/applications"));
            dirs.push(PathBuf::from("/usr/local/share/applications"));
        }

        if let Ok(local) = std::env::var("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(local).join("applications"));
        } else {
            if let Ok(home) = std::env::var("HOME") {
                dirs.push(PathBuf::from(home).join(".local/share/applications"));
            }
        }

        Self { search_dirs: dirs }
    }
}

impl AppsSource {
    fn parse_desktop_file(path: &Path) -> Option<(String, String, String, bool, Option<PathBuf>)> {
        let content = std::fs::read_to_string(path).ok()?;

        let mut name = String::new();
        let mut exec = String::new();
        let mut terminal = false;
        let mut in_desktop_entry = false;

        for line in content.lines() {
            if line.trim() == "[Desktop Entry]" {
                in_desktop_entry = true;
                continue;
            }
            if line.starts_with('[') {
                in_desktop_entry = false;
                continue;
            }
            if !in_desktop_entry {
                continue;
            }

            if let Some((key, val)) = line.split_once('=') {
                match key.trim() {
                    "Name" if name.is_empty() => {
                        name = val.trim().to_string();
                    }
                    "Exec" => {
                        exec = val.trim().to_string();
                        // Remove field codes (%f, %F, %u, etc.)
                        exec = exec
                            .split_whitespace()
                            .filter(|s| !s.starts_with('%'))
                            .collect::<Vec<_>>()
                            .join(" ");
                    }
                    "Terminal" => {
                        terminal = val.trim().eq_ignore_ascii_case("true");
                    }
                    _ => {}
                }
            }
        }

        if name.is_empty() || exec.is_empty() {
            return None;
        }

        let file_stem = path.file_stem()?.to_string_lossy().to_string();
        let working_dir = None; // Could be parsed from Path key

        Some((file_stem, name, exec, terminal, working_dir))
    }
}

impl crate::Source for AppsSource {
    fn name(&self) -> &'static str {
        "applications"
    }

    fn index_all(&self) -> Result<Vec<Document>> {
        let mut docs = Vec::new();

        for dir in &self.search_dirs {
            if !dir.exists() {
                continue;
            }

            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();

                if !path.extension().map(|e| e == "desktop").unwrap_or(false) {
                    continue;
                }

                if let Some((desktop_id, name, exec, terminal, working_dir)) =
                    Self::parse_desktop_file(&path)
                {
                    docs.push(Document {
                        id: DocId(format!("app:{}", desktop_id)),
                        category: Category::App,
                        title: name,
                        subtitle: desktop_id.clone(),
                        body: None,
                        path: path.to_string_lossy().to_string(),
                        mtime: 0,
                        size: 0,
                        action: Action::Launch {
                            exec,
                            terminal,
                            working_dir,
                        },
                        extract_fail: false,
                    });
                }
            }
        }

        tracing::info!("Applications: indexed {} apps", docs.len());
        Ok(docs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use std::fs;

    #[test]
    fn test_parse_desktop_file_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("firefox.desktop");
        fs::write(
            &path,
            r#"[Desktop Entry]
Name=Firefox
Exec=/usr/bin/firefox %u
Type=Application
Terminal=false
"#,
        )
        .unwrap();

        let result = AppsSource::parse_desktop_file(&path);
        assert!(result.is_some());
        let (id, name, exec, terminal, _) = result.unwrap();
        assert_eq!(id, "firefox");
        assert_eq!(name, "Firefox");
        assert_eq!(exec, "/usr/bin/firefox");
        assert!(!terminal);
    }

    #[test]
    fn test_parse_desktop_file_removes_field_codes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.desktop");
        fs::write(
            &path,
            r#"[Desktop Entry]
Name=Test
Exec=/usr/bin/test %f %F %u %U
"#,
        )
        .unwrap();

        let (_, _, exec, _, _) = AppsSource::parse_desktop_file(&path).unwrap();
        assert_eq!(exec, "/usr/bin/test");
    }

    #[test]
    fn test_parse_desktop_file_terminal_true() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("terminal.desktop");
        fs::write(
            &path,
            r#"[Desktop Entry]
Name=Terminal
Exec=/usr/bin/terminal
Terminal=true
"#,
        )
        .unwrap();

        let (_, _, _, terminal, _) = AppsSource::parse_desktop_file(&path).unwrap();
        assert!(terminal);
    }

    #[test]
    fn test_parse_desktop_file_missing_name() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("noname.desktop");
        fs::write(
            &path,
            r#"[Desktop Entry]
Exec=/usr/bin/something
"#,
        )
        .unwrap();

        assert!(AppsSource::parse_desktop_file(&path).is_none());
    }

    #[test]
    fn test_parse_desktop_file_missing_exec() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("noexec.desktop");
        fs::write(
            &path,
            r#"[Desktop Entry]
Name=No Exec
"#,
        )
        .unwrap();

        assert!(AppsSource::parse_desktop_file(&path).is_none());
    }

    #[test]
    fn test_parse_desktop_file_ignores_non_desktop_section() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.desktop");
        fs::write(
            &path,
            r#"[Desktop Action NewWindow]
Name=New Window
Exec=/usr/bin/test --new-window

[Desktop Entry]
Name=Test App
Exec=/usr/bin/test
"#,
        )
        .unwrap();

        let (_, name, exec, _, _) = AppsSource::parse_desktop_file(&path).unwrap();
        assert_eq!(name, "Test App");
        assert_eq!(exec, "/usr/bin/test");
    }

    #[test]
    fn test_index_all_with_temp_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let apps_dir = tmp.path().join("applications");
        fs::create_dir_all(&apps_dir).unwrap();

        fs::write(
            apps_dir.join("app1.desktop"),
            r#"[Desktop Entry]
Name=App One
Exec=/usr/bin/app1
"#,
        )
        .unwrap();

        fs::write(
            apps_dir.join("app2.desktop"),
            r#"[Desktop Entry]
Name=App Two
Exec=/usr/bin/app2
"#,
        )
        .unwrap();

        let source = AppsSource {
            search_dirs: vec![apps_dir],
        };

        let docs = source.index_all().unwrap();
        assert_eq!(docs.len(), 2);

        let names: Vec<_> = docs.iter().map(|d| &d.title).collect();
        assert!(names.contains(&&"App One".to_string()));
        assert!(names.contains(&&"App Two".to_string()));
    }
}
