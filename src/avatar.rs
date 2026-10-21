//! Loading and validation of the avatar PNGs.
//!
//! A file is read once; the same bytes are decoded for the preview and later
//! served to the browser source, so a file changed on disk afterwards cannot
//! slip past validation.

use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use image::{ImageFormat, ImageReader};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_side: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_side: 4096,
        }
    }
}

#[derive(Clone)]
pub struct Png {
    /// The file exactly as read, for serving.
    pub bytes: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// Decoded pixels, unpremultiplied RGBA8, for the preview.
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    TooLarge {
        max_bytes: u64,
    },
    NotPng,
    TooBig {
        width: u32,
        height: u32,
        max_side: u32,
    },
    Decode(image::ImageError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "não foi possível ler o arquivo: {e}"),
            Self::TooLarge { max_bytes } => {
                write!(f, "arquivo maior que {} MiB", max_bytes / (1024 * 1024))
            }
            Self::NotPng => write!(f, "o arquivo não é um PNG"),
            Self::TooBig {
                width,
                height,
                max_side,
            } => write!(
                f,
                "imagem de {width}×{height}; o limite é {max_side} pixels por lado"
            ),
            Self::Decode(e) => write!(f, "PNG inválido: {e}"),
        }
    }
}

pub fn load(path: &Path, limits: Limits) -> Result<Png, LoadError> {
    let mut bytes = Vec::new();
    // Read one byte past the limit to detect oversize files without trusting metadata.
    File::open(path)
        .and_then(|f| f.take(limits.max_bytes + 1).read_to_end(&mut bytes))
        .map_err(LoadError::Io)?;
    if bytes.len() as u64 > limits.max_bytes {
        return Err(LoadError::TooLarge {
            max_bytes: limits.max_bytes,
        });
    }
    decode(bytes, limits)
}

fn decode(bytes: Vec<u8>, limits: Limits) -> Result<Png, LoadError> {
    let reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(LoadError::Io)?;
    if reader.format() != Some(ImageFormat::Png) {
        return Err(LoadError::NotPng);
    }
    let (width, height) = reader.into_dimensions().map_err(LoadError::Decode)?;
    if width > limits.max_side || height > limits.max_side {
        return Err(LoadError::TooBig {
            width,
            height,
            max_side: limits.max_side,
        });
    }
    let image =
        image::load_from_memory_with_format(&bytes, ImageFormat::Png).map_err(LoadError::Decode)?;
    Ok(Png {
        bytes: bytes.into(),
        width,
        height,
        rgba: image.into_rgba8().into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 128]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn decodes_valid_png_and_keeps_original_bytes() {
        let bytes = png(3, 2);
        let p = decode(bytes.clone(), Limits::default()).unwrap();
        assert_eq!((p.width, p.height), (3, 2));
        assert_eq!(p.rgba.len(), 3 * 2 * 4);
        assert_eq!(&p.rgba[..4], &[255, 0, 0, 128]);
        assert_eq!(&*p.bytes, &bytes[..]);
    }

    #[test]
    fn rejects_other_formats() {
        let mut jpeg_header = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg_header.resize(64, 0);
        assert!(matches!(
            decode(jpeg_header, Limits::default()),
            Err(LoadError::NotPng)
        ));
        assert!(matches!(
            decode(b"hello".to_vec(), Limits::default()),
            Err(LoadError::NotPng)
        ));
    }

    #[test]
    fn rejects_oversized_dimensions_before_decoding() {
        let limits = Limits {
            max_side: 4,
            ..Limits::default()
        };
        assert!(matches!(
            decode(png(5, 1), limits),
            Err(LoadError::TooBig {
                width: 5,
                height: 1,
                max_side: 4
            })
        ));
        assert!(decode(png(4, 4), limits).is_ok());
    }

    #[test]
    fn rejects_truncated_png() {
        let mut bytes = png(8, 8);
        bytes.truncate(bytes.len() - 20);
        assert!(matches!(
            decode(bytes, Limits::default()),
            Err(LoadError::Decode(_))
        ));
    }

    #[test]
    fn rejects_file_over_byte_limit() {
        let dir = std::env::temp_dir().join(format!("omatuber-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        let bytes = png(16, 16);
        std::fs::write(&path, &bytes).unwrap();

        let exact = Limits {
            max_bytes: bytes.len() as u64,
            ..Limits::default()
        };
        assert!(load(&path, exact).is_ok());
        let short = Limits {
            max_bytes: bytes.len() as u64 - 1,
            ..Limits::default()
        };
        assert!(matches!(
            load(&path, short),
            Err(LoadError::TooLarge { .. })
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_io_error() {
        let r = load(Path::new("/nonexistent/omatuber.png"), Limits::default());
        assert!(matches!(r, Err(LoadError::Io(_))));
    }
}
