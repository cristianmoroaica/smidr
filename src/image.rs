//! File attachment handling — drag-and-drop / pasted path detection.
//! Supports images (png, jpg, etc.) and PDFs.

use std::path::{Path, PathBuf};

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tiff"];
const DOC_EXTENSIONS: &[&str] = &["pdf"];

/// All supported attachment extensions.
fn is_attachment_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_lowercase();
            IMAGE_EXTENSIONS.contains(&lower.as_str()) || DOC_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Whether `path` has an image extension understood by the attachment
/// pipeline. This is public so the web upload boundary can reject arbitrary
/// files before they are persisted.
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// MIME type used by OpenAI-compatible multimodal requests.
pub fn image_mime_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        _ => None,
    }
}

/// Check if a path is a PDF.
pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase() == "pdf")
        .unwrap_or(false)
}

/// Extract file attachment paths from user input text.
/// Handles:
///   - Quoted paths: "path with spaces.pdf"
///   - Single-quoted paths: 'path with spaces.pdf'
///   - Escaped spaces: path\ with\ spaces.pdf
///   - Simple paths: /home/user/file.png
///   - ~ expansion: ~/docs/file.pdf
///
/// Returns (cleaned text without paths, list of attachment paths found).
pub fn extract_attachment_paths(input: &str) -> (String, Vec<PathBuf>) {
    let mut attachments = Vec::new();
    let mut text_parts = Vec::new();
    let mut chars = input.chars().peekable();
    let mut current_token = String::new();

    while let Some(&ch) = chars.peek() {
        match ch {
            // Quoted path: "..." or '...'
            '"' | '\'' => {
                let quote = ch;
                chars.next(); // consume opening quote
                let mut path_str = String::new();
                while let Some(&c) = chars.peek() {
                    if c == quote { chars.next(); break; }
                    path_str.push(c);
                    chars.next();
                }
                let expanded = expand_path(&path_str);
                if Path::new(&expanded).exists() && is_attachment_path(Path::new(&expanded)) {
                    // Flush any pending text
                    if !current_token.is_empty() {
                        text_parts.push(std::mem::take(&mut current_token));
                    }
                    attachments.push(PathBuf::from(expanded));
                } else {
                    current_token.push_str(&path_str);
                }
            }
            // Whitespace: end of token
            ' ' | '\t' => {
                chars.next();
                if !current_token.is_empty() {
                    let expanded = expand_path(&current_token);
                    if Path::new(&expanded).exists() && is_attachment_path(Path::new(&expanded)) {
                        attachments.push(PathBuf::from(expanded));
                    } else {
                        text_parts.push(std::mem::take(&mut current_token));
                    }
                    current_token.clear();
                }
            }
            // Escaped space: \ followed by space
            '\\' => {
                chars.next();
                if let Some(&next) = chars.peek() {
                    if next == ' ' {
                        current_token.push(' ');
                        chars.next();
                    } else {
                        current_token.push('\\');
                    }
                }
            }
            _ => {
                current_token.push(ch);
                chars.next();
            }
        }
    }

    // Handle last token
    if !current_token.is_empty() {
        let expanded = expand_path(&current_token);
        if Path::new(&expanded).exists() && is_attachment_path(Path::new(&expanded)) {
            attachments.push(PathBuf::from(expanded));
        } else {
            text_parts.push(current_token);
        }
    }

    (text_parts.join(" "), attachments)
}

/// Expand ~ to home directory.
fn expand_path(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

/// Describe an attachment for the Claude prompt.
pub fn describe_attachment(path: &Path) -> String {
    if is_pdf(path) {
        format!("PDF document: {}", path.to_string_lossy())
    } else {
        format!("Image: {}", path.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_attachment_path() {
        assert!(is_attachment_path(Path::new("photo.png")));
        assert!(is_attachment_path(Path::new("photo.JPG")));
        assert!(is_attachment_path(Path::new("document.pdf")));
        assert!(is_attachment_path(Path::new("document.PDF")));
        assert!(!is_attachment_path(Path::new("model.stl")));
        assert!(!is_attachment_path(Path::new("code.py")));
    }

    #[test]
    fn test_extract_no_attachments() {
        let (text, files) = extract_attachment_paths("make a 10mm cube");
        assert_eq!(text, "make a 10mm cube");
        assert!(files.is_empty());
    }

    #[test]
    fn test_is_pdf() {
        assert!(is_pdf(Path::new("datasheet.pdf")));
        assert!(is_pdf(Path::new("SPECS.PDF")));
        assert!(!is_pdf(Path::new("photo.png")));
    }

    #[test]
    fn test_describe_attachment() {
        assert!(describe_attachment(Path::new("foo.pdf")).starts_with("PDF document:"));
        assert!(describe_attachment(Path::new("bar.png")).starts_with("Image:"));
    }
}
