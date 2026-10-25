//! Images from the clipboard → PNG files in a terminal's scratch folder (GPUI-free).
//!
//! A program in a terminal cannot be given pixels, only text; coding agents (Claude Code, Codex,
//! OMP) take the *path* of an image typed or pasted into their prompt. So ⌘V of an image saves
//! it here and the view pastes the path (`docs/design.md` § Terminal drops and pastes).

use std::io::{Cursor, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use image::{ImageFormat, Limits};

/// The largest image taken: the bytes on the clipboard and the PNG written (20 MiB).
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// The most one scratch folder holds (a session's pasted images together).
pub const MAX_SCRATCH_BYTES: u64 = 200 * 1024 * 1024;
/// Decoding a TIFF or WebP may not allocate more than this, whatever its header claims.
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;
/// Width and height a clipboard image may have to be converted.
const MAX_DIMENSION: u32 = 16_384;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const PREFIX: &str = "paste-";
const SUFFIX: &str = ".png";

/// Why an image was not saved. `Display` is the sentence the user reads.
#[derive(Debug, thiserror::Error)]
pub enum ImagePasteError {
    #[error("The image is {size}: at most {limit} can be pasted into a terminal", size = human(*.size as u64), limit = human(MAX_IMAGE_BYTES as u64))]
    TooLarge { size: usize },
    #[error("A {0} image can't be pasted into a terminal: PNG, JPEG, GIF, WebP, TIFF and BMP only")]
    Unsupported(String),
    #[error("The image on the clipboard could not be read")]
    Unreadable,
    #[error(
        "This terminal's pasted images already take {used} (at most {limit}); they go when the session is closed",
        used = human(*.used),
        limit = human(MAX_SCRATCH_BYTES)
    )]
    ScratchFull { used: u64 },
    #[error("Could not save the image: {0}")]
    Io(#[from] std::io::Error),
}

/// A coding agent that takes the path of an image typed or pasted into its prompt, and what the
/// terminal is told of it by the owner (`docs/integrations.md` § Pasting images).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteAgent {
    ClaudeCode,
    Codex,
    Omp,
}

impl PasteAgent {
    /// The agent a session's report names (`SessionInfo.agent.agent`: `claude`, `codex`, `omp`);
    /// `None` for any other.
    pub fn from_report(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "omp" => Some(Self::Omp),
            _ => None,
        }
    }
}

/// Where a terminal's pasted images go and who reads the prompt they are pasted into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePasteTarget {
    /// The session's scratch folder (`$HUTAN_HOME/scratch/<session>`).
    pub dir: PathBuf,
    /// The agent the session runs, when it said so.
    pub agent: Option<PasteAgent>,
}

/// `path` for a prompt of `agent`: backslash-escaped where a shell would care, which Claude Code
/// (the form a terminal's file drop types), Codex (`shlex`) and OMP (a bracketed paste of
/// paths) all read back as the one path; for no agent (a shell) quoted like a drop. `None` for
/// a path that is not UTF-8 or holds a control character, which a line editor would act on.
pub fn path_text(path: &Path, agent: Option<PasteAgent>) -> Option<String> {
    let path = path.to_str()?;
    if path.chars().any(char::is_control) {
        return None;
    }
    Some(match agent {
        None => crate::paste::shell_quote(path),
        Some(_) => crate::paste::escape_path(path),
    })
}

/// A saved image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedImage {
    pub path: PathBuf,
    /// Bytes of the PNG file.
    pub bytes: u64,
}

/// Saves the image `bytes` (any format GPUI's clipboard reads) as the next `paste-<n>.png` in
/// `dir`, created with mode `0700` (files `0600`: a screenshot can hold anything). `n` is one
/// more than the largest the folder has, so a number is never reused while its file exists.
pub fn save_png(dir: &Path, bytes: &[u8]) -> Result<SavedImage, ImagePasteError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImagePasteError::TooLarge { size: bytes.len() });
    }
    let png = to_png(bytes)?;
    if png.len() > MAX_IMAGE_BYTES {
        return Err(ImagePasteError::TooLarge { size: png.len() });
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let (mut next, used) = scan(dir)?;
    if used.saturating_add(png.len() as u64) > MAX_SCRATCH_BYTES {
        return Err(ImagePasteError::ScratchFull { used });
    }
    loop {
        let path = dir.join(format!("{PREFIX}{next}{SUFFIX}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(&png) {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(error.into());
                }
                return Ok(SavedImage {
                    path,
                    bytes: png.len() as u64,
                });
            }
            // A paste in another view of the session took the number first.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => next += 1,
            Err(error) => return Err(error.into()),
        }
    }
}

/// The PNG of `bytes`: PNG as it is, the other formats decoded under limits and encoded.
fn to_png(bytes: &[u8]) -> Result<Vec<u8>, ImagePasteError> {
    let format = image::guess_format(bytes).map_err(|_| ImagePasteError::Unreadable)?;
    match format {
        ImageFormat::Png => {
            // Only a file that starts like a PNG is written under the name.
            if bytes.starts_with(PNG_SIGNATURE) {
                Ok(bytes.to_vec())
            } else {
                Err(ImagePasteError::Unreadable)
            }
        }
        ImageFormat::Jpeg
        | ImageFormat::Gif
        | ImageFormat::WebP
        | ImageFormat::Tiff
        | ImageFormat::Bmp => {
            let mut limits = Limits::default();
            limits.max_alloc = Some(MAX_DECODE_ALLOC);
            limits.max_image_width = Some(MAX_DIMENSION);
            limits.max_image_height = Some(MAX_DIMENSION);
            let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
            reader.limits(limits);
            let decoded = reader.decode().map_err(|_| ImagePasteError::Unreadable)?;
            let mut out = Cursor::new(Vec::new());
            decoded
                .write_to(&mut out, ImageFormat::Png)
                .map_err(|_| ImagePasteError::Unreadable)?;
            Ok(out.into_inner())
        }
        other => Err(ImagePasteError::Unsupported(
            other.extensions_str().first().map_or_else(
                || "this kind of".to_owned(),
                |extension| extension.to_ascii_uppercase(),
            ),
        )),
    }
}

/// The next number to try and the bytes the folder's pasted images take.
fn scan(dir: &Path) -> std::io::Result<(u64, u64)> {
    let (mut largest, mut used) = (0, 0);
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if let Some(n) = number_of(&entry.file_name().to_string_lossy()) {
            largest = largest.max(n);
            used += entry.metadata().map_or(0, |meta| meta.len());
        }
    }
    Ok((largest + 1, used))
}

/// `n` of `paste-<n>.png`.
fn number_of(name: &str) -> Option<u64> {
    name.strip_prefix(PREFIX)?
        .strip_suffix(SUFFIX)?
        .parse()
        .ok()
}

/// `1.5 MiB`, one decimal from KiB up.
pub fn human(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KIB {
        format!("{bytes} B")
    } else if value < KIB * KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{:.1} MiB", value / (KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 PNG, built with the encoder so the test needs no fixture.
    fn png_of(format: ImageFormat) -> Vec<u8> {
        let pixel = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            3,
            image::Rgb([10, 20, 30]),
        ));
        let mut out = Cursor::new(Vec::new());
        pixel.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_png_is_written_as_it_is_and_numbered_up() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch").join("s1");
        let png = png_of(ImageFormat::Png);
        let first = save_png(&scratch, &png).unwrap();
        assert_eq!(first.path, scratch.join("paste-1.png"));
        assert_eq!(std::fs::read(&first.path).unwrap(), png);
        let second = save_png(&scratch, &png).unwrap();
        assert_eq!(second.path, scratch.join("paste-2.png"));
        // A number is one more than the largest, so a deleted file's number is not reused
        // underneath a later one.
        std::fs::remove_file(&first.path).unwrap();
        assert_eq!(
            save_png(&scratch, &png).unwrap().path,
            scratch.join("paste-3.png")
        );
    }

    #[test]
    fn the_folder_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("s");
        let saved = save_png(&scratch, &png_of(ImageFormat::Png)).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&scratch), 0o700);
        assert_eq!(mode(&saved.path), 0o600);
    }

    #[test]
    fn other_formats_become_png() {
        let dir = tempfile::tempdir().unwrap();
        for format in [
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::Tiff,
            ImageFormat::Bmp,
        ] {
            let saved = save_png(dir.path(), &png_of(format)).unwrap();
            let written = std::fs::read(&saved.path).unwrap();
            assert!(written.starts_with(PNG_SIGNATURE), "{format:?}");
            let back = image::load_from_memory(&written).unwrap();
            assert_eq!((back.width(), back.height()), (2, 3), "{format:?}");
        }
    }

    #[test]
    fn what_is_not_a_supported_image_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("s");
        assert!(matches!(
            save_png(&scratch, b"not an image"),
            Err(ImagePasteError::Unreadable)
        ));
        assert!(matches!(
            save_png(
                &scratch,
                b"\x00\x00\x01\x00\x01\x00\x10\x10\x00\x00\x01\x00\x20\x00"
            ),
            Err(ImagePasteError::Unsupported(_))
        ));
        assert!(!scratch.exists(), "nothing was created");
    }

    #[test]
    fn an_image_over_the_limit_is_refused_before_any_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("s");
        let mut big = png_of(ImageFormat::Png);
        big.resize(MAX_IMAGE_BYTES + 5 * 1024 * 1024, 0);
        let error = save_png(&scratch, &big).unwrap_err();
        assert!(matches!(error, ImagePasteError::TooLarge { .. }));
        assert_eq!(
            error.to_string(),
            "The image is 25.0 MiB: at most 20.0 MiB can be pasted into a terminal"
        );
        assert!(!scratch.exists(), "nothing was created");
    }

    #[test]
    fn a_full_scratch_folder_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let big = std::fs::File::create(dir.path().join("paste-1.png")).unwrap();
        big.set_len(MAX_SCRATCH_BYTES).unwrap();
        let error = save_png(dir.path(), &png_of(ImageFormat::Png)).unwrap_err();
        assert!(
            matches!(error, ImagePasteError::ScratchFull { .. }),
            "{error}"
        );
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(3 * 1024 * 1024), "3.0 MiB");
    }
}
