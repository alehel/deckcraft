//! Two-way vocabularies: each model value and the ODF attribute value it is, in one table read
//! in both directions. Reading takes the first row with the file's value; writing the first row
//! with ours.

use deckcraft_model::style::{LineCap, LineJoin};
use deckcraft_model::text::{Align, Anchor, TextDir};

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

/// `fo:text-align` (`left`/`right` are older names of `start`/`end`; ODF has one justification).
pub const ALIGNS: &[(Align, &str)] = &[
    (Align::Left, "start"),
    (Align::Left, "left"),
    (Align::Right, "end"),
    (Align::Right, "right"),
    (Align::Center, "center"),
    (Align::Justify, "justify"),
    (Align::JustLow, "justify"),
    (Align::Distributed, "justify"),
    (Align::ThaiDistributed, "justify"),
];

/// Our underline and `style:text-underline-style`, `-type` and `-width`.
pub const UNDERLINES: &[(&str, &str, &str, &str)] = &[
    ("none", "none", "single", "auto"),
    ("sng", "solid", "single", "auto"),
    ("dbl", "solid", "double", "auto"),
    ("heavy", "solid", "single", "bold"),
    ("dotted", "dotted", "single", "auto"),
    ("dottedHeavy", "dotted", "single", "bold"),
    ("dash", "dash", "single", "auto"),
    ("dashHeavy", "dash", "single", "bold"),
    ("dashLong", "long-dash", "single", "auto"),
    ("dashLongHeavy", "long-dash", "single", "bold"),
    ("dotDash", "dot-dash", "single", "auto"),
    ("dotDashHeavy", "dot-dash", "single", "bold"),
    ("dotDotDash", "dot-dot-dash", "single", "auto"),
    ("dotDotDashHeavy", "dot-dot-dash", "single", "bold"),
    ("wavy", "wave", "single", "auto"),
    ("wavyHeavy", "wave", "single", "bold"),
    ("wavyDbl", "wave", "double", "auto"),
];

/// Our underline for a file's style, type and width: the exact row, else one of the same style
/// and type, else of the style, else a single line.
pub fn underline_of(style: &str, ty: &str, width: &str) -> &'static str {
    let rows = || UNDERLINES.iter();
    rows()
        .find(|u| (u.1, u.2, u.3) == (style, ty, width))
        .or_else(|| rows().find(|u| (u.1, u.2) == (style, ty)))
        .or_else(|| rows().find(|u| u.1 == style))
        .map_or("sng", |u| u.0)
}

/// A file's underline style, type and width for ours.
pub fn odf_underline(ours: &str) -> (&'static str, &'static str, &'static str) {
    let u = UNDERLINES.iter().find(|u| u.0 == ours).or(UNDERLINES.get(1));
    u.map_or(("solid", "single", "auto"), |u| (u.1, u.2, u.3))
}

/// Our chart legend position and `chart:legend-position`.
pub const LEGEND: &[(&str, &str)] = &[("r", "end"), ("l", "start"), ("t", "top"), ("b", "bottom"), ("tr", "top-end")];

/// Show-navigation link targets as LibreOffice writes them (`#action?jump=…`).
pub const JUMPS: &[(deckcraft_model::text::Action, &str)] = &[
    (deckcraft_model::text::Action::NextSlide, "#action?jump=nextslide"),
    (deckcraft_model::text::Action::PreviousSlide, "#action?jump=previousslide"),
    (deckcraft_model::text::Action::FirstSlide, "#action?jump=firstslide"),
    (deckcraft_model::text::Action::LastSlide, "#action?jump=lastslide"),
    (deckcraft_model::text::Action::EndShow, "#action?jump=endshow"),
];

/// `loext:writing-mode`: LibreOffice's text rotated a quarter turn either way.
pub const LOEXT_WRITING_MODES: &[(TextDir, &str)] = &[(TextDir::Vertical, "tb-rl90"), (TextDir::Vertical270, "bt-lr")];

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
pub fn read<T: Clone>(table: &[(T, &str)], odf: &str) -> Option<T> {
    table.iter().find(|r| r.1 == odf).map(|r| r.0.clone())
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
        round_trips(ALIGNS, &[Align::JustLow, Align::Distributed, Align::ThaiDistributed]);
        round_trips(LEGEND, &[]);
        for (ours, ..) in UNDERLINES {
            let (s, t, w) = odf_underline(ours);
            assert_eq!(underline_of(s, t, w), *ours);
        }
        for (ext, mime) in MEDIA_TYPES {
            assert_eq!(mime_for(&format!("a.{ext}")), *mime);
            assert_eq!(mime_for(&format!("a.{}", ext_for(mime))), *mime);
        }
    }
}
