//! OpenDocument presentations (`.odp`, `.otp`, flat `.fodp`): import and export, written from the
//! public OASIS OpenDocument 1.3 standard (ISO/IEC 26300).
//!
//! [`import`] reads a package leniently: unknown elements are skipped, every size and depth is
//! bounded, and styles are resolved through their parent chains into explicit model formatting, so
//! a slide looks the way its file describes it. [`export`] writes a package LibreOffice Impress
//! opens without repair.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod anim;
mod chart;
mod dom;
mod geometry;
mod hatch;
mod package;
mod read;
mod styles;
mod text;
mod units;
mod write;

use deckcraft_model::Presentation;

/// A shape's connection sites for LibreOffice's glue points 0–3 (top, right, bottom, left): ours
/// run top, left, bottom, right.
pub(crate) const GLUE_POINTS: [u32; 4] = [0, 3, 2, 1];

/// Main-document media types this crate reads and writes.
pub const MIME_PRESENTATION: &str = "application/vnd.oasis.opendocument.presentation";
pub const MIME_TEMPLATE: &str = "application/vnd.oasis.opendocument.presentation-template";

#[derive(Debug, thiserror::Error)]
pub enum OdpError {
    #[error("not an OpenDocument presentation: {0}")]
    NotOdp(String),
    #[error("malformed XML: {0}")]
    Xml(String),
    #[error("write failed: {0}")]
    Write(String),
}

/// Does this look like an OpenDocument presentation: a zip whose `mimetype` (or manifest) names a
/// presentation, or a flat XML document (`.fodp`) with an `office:presentation` body?
pub fn sniff(bytes: &[u8]) -> bool {
    package::sniff(bytes)
}

/// Read an `.odp` / `.otp` package or a flat `.fodp` document.
pub fn import(bytes: &[u8]) -> Result<Presentation, OdpError> {
    read::import(bytes)
}

/// Write an `.odp` package.
pub fn export(p: &Presentation) -> Result<Vec<u8>, OdpError> {
    write::export(p, MIME_PRESENTATION)
}

/// Write a package for `ext`: `otp` (template) differs from `odp` only in its media type.
pub fn export_as(p: &Presentation, ext: &str) -> Result<Vec<u8>, OdpError> {
    write::export(p, if ext.eq_ignore_ascii_case("otp") { MIME_TEMPLATE } else { MIME_PRESENTATION })
}
