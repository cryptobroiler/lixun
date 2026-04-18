use mime_guess::Mime;

pub fn mime_to_icon_name(mime: &Mime) -> String {
    match mime.essence_str() {
        "application/pdf" => "application-pdf".into(),
        "text/plain" => "text-plain".into(),
        "text/markdown" | "text/x-markdown" => "text-x-markdown".into(),
        "application/zip" | "application/x-tar" | "application/x-gzip" => {
            "package-x-generic".into()
        }
        "inode/directory" => "folder".into(),
        _ if mime.type_() == mime_guess::mime::IMAGE => "image-x-generic".into(),
        _ if mime.type_() == mime_guess::mime::VIDEO => "video-x-generic".into(),
        _ if mime.type_() == mime_guess::mime::AUDIO => "audio-x-generic".into(),
        _ => "text-x-generic".into(),
    }
}

pub fn human_kind(mime: &Mime) -> String {
    match mime.essence_str() {
        "application/pdf" => "PDF Document".into(),
        "text/plain" => "Text".into(),
        "text/markdown" | "text/x-markdown" => "Markdown".into(),
        "application/json" => "JSON".into(),
        "application/zip" | "application/x-tar" | "application/x-gzip" => "Archive".into(),
        "inode/directory" => "Folder".into(),
        _ if mime.type_() == mime_guess::mime::IMAGE => "Image".into(),
        _ if mime.type_() == mime_guess::mime::VIDEO => "Video".into(),
        _ if mime.type_() == mime_guess::mime::AUDIO => "Audio".into(),
        _ => titleize_subtype(mime.subtype().as_str()),
    }
}

fn titleize_subtype(subtype: &str) -> String {
    let mut words = Vec::new();
    let mut current = String::new();

    for ch in subtype.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(capitalize(&current));
            current.clear();
        }
    }

    if !current.is_empty() {
        words.push(capitalize(&current));
    }

    if words.is_empty() {
        "Unknown".into()
    } else {
        words.join(" ")
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut out = String::new();
    out.extend(first.to_uppercase());
    out.push_str(chars.as_str());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mime(input: &str) -> Mime {
        input.parse().unwrap()
    }

    #[test]
    fn test_mime_to_icon_name_examples() {
        assert_eq!(
            mime_to_icon_name(&mime("application/pdf")),
            "application-pdf"
        );
        assert_eq!(mime_to_icon_name(&mime("text/plain")), "text-plain");
        assert_eq!(mime_to_icon_name(&mime("text/markdown")), "text-x-markdown");
        assert_eq!(
            mime_to_icon_name(&mime("text/x-markdown")),
            "text-x-markdown"
        );
        assert_eq!(mime_to_icon_name(&mime("image/png")), "image-x-generic");
        assert_eq!(mime_to_icon_name(&mime("image/jpeg")), "image-x-generic");
        assert_eq!(mime_to_icon_name(&mime("video/mp4")), "video-x-generic");
        assert_eq!(mime_to_icon_name(&mime("audio/mpeg")), "audio-x-generic");
        assert_eq!(
            mime_to_icon_name(&mime("application/zip")),
            "package-x-generic"
        );
        assert_eq!(
            mime_to_icon_name(&mime("application/x-tar")),
            "package-x-generic"
        );
        assert_eq!(
            mime_to_icon_name(&mime("application/x-gzip")),
            "package-x-generic"
        );
        assert_eq!(mime_to_icon_name(&mime("inode/directory")), "folder");
        assert_eq!(
            mime_to_icon_name(&mime("application/octet-stream")),
            "text-x-generic"
        );
    }

    #[test]
    fn test_human_kind_examples() {
        assert_eq!(human_kind(&mime("application/pdf")), "PDF Document");
        assert_eq!(human_kind(&mime("text/plain")), "Text");
        assert_eq!(human_kind(&mime("text/markdown")), "Markdown");
        assert_eq!(human_kind(&mime("text/x-markdown")), "Markdown");
        assert_eq!(human_kind(&mime("image/png")), "Image");
        assert_eq!(human_kind(&mime("video/mp4")), "Video");
        assert_eq!(human_kind(&mime("audio/mpeg")), "Audio");
        assert_eq!(human_kind(&mime("application/json")), "JSON");
        assert_eq!(human_kind(&mime("application/zip")), "Archive");
        assert_eq!(human_kind(&mime("application/x-tar")), "Archive");
        assert_eq!(human_kind(&mime("inode/directory")), "Folder");
        assert_eq!(human_kind(&mime("text/html")), "Html");
        assert_eq!(human_kind(&mime("application/xml")), "Xml");
    }
}
