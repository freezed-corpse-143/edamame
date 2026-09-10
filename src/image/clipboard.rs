//! Paste images from the OS clipboard.
//!
//! Two clipboard payloads map to two behaviors:
//! - a screenshot arrives as a *bitmap* ([`read_clipboard_image`]); it is
//!   encoded to PNG and saved into the configured image directory;
//! - a copied image-file *path* arrives as text ([`read_clipboard_path`]);
//!   it is normalized and referenced directly, without reading or copying
//!   the file.
//!
//! The save directory is resolved from `EDAMAME_IMAGES_DIR` (highest),
//! then `ImagesConfig::save_dir`, relative to the open document.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Environment variable that overrides the configured image save directory.
pub const IMAGES_DIR_ENV: &str = "EDAMAME_IMAGES_DIR";

/// Extensions edamame's image loader accepts, kept in step with the `image`
/// crate features in `Cargo.toml` plus SVG.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg"];

/// Raw RGBA pixels read off the clipboard.
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// The clipboard bitmap, or `Err` when no bitmap is present.
#[cfg(feature = "clipboard")]
pub fn read_clipboard_image() -> Result<RawImage, String> {
    // Tests must not touch the real OS clipboard (it races parallel tests).
    if cfg!(test) {
        return Err("clipboard unavailable in tests".to_owned());
    }
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    let img = cb
        .get_image()
        .map_err(|e| format!("no image on the clipboard: {e}"))?;
    Ok(RawImage {
        width: img.width as u32,
        height: img.height as u32,
        rgba: img.bytes.into_owned(),
    })
}

#[cfg(not(feature = "clipboard"))]
pub fn read_clipboard_image() -> Result<RawImage, String> {
    Err("clipboard support is disabled in this build".to_owned())
}

/// The clipboard text, if any — the "copied image-file path" case.
#[cfg(feature = "clipboard")]
pub fn read_clipboard_path() -> Option<String> {
    let mut cb = arboard::Clipboard::new().ok()?;
    cb.get_text().ok()
}

#[cfg(not(feature = "clipboard"))]
pub fn read_clipboard_path() -> Option<String> {
    None
}

/// The OS clipboard's text, if any — without the in-process kill-ring
/// fallback.  `None` when the clipboard is unreachable or holds no text.
#[cfg(feature = "clipboard")]
pub fn os_clipboard_text() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    arboard::Clipboard::new().ok()?.get_text().ok()
}

#[cfg(not(feature = "clipboard"))]
pub fn os_clipboard_text() -> Option<String> {
    None
}

/// The image save directory from the environment, if set and non-empty.
pub fn images_dir_from_env() -> Option<String> {
    std::env::var(IMAGES_DIR_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Normalize a copied path into a Markdown-safe form, returning `None`
/// when it does not look like an image file.
///
/// Handles the Windows "Copy as path" surrounding quotes, `file://` URIs
/// (including the `file:///C:/…` drive form), and backslash separators.
/// Percent-decoding of `file://` URIs is intentionally not done — the
/// dominant source here is Windows "Copy as path", which emits raw paths.
pub fn normalize_image_path(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_owned();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].to_owned();
    }
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.to_owned();
        // `file:///C:/…` → `C:/…`: strip the leading slash before a drive.
        let bytes = s.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
            s = s[1..].to_owned();
        }
    }
    s = s.replace('\\', "/");
    if !is_image_ext(&s) {
        return None;
    }
    Some(s)
}

fn is_image_ext(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Encode RGBA pixels to PNG bytes.
fn encode_png(raw: &RawImage) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(raw.width, raw.height, raw.rgba.clone())
        .ok_or_else(|| "invalid RGBA buffer".to_owned())?;
    let dynamic = image::DynamicImage::ImageRgba8(img);
    let mut buf = Vec::new();
    dynamic
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(buf)
}

/// Resolve a save-directory string to a concrete directory.
///
/// Absolute paths pass through; relative paths (a leading `./` is
/// dropped) resolve against the open document's parent.  An unsaved
/// buffer has no parent, so relative resolution fails.
pub fn resolve_save_dir(dir: &str, doc_path: Option<&Path>) -> Result<PathBuf, String> {
    let dir = dir.strip_prefix("./").unwrap_or(dir);
    let dir = Path::new(dir);
    if dir.is_absolute() {
        return Ok(dir.to_path_buf());
    }
    let parent = doc_path.and_then(|p| p.parent()).ok_or_else(|| {
        "save the file first — there is no document path to resolve the image directory against"
            .to_owned()
    })?;
    Ok(parent.join(dir))
}

/// A non-colliding `image-<millis>.png` path inside `dir`.
fn unique_image_path(dir: &Path) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut path = dir.join(format!("image-{millis}.png"));
    let mut n = 1u32;
    while path.exists() {
        path = dir.join(format!("image-{millis}-{n}.png"));
        n += 1;
    }
    path
}

/// Save `raw` into `dir` and return the Markdown link path to reference
/// it — always the absolute path, forward-slash separated, so the link
/// stays valid regardless of the terminal's working directory.
pub fn save_image(raw: &RawImage, dir: &str, doc_path: Option<&Path>) -> Result<String, String> {
    let dir = resolve_save_dir(dir, doc_path)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create image directory {}: {e}", dir.display()))?;
    let path = unique_image_path(&dir);
    let bytes = encode_png(raw)?;
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let abs = std::fs::canonicalize(&path)
        .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
    Ok(to_forward_slash(&abs))
}

/// Absolute path as a forward-slash string, dropping the `\\?\` verbatim
/// prefix `std::fs::canonicalize` can emit on Windows.
fn to_forward_slash(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").unwrap_or(&s).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_quotes_file_uri_and_backslashes() {
        assert_eq!(
            normalize_image_path("\"C:\\Users\\me\\pic.PNG\"").as_deref(),
            Some("C:/Users/me/pic.PNG")
        );
        assert_eq!(
            normalize_image_path("file:///C:/Users/me/pic.png").as_deref(),
            Some("C:/Users/me/pic.png")
        );
        assert_eq!(
            normalize_image_path("file:///home/me/pic.jpg").as_deref(),
            Some("/home/me/pic.jpg")
        );
    }

    #[test]
    fn normalize_rejects_non_image_paths() {
        assert_eq!(normalize_image_path("/tmp/notes.txt"), None);
        assert_eq!(normalize_image_path("just some text"), None);
    }

    #[test]
    fn resolve_absolute_relative_and_unsaved() {
        let abs_input = if cfg!(windows) { "C:\\img" } else { "/tmp/img" };
        let abs = resolve_save_dir(abs_input, None).unwrap();
        assert!(abs.is_absolute());

        let doc = Path::new("docs").join("guide.md");
        let rel = resolve_save_dir("./images", Some(&doc)).unwrap();
        assert_eq!(rel, Path::new("docs").join("images"));

        assert!(resolve_save_dir("./images", None).is_err());
    }

    #[test]
    fn png_roundtrip() {
        let raw = RawImage {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        };
        let bytes = encode_png(&raw).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 1);
    }
}
