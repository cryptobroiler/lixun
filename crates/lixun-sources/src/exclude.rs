use std::path::Path;

pub fn path_excluded(path: &Path, substrings: &[String], regexes: &[regex::Regex]) -> bool {
    let s = path.to_string_lossy();
    substrings.iter().any(|p| s.contains(p.as_str())) || regexes.iter().any(|r| r.is_match(&s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn re(s: &str) -> regex::Regex {
        regex::Regex::new(s).unwrap()
    }

    #[test]
    fn empty_lists_never_exclude() {
        assert!(!path_excluded(&p("/any/path.txt"), &[], &[]));
    }

    #[test]
    fn substring_matches_anywhere_in_path() {
        let subs = vec!["node_modules".to_string()];
        assert!(path_excluded(
            &p("/home/me/proj/node_modules/lib/x.js"),
            &subs,
            &[]
        ));
        assert!(path_excluded(&p("/tmp/node_modules"), &subs, &[]));
        assert!(!path_excluded(&p("/home/me/src/main.rs"), &subs, &[]));
    }

    #[test]
    fn regex_anchored_to_extension() {
        let res = vec![re(r"\.pyc$")];
        assert!(path_excluded(&p("/a/b/foo.pyc"), &[], &res));
        assert!(!path_excluded(&p("/a/b/foo.py"), &[], &res));
        assert!(!path_excluded(&p("/a/b/.pyc.txt"), &[], &res));
    }

    #[test]
    fn regex_libreoffice_lock_file() {
        let res = vec![re(r"\.~lock\..*#$")];
        assert!(path_excluded(
            &p("/home/u/Docs/.~lock.Report.xlsx#"),
            &[],
            &res
        ));
        assert!(!path_excluded(&p("/home/u/Docs/Report.xlsx"), &[], &res));
    }

    #[test]
    fn substring_or_regex_short_circuits() {
        let subs = vec![".git".to_string()];
        let res = vec![re(r"\.log$")];
        assert!(path_excluded(&p("/repo/.git/HEAD"), &subs, &res));
        assert!(path_excluded(&p("/var/app.log"), &subs, &res));
        assert!(!path_excluded(&p("/home/u/notes.md"), &subs, &res));
    }

    #[test]
    fn substring_case_sensitive() {
        let subs = vec!["CACHE".to_string()];
        assert!(!path_excluded(&p("/home/u/.cache/x"), &subs, &[]));
        assert!(path_excluded(&p("/home/u/CACHE/x"), &subs, &[]));
    }
}
