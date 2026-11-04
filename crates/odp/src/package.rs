//! The package: a zip (ODF 1.3 Part 2) read with size limits, or a flat single-file document.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::OdpError;

/// Largest single part kept.
const MAX_PART: u64 = 512 * 1024 * 1024;
/// Largest total kept, and largest total inflated (a zip bomb stops here).
const MAX_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
/// Largest XML part parsed.
pub const MAX_XML_PART: usize = 96 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;

pub struct Package {
    parts: HashMap<String, Vec<u8>>,
    /// Lower-case part names → names, for lookups whose case differs.
    lower: HashMap<String, String>,
    /// A flat document (`.fodp`): everything is in this one XML.
    pub flat: Option<Vec<u8>>,
}

fn is_presentation_mime(m: &str) -> bool {
    m.trim().starts_with("application/vnd.oasis.opendocument.presentation")
}

/// Is `bytes` a zip with a presentation `mimetype`, or a flat presentation document?
pub fn sniff(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"PK") {
        // The mimetype entry is first and stored, so its text sits at a fixed offset; read it from
        // the archive anyway (some writers deflate it or put it later).
        let Ok(mut z) = zip::ZipArchive::new(Cursor::new(bytes)) else { return false };
        if let Ok(mut f) = z.by_name("mimetype") {
            let mut s = String::new();
            if (&mut f).take(256).read_to_string(&mut s).is_ok() {
                return is_presentation_mime(&s);
            }
        }
        // No mimetype: a manifest naming a presentation root.
        let has_content = z.index_for_name("content.xml").is_some();
        if let Ok(mut f) = z.by_name("META-INF/manifest.xml") {
            let mut s = String::new();
            if (&mut f).take(1 << 20).read_to_string(&mut s).is_ok() {
                return s.contains("application/vnd.oasis.opendocument.presentation") && has_content;
            }
        }
        return false;
    }
    // Flat XML: the office namespace near the start, and a presentation media type or body.
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(64 * 1024)]);
    if !head.contains("urn:oasis:names:tc:opendocument:xmlns:office:1.0") || !head.contains(":document") {
        return false;
    }
    if head.contains("application/vnd.oasis.opendocument.presentation") {
        return true;
    }
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(32 * 1024 * 1024)]);
    body.contains(":presentation>") || body.contains(":presentation/>")
}

impl Package {
    pub fn open(bytes: &[u8]) -> Result<Self, OdpError> {
        if !bytes.starts_with(b"PK") {
            if bytes.len() > MAX_XML_PART {
                return Err(OdpError::NotOdp("flat document too large".into()));
            }
            return Ok(Package { parts: HashMap::new(), lower: HashMap::new(), flat: Some(bytes.to_vec()) });
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| OdpError::NotOdp(e.to_string()))?;
        if zip.len() > MAX_ENTRIES {
            return Err(OdpError::NotOdp(format!("too many entries ({})", zip.len())));
        }
        let mut parts = HashMap::new();
        let (mut total, mut work) = (0u64, 0u64);
        for i in 0..zip.len() {
            if work >= MAX_TOTAL {
                log::warn!("odp: decompression budget spent; skipping the remaining entries");
                break;
            }
            let Ok(mut f) = zip.by_index(i) else { continue };
            if f.is_dir() {
                continue;
            }
            let name = f.name().trim_start_matches('/').replace('\\', "/");
            if f.size() > MAX_PART {
                log::warn!("odp: skipping oversized part {name}");
                continue;
            }
            let cap = MAX_PART.min(MAX_TOTAL.saturating_sub(total)).min(MAX_TOTAL.saturating_sub(work));
            let mut data = Vec::with_capacity(f.size().min(16 * 1024 * 1024) as usize);
            let read = (&mut f).take(cap.saturating_add(1)).read_to_end(&mut data);
            work = work.saturating_add(data.len() as u64);
            match read {
                Ok(_) if data.len() as u64 > cap => {
                    log::warn!("odp: part {name} exceeds size limits; skipped");
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("odp: could not read {name}: {e}");
                    continue;
                }
            }
            total = total.saturating_add(data.len() as u64);
            parts.insert(name, data);
        }
        let lower = parts.keys().map(|k| (k.to_ascii_lowercase(), k.clone())).collect();
        Ok(Package { parts, lower, flat: None })
    }

    /// A part by its path in the package (`Pictures/a.png`, `./Pictures/a.png`). Lookup falls back
    /// to a case-insensitive match, as some writers disagree with their own manifest.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.part(name).map(|(_, d)| d)
    }

    /// A part and its name as the package spells it: every way of naming one part finds the same
    /// name.
    pub fn part(&self, name: &str) -> Option<(&str, &[u8])> {
        let n = name.trim_start_matches("./").trim_start_matches('/');
        if let Some((k, d)) = self.parts.get_key_value(n) {
            return Some((k.as_str(), d.as_slice()));
        }
        let k = self.lower.get(&n.to_ascii_lowercase())?;
        self.parts.get_key_value(k).map(|(k, d)| (k.as_str(), d.as_slice()))
    }

    pub fn xml(&self, name: &str) -> Option<crate::dom::El> {
        let b = self.get(name)?;
        if b.len() > MAX_XML_PART {
            log::warn!("odp: XML part {name} too large");
            return None;
        }
        match crate::dom::parse(b) {
            Ok(d) => Some(d),
            Err(e) => {
                log::warn!("odp: {name}: {e}");
                None
            }
        }
    }
}

pub fn sniff_image(d: &[u8]) -> (&'static str, &'static str) {
    if d.starts_with(b"\x89PNG") {
        ("image/png", "png")
    } else if d.starts_with(&[0xFF, 0xD8]) {
        ("image/jpeg", "jpg")
    } else if d.starts_with(b"GIF8") {
        ("image/gif", "gif")
    } else if d.starts_with(b"<svg") || d.starts_with(b"<?xml") {
        ("image/svg+xml", "svg")
    } else {
        ("application/octet-stream", "bin")
    }
}

/// Standard base64 (white space ignored), bounded.
pub fn base64(s: &str) -> Option<Vec<u8>> {
    if s.len() > 512 * 1024 * 1024 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("aGVs\nbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64("*"), None);
    }
}
