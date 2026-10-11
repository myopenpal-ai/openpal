//! Turn a file the user picked into a chat [`Attachment`].
//!
//! An image becomes a `data:` URI (rendered as a thumbnail and sent to vision
//! models as an `image_url` content part); a document (PDF or any text/code
//! file) becomes extracted text that the engine folds into the prompt. This is
//! pure, GUI-free logic — the Tauri layer only opens the native picker and
//! hands us the chosen paths.
//!
//! Pasted images never reach here: the webview reads its own clipboard image
//! straight to a `data:` URI. This module is only for picked files, where Rust
//! has the path and can also pull text out of PDFs.

use crate::types::{Attachment, AttachmentKind};
use base64::Engine as _;
use std::path::Path;

/// Largest image we embed. Past this the base64 payload (and the stored
/// conversation) balloon for little benefit — vision models downscale
/// internally anyway.
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest amount of extracted document text we keep, so one huge file can't
/// blow up memory or the prompt. Anything longer is truncated with a marker.
const MAX_DOC_CHARS: usize = 100_000;

/// Image file extensions we accept (and can map to a MIME type).
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Build an [`Attachment`] from a picked file. `Err` carries a short, user-safe
/// sentence explaining why this one file couldn't be added (the caller shows it
/// and skips the file).
pub fn from_path(path: &Path) -> Result<Attachment, String> {
    let name = file_label(path);
    let ext = extension(path);

    if is_image_ext(&ext) {
        let data_uri = read_image_data_uri(path, &ext)?;
        Ok(Attachment {
            kind: AttachmentKind::Image,
            name,
            data_uri: Some(data_uri),
            text: None,
        })
    } else {
        let text = extract_document_text(path, &ext)?;
        Ok(Attachment {
            kind: AttachmentKind::Document,
            name,
            data_uri: None,
            text: Some(text),
        })
    }
}

/// True for an extension the picker should offer (and accept) as an image.
pub fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

fn read_image_data_uri(path: &Path, ext: &str) -> Result<String, String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > MAX_IMAGE_BYTES {
        return Err(format!(
            "“{}” is too large to attach. Try an image under {} MB.",
            file_label(path),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| {
        tracing::warn!("reading image {path:?} failed: {e}");
        "That image couldn't be read. Please try another file.".to_string()
    })?;
    let mime = image_mime(ext, &bytes);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

fn extract_document_text(path: &Path, ext: &str) -> Result<String, String> {
    let raw = if ext == "pdf" {
        pdf_extract::extract_text(path).map_err(|e| {
            tracing::warn!("extracting PDF text from {path:?} failed: {e}");
            "That PDF couldn't be read. If it's a scan or only images, there's no \
             text to pull out."
                .to_string()
        })?
    } else {
        // Everything else is treated as text/code: read as UTF-8, lossily, so a
        // stray byte doesn't reject an otherwise-fine file.
        let bytes = std::fs::read(path).map_err(|e| {
            tracing::warn!("reading document {path:?} failed: {e}");
            "That file couldn't be read. Please try another one.".to_string()
        })?;
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("That file didn't have any text to read.".to_string());
    }
    Ok(truncate_chars(trimmed, MAX_DOC_CHARS))
}

/// MIME type for an accepted image — magic bytes first (reliable), then the
/// extension as a fallback.
fn image_mime(ext: &str, bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    match ext {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg", // jpg/jpeg and anything unrecognized
    }
}

/// Keep at most `max` characters, appending a clear marker when truncated.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("\n\n…[truncated]");
    out
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string())
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extensions_are_recognized() {
        assert!(is_image_ext("PNG"));
        assert!(is_image_ext("jpeg"));
        assert!(!is_image_ext("pdf"));
        assert!(!is_image_ext("txt"));
        assert!(!is_image_ext(""));
    }

    #[test]
    fn mime_trusts_magic_then_extension() {
        assert_eq!(image_mime("jpg", b"\x89PNG\r\n\x1a\n...."), "image/png");
        assert_eq!(image_mime("png", b"not-magic"), "image/png");
        assert_eq!(image_mime("jpeg", b"not-magic"), "image/jpeg");
        assert_eq!(image_mime("webp", b"not-magic"), "image/webp");
    }

    #[test]
    fn truncation_marks_long_text() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        let long = "x".repeat(50);
        let cut = truncate_chars(&long, 10);
        assert!(cut.starts_with(&"x".repeat(10)));
        assert!(cut.ends_with("[truncated]"));
    }

    #[test]
    fn text_file_becomes_a_document_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"Hello from a text file.").unwrap();
        let att = from_path(&path).unwrap();
        assert_eq!(att.kind, AttachmentKind::Document);
        assert_eq!(att.name, "notes.txt");
        assert_eq!(att.text.as_deref(), Some("Hello from a text file."));
        assert!(att.data_uri.is_none());
    }

    #[test]
    fn png_file_becomes_an_image_data_uri() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixel.png");
        // A 1x1 PNG is unnecessary — only the magic header drives the MIME.
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nrest-of-file").unwrap();
        let att = from_path(&path).unwrap();
        assert_eq!(att.kind, AttachmentKind::Image);
        assert!(att
            .data_uri
            .as_deref()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(att.text.is_none());
    }

    #[test]
    fn empty_document_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blank.txt");
        std::fs::write(&path, b"   \n  ").unwrap();
        assert!(from_path(&path).is_err());
    }
}
