use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};

const MAX_IMAGE_PIXELS: usize = 16_777_216;
const MAX_IMAGE_LINES: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClipboardPaste {
    Text(String),
    Image { placeholder: String, path: PathBuf },
}

pub(crate) fn read_clipboard() -> anyhow::Result<ClipboardPaste> {
    let mut clipboard = arboard::Clipboard::new().context("open system clipboard")?;
    if let Ok(image) = clipboard.get_image() {
        return save_clipboard_image(
            image.width,
            image.height,
            image.bytes.into_owned(),
            Path::new(".ridge").join("pasted-images"),
        );
    }
    clipboard
        .get_text()
        .map(|text| ClipboardPaste::Text(crate::tui::sanitize_paste(&text)))
        .context("read clipboard text")
}

fn save_clipboard_image(
    width: usize,
    height: usize,
    bytes: Vec<u8>,
    directory: PathBuf,
) -> anyhow::Result<ClipboardPaste> {
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| anyhow!("clipboard image dimensions overflow"))?;
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS {
        return Err(anyhow!("clipboard image is too large"));
    }
    let expected = pixels
        .checked_mul(4)
        .ok_or_else(|| anyhow!("clipboard image byte length overflow"))?;
    if bytes.len() != expected {
        return Err(anyhow!("clipboard image has invalid RGBA data"));
    }
    let width = u32::try_from(width).map_err(|_| anyhow!("clipboard image is too wide"))?;
    let height = u32::try_from(height).map_err(|_| anyhow!("clipboard image is too tall"))?;
    let path = next_image_path(directory)?;
    let rgba = image::RgbaImage::from_raw(width, height, bytes)
        .ok_or_else(|| anyhow!("clipboard image cannot be represented as RGBA"))?;
    rgba.save_with_format(&path, image::ImageFormat::Png)
        .with_context(|| format!("save clipboard image to {}", path.display()))?;
    let lines = height as usize;
    let id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image1");
    Ok(ClipboardPaste::Image {
        placeholder: format!("[{id}] [{} lines]", lines.clamp(1, MAX_IMAGE_LINES)),
        path,
    })
}

fn next_image_path(directory: PathBuf) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create clipboard image directory {}", directory.display()))?;
    let mut next = 1usize;
    for entry in std::fs::read_dir(&directory)
        .with_context(|| format!("read clipboard image directory {}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(number) = name
            .strip_prefix("image")
            .and_then(|value| value.strip_suffix(".png"))
            .and_then(|value| value.parse::<usize>().ok())
        else {
            continue;
        };
        next = next.max(number.saturating_add(1));
    }
    Ok(directory.join(format!("image{next}.png")))
}

#[cfg(test)]
mod tests {
    use super::{next_image_path, read_clipboard, save_clipboard_image, ClipboardPaste};

    #[test]
    fn image_paths_increment_without_overwriting_existing_files() {
        let root =
            std::env::temp_dir().join(format!("ridgecode-clipboard-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("image1.png"), []).unwrap();
        std::fs::write(root.join("image7.png"), []).unwrap();
        assert_eq!(
            next_image_path(root.clone()).unwrap().file_name().unwrap(),
            "image8.png"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn image_paste_validates_saves_and_labels_rgba() {
        let root = std::env::temp_dir().join(format!(
            "ridgecode-clipboard-image-test-{}",
            std::process::id()
        ));
        let result = save_clipboard_image(2, 3, vec![255; 24], root.clone()).unwrap();
        let ClipboardPaste::Image { placeholder, path } = result else {
            panic!("expected image paste");
        };
        assert_eq!(placeholder, "[image1] [3 lines]");
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn image_paste_rejects_bad_dimensions_and_bytes() {
        let root = std::env::temp_dir().join(format!(
            "ridgecode-clipboard-invalid-test-{}",
            std::process::id()
        ));
        assert!(save_clipboard_image(0, 1, Vec::new(), root.clone()).is_err());
        assert!(save_clipboard_image(usize::MAX, 2, Vec::new(), root.clone()).is_err());
        assert!(save_clipboard_image(2, 2, vec![0; 3], root.clone()).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn system_clipboard_probe_is_bounded() {
        let _ = read_clipboard();
    }
}
