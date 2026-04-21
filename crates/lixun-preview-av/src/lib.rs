//! Audio/video preview plugin.
//!
//! Thin wrapper around `gtk::Video`. GTK 4.12+ ships its own
//! `GtkMediaFile` backend that uses GStreamer under the hood — we
//! do not add a direct `gstreamer` crate dep, which keeps the
//! build tree small and lets the distro's gstreamer plugin stack
//! (`plugins-base`/`-good`/`-bad`/`-ugly`) drive codec coverage.
//!
//! # Space semantics
//!
//! The preview binary's close controller eats Space to close the
//! window (G2.8 Decision 4). For AV this means Space closes rather
//! than play/pause. We accept that as a v1 limitation and surface
//! the built-in `GtkVideo` transport controls (play/pause button,
//! seek bar) for all transport interactions. Revisit if dogfood
//! demands it — the fix would live in the preview binary's
//! keymap, not here.
//!
//! # Error handling
//!
//! `GtkMediaFile` is lazy: construction does not probe codecs. If
//! the stream is unsupported, the widget emits an error signal at
//! play time which GTK renders as a broken-file icon. We do NOT
//! install a custom error-bus handler here; that error path is
//! handled visually by the widget. Codec-missing on a fresh system
//! lands on the same path.

use std::path::Path;

use gtk::prelude::*;
use lixun_core::{Action, Hit};
use lixun_preview::{PreviewPlugin, PreviewPluginCfg, PreviewPluginEntry};

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "wav", "m4a", "aac", "opus", "wma",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "wmv", "m4v", "flv", "mpg", "mpeg", "3gp", "ts",
];

pub struct AvPreview;

impl PreviewPlugin for AvPreview {
    fn id(&self) -> &'static str {
        "av"
    }

    fn match_score(&self, hit: &Hit) -> u32 {
        let path = match &hit.action {
            Action::OpenFile { path } | Action::ShowInFileManager { path } => path,
            _ => return 0,
        };

        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let lower = ext.to_ascii_lowercase();
            if AUDIO_EXTENSIONS.iter().any(|&e| e == lower)
                || VIDEO_EXTENSIONS.iter().any(|&e| e == lower)
            {
                return 80;
            }
        }

        if hit
            .kind_label
            .as_deref()
            .is_some_and(|m| m.starts_with("audio/") || m.starts_with("video/"))
        {
            return 60;
        }

        0
    }

    fn build(&self, hit: &Hit, _cfg: &PreviewPluginCfg<'_>) -> anyhow::Result<gtk::Widget> {
        let path = match &hit.action {
            Action::OpenFile { path } | Action::ShowInFileManager { path } => path.clone(),
            _ => anyhow::bail!("av plugin: hit has no openable path"),
        };

        let media = gtk::MediaFile::for_filename(&path);
        media.set_loop(false);

        let video = gtk::Video::new();
        video.set_media_stream(Some(&media));
        video.set_autoplay(true);
        video.set_hexpand(true);
        video.set_vexpand(true);
        video.add_css_class("lixun-preview-av");

        let header = build_header(&path, is_audio(&path));

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
        vbox.append(&header);
        vbox.append(&video);

        tracing::info!(
            "av: rendered {:?} audio={} video={}",
            path,
            is_audio(&path),
            is_video(&path)
        );

        Ok(vbox.upcast())
    }
}

fn build_header(path: &Path, is_audio: bool) -> gtk::Box {
    let header = gtk::Box::new(gtk::Orientation::Vertical, 2);
    header.set_margin_top(12);
    header.set_margin_start(16);
    header.set_margin_end(16);

    let title = gtk::Label::new(Some(
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(unknown)"),
    ));
    title.set_xalign(0.0);
    title.add_css_class("lixun-preview-av-title");
    header.append(&title);

    let kind = gtk::Label::new(Some(if is_audio { "audio" } else { "video" }));
    kind.set_xalign(0.0);
    kind.add_css_class("lixun-preview-av-kind");
    header.append(&kind);

    header
}

fn is_audio(path: &Path) -> bool {
    ext_matches(path, AUDIO_EXTENSIONS)
}

fn is_video(path: &Path) -> bool {
    ext_matches(path, VIDEO_EXTENSIONS)
}

fn ext_matches(path: &Path, set: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| set.iter().any(|&s| s == e))
}

inventory::submit! {
    PreviewPluginEntry {
        factory: || Box::new(AvPreview),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lixun_core::{Category, DocId};
    use std::path::PathBuf;

    fn file_hit(path: impl Into<PathBuf>, kind: Option<&str>) -> Hit {
        let path = path.into();
        Hit {
            id: DocId(format!("fs:{}", path.display())),
            category: Category::File,
            title: path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            subtitle: path.display().to_string(),
            icon_name: None,
            kind_label: kind.map(str::to_string),
            score: 1.0,
            action: Action::OpenFile { path },
            extract_fail: false,
        }
    }

    #[test]
    fn mp3_scores_eighty() {
        let hit = file_hit("/tmp/tune.mp3", None);
        assert_eq!(AvPreview.match_score(&hit), 80);
    }

    #[test]
    fn mp4_scores_eighty() {
        let hit = file_hit("/tmp/clip.mp4", None);
        assert_eq!(AvPreview.match_score(&hit), 80);
    }

    #[test]
    fn mkv_uppercase_scores_eighty() {
        let hit = file_hit("/tmp/movie.MKV", None);
        assert_eq!(AvPreview.match_score(&hit), 80);
    }

    #[test]
    fn audio_mime_scores_sixty() {
        let hit = file_hit("/tmp/noext", Some("audio/mpeg"));
        assert_eq!(AvPreview.match_score(&hit), 60);
    }

    #[test]
    fn video_mime_scores_sixty() {
        let hit = file_hit("/tmp/noext", Some("video/webm"));
        assert_eq!(AvPreview.match_score(&hit), 60);
    }

    #[test]
    fn text_mime_scores_zero() {
        let hit = file_hit("/tmp/noext", Some("text/plain"));
        assert_eq!(AvPreview.match_score(&hit), 0);
    }

    #[test]
    fn txt_extension_scores_zero() {
        let hit = file_hit("/tmp/thing.txt", None);
        assert_eq!(AvPreview.match_score(&hit), 0);
    }

    #[test]
    fn launch_scores_zero() {
        let hit = Hit {
            id: DocId("app:firefox".into()),
            category: Category::App,
            title: "Firefox".into(),
            subtitle: String::new(),
            icon_name: None,
            kind_label: None,
            score: 1.0,
            action: Action::Launch {
                exec: "firefox".into(),
                terminal: false,
                desktop_id: None,
                desktop_file: None,
                working_dir: None,
            },
            extract_fail: false,
        };
        assert_eq!(AvPreview.match_score(&hit), 0);
    }

    #[test]
    fn is_audio_catches_flac() {
        assert!(is_audio(Path::new("/x/song.flac")));
        assert!(!is_video(Path::new("/x/song.flac")));
    }

    #[test]
    fn is_video_catches_webm() {
        assert!(is_video(Path::new("/x/clip.webm")));
        assert!(!is_audio(Path::new("/x/clip.webm")));
    }

    #[test]
    fn av_beats_code_for_m4v() {
        let hit = file_hit("/tmp/mix.m4v", None);
        assert_eq!(AvPreview.match_score(&hit), 80);
    }
}
