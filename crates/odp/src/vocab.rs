//! Two-way vocabularies: each model value and the ODF attribute value it is, in one table read
//! in both directions. Reading takes the first row with the file's value; writing the first row
//! with ours.

use deckcraft_model::style::{LineCap, LineJoin};
use deckcraft_model::text::{Anchor, TextDir};

pub const LINE_JOINS: &[(LineJoin, &str)] = &[(LineJoin::Miter, "miter"), (LineJoin::Bevel, "bevel"), (LineJoin::Round, "round")];

pub const LINE_CAPS: &[(LineCap, &str)] = &[(LineCap::Flat, "butt"), (LineCap::Round, "round"), (LineCap::Square, "square")];

/// `draw:textarea-vertical-align` (ODF has one justification).
pub const ANCHORS: &[(Anchor, &str)] =
    &[(Anchor::Top, "top"), (Anchor::Middle, "middle"), (Anchor::Bottom, "bottom"), (Anchor::Justified, "justify"), (Anchor::Distributed, "justify")];

/// `style:writing-mode` of vertical text (`tb` is the older name of `tb-rl`).
pub const WRITING_MODES: &[(TextDir, &str)] = &[
    (TextDir::EaVertical, "tb-rl"),
    (TextDir::EaVertical, "tb"),
    (TextDir::Vertical, "tb-rl"),
    (TextDir::MongolianVertical, "tb-rl"),
    (TextDir::Vertical270, "tb-lr"),
];

/// Our tab stop alignment and `style:type`.
pub const TAB_TYPES: &[(&str, &str)] = &[("l", "left"), ("r", "right"), ("ctr", "center"), ("dec", "char")];

/// File extensions and media types of pictures, audio and video; the first extension of a type
/// is the one written.
const MEDIA_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("bmp", "image/bmp"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("svg", "image/svg+xml"),
    ("webp", "image/webp"),
    ("wmf", "image/x-wmf"),
    ("emf", "image/x-emf"),
    ("svm", "image/x-svm"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("mkv", "video/x-matroska"),
    ("avi", "video/x-msvideo"),
    ("wmv", "video/x-ms-wmv"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("flac", "audio/flac"),
    ("wma", "audio/x-ms-wma"),
];

/// The media type of a file name's extension.
pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    MEDIA_TYPES.iter().find(|m| m.0 == ext).map_or("application/octet-stream", |m| m.1)
}

/// The extension a media type is written with.
pub fn ext_for(mime: &str) -> &'static str {
    MEDIA_TYPES.iter().find(|m| m.1 == mime).map_or("bin", |m| m.0)
}

/// Our value for a file's.
pub fn read<T: Copy>(table: &[(T, &str)], odf: &str) -> Option<T> {
    table.iter().find(|r| r.1 == odf).map(|r| r.0)
}

/// The file's value for ours.
pub fn write<T: PartialEq>(table: &[(T, &'static str)], ours: &T) -> Option<&'static str> {
    table.iter().find(|r| r.0 == *ours).map(|r| r.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trips<T: Copy + PartialEq + std::fmt::Debug>(table: &[(T, &'static str)], aliases: &[T]) {
        for (ours, _) in table {
            let back = write(table, ours).and_then(|v| read(table, v));
            assert!(back == Some(*ours) || aliases.contains(ours), "{ours:?}");
        }
    }

    #[test]
    fn vocabularies_read_back_what_they_write() {
        round_trips(LINE_JOINS, &[]);
        round_trips(LINE_CAPS, &[]);
        round_trips(ANCHORS, &[Anchor::Distributed]);
        round_trips(WRITING_MODES, &[TextDir::Vertical, TextDir::MongolianVertical]);
        round_trips(TAB_TYPES, &[]);
        for (ext, mime) in MEDIA_TYPES {
            assert_eq!(mime_for(&format!("a.{ext}")), *mime);
            assert_eq!(mime_for(&format!("a.{}", ext_for(mime))), *mime);
        }
    }
}
