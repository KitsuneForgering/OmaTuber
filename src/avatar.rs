//! Loading and validation of avatar images: PNG, APNG and GIF.
//!
//! A file is read once; the same bytes are decoded for the preview and later
//! served to the browser source, so a file changed on disk afterwards cannot
//! slip past validation. The format comes from the bytes, not the extension.

use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::{AnimationDecoder, ImageDecoder, ImageFormat, ImageReader};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_side: u32,
    /// Frames of an animation.
    pub max_frames: usize,
    /// Decoded RGBA bytes across all frames; the preview keeps them all.
    pub max_decoded_bytes: u64,
    /// One loop of an animation.
    pub max_duration: Duration,
}

impl Default for Limits {
    /// The animation limits are first guesses, to be revisited with real
    /// avatars (plan: "limite próprio ... a definir com testes de uso real").
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_side: 4096,
            max_frames: 600,
            max_decoded_bytes: 256 * 1024 * 1024,
            max_duration: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Apng,
    Gif,
}

impl Kind {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Apng => "image/apng",
            Self::Gif => "image/gif",
        }
    }

    pub fn animated(self) -> bool {
        self != Self::Png
    }
}

#[derive(Clone)]
pub struct Frame {
    /// Full canvas, unpremultiplied RGBA8, with disposal and blending applied.
    pub rgba: Vec<u8>,
    /// Display time, as Chromium (and so OBS's browser source) will use it.
    pub delay: Duration,
}

#[derive(Clone)]
pub struct Png {
    /// The file exactly as read, for serving.
    pub bytes: Arc<[u8]>,
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
    /// One frame for a still image. Never empty.
    pub frames: Vec<Frame>,
}

impl Png {
    /// Length of one loop; zero for a still image.
    pub fn duration(&self) -> Duration {
        if self.kind.animated() {
            self.frames.iter().map(|f| f.delay).sum()
        } else {
            Duration::ZERO
        }
    }

    /// Index of the frame shown `elapsed` after the animation started, looping.
    pub fn frame_at(&self, elapsed: Duration) -> usize {
        let delays: Vec<Duration> = self.frames.iter().map(|f| f.delay).collect();
        frame_index(&delays, elapsed).0
    }
}

/// Frame shown `elapsed` into a looping animation with these per-frame
/// delays, and how long until the next frame. A still image (no delays, or
/// all zero) stays on frame 0 forever.
pub fn frame_index(delays: &[Duration], elapsed: Duration) -> (usize, Option<Duration>) {
    let total: Duration = delays.iter().sum();
    if total.is_zero() {
        return (0, None);
    }
    let mut t = Duration::from_nanos((elapsed.as_nanos() % total.as_nanos()) as u64);
    for (i, &d) in delays.iter().enumerate() {
        if t < d {
            return (i, Some(d - t));
        }
        t -= d;
    }
    (delays.len() - 1, Some(Duration::ZERO))
}

/// Chromium shows frames that ask for ≤ 10 ms for 100 ms instead
/// (`third_party/blink/renderer/platform/graphics/deferred_image_decoder.cc`,
/// `FrameDurationAtIndex`). The preview follows it to match the browser source.
pub fn browser_delay(requested: Duration) -> Duration {
    if requested <= Duration::from_millis(10) {
        Duration::from_millis(100)
    } else {
        requested
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    TooLarge {
        max_bytes: u64,
    },
    Unsupported,
    TooBig {
        width: u32,
        height: u32,
        max_side: u32,
    },
    TooManyFrames {
        max: usize,
    },
    TooMuchMemory {
        max_mib: u64,
    },
    TooLong {
        max: Duration,
    },
    Decode(image::ImageError),
    /// Valid APNG/GIF, refused while animation support is still being
    /// validated in OBS.
    AnimationNotEnabled,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "não foi possível ler o arquivo: {e}"),
            Self::TooLarge { max_bytes } => {
                write!(f, "arquivo maior que {} MiB", max_bytes / (1024 * 1024))
            }
            Self::Unsupported => write!(f, "o arquivo não é PNG, APNG nem GIF"),
            Self::TooBig {
                width,
                height,
                max_side,
            } => write!(
                f,
                "imagem de {width}×{height}; o limite é {max_side} pixels por lado"
            ),
            Self::TooManyFrames { max } => write!(f, "animação com mais de {max} quadros"),
            Self::TooMuchMemory { max_mib } => write!(
                f,
                "animação grande demais: passaria de {max_mib} MiB na memória"
            ),
            Self::TooLong { max } => {
                write!(f, "animação mais longa que {} s", max.as_secs())
            }
            Self::Decode(e) => write!(f, "imagem inválida: {e}"),
            Self::AnimationNotEnabled => write!(
                f,
                "animações (APNG e GIF) ainda estão em teste; use um PNG parado por enquanto"
            ),
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

pub fn decode(bytes: Vec<u8>, limits: Limits) -> Result<Png, LoadError> {
    let format = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(LoadError::Io)?
        .format();
    let (kind, width, height, frames) = match format {
        Some(ImageFormat::Png) => {
            let decoder = PngDecoder::new(Cursor::new(&bytes)).map_err(LoadError::Decode)?;
            let (w, h) = decoder.dimensions();
            check_side(w, h, limits)?;
            if decoder.is_apng().map_err(LoadError::Decode)? {
                let frames = collect(decoder.apng().map_err(LoadError::Decode)?, w, h, limits)?;
                (Kind::Apng, w, h, frames)
            } else {
                let image =
                    image::DynamicImage::from_decoder(decoder).map_err(LoadError::Decode)?;
                let rgba = image.into_rgba8().into_raw();
                (
                    Kind::Png,
                    w,
                    h,
                    vec![Frame {
                        rgba,
                        delay: Duration::ZERO,
                    }],
                )
            }
        }
        Some(ImageFormat::Gif) => {
            let decoder = GifDecoder::new(Cursor::new(&bytes)).map_err(LoadError::Decode)?;
            let (w, h) = decoder.dimensions();
            check_side(w, h, limits)?;
            (Kind::Gif, w, h, collect(decoder, w, h, limits)?)
        }
        _ => return Err(LoadError::Unsupported),
    };
    // An APNG whose animation holds a single frame still has a delay; keep it
    // animated only if there is something to animate.
    let kind = if frames.len() == 1 && kind == Kind::Apng {
        Kind::Png
    } else {
        kind
    };
    Ok(Png {
        bytes: bytes.into(),
        kind,
        width,
        height,
        frames,
    })
}

fn check_side(width: u32, height: u32, limits: Limits) -> Result<(), LoadError> {
    if width > limits.max_side || height > limits.max_side {
        return Err(LoadError::TooBig {
            width,
            height,
            max_side: limits.max_side,
        });
    }
    Ok(())
}

/// Decodes frames one by one, stopping as soon as a limit is crossed so a
/// huge animation fails early instead of after filling memory.
fn collect<'a>(
    decoder: impl AnimationDecoder<'a>,
    width: u32,
    height: u32,
    limits: Limits,
) -> Result<Vec<Frame>, LoadError> {
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    let mut frames = Vec::new();
    let mut total = Duration::ZERO;
    for frame in decoder.into_frames() {
        let frame = frame.map_err(LoadError::Decode)?;
        if frames.len() == limits.max_frames {
            return Err(LoadError::TooManyFrames {
                max: limits.max_frames,
            });
        }
        if (frames.len() as u64 + 1) * frame_bytes > limits.max_decoded_bytes {
            return Err(LoadError::TooMuchMemory {
                max_mib: limits.max_decoded_bytes / (1024 * 1024),
            });
        }
        let (num, den) = frame.delay().numer_denom_ms();
        let requested = Duration::from_secs_f64(f64::from(num) / f64::from(den.max(1)) / 1000.0);
        let delay = browser_delay(requested);
        total += delay;
        if total > limits.max_duration {
            return Err(LoadError::TooLong {
                max: limits.max_duration,
            });
        }
        frames.push(Frame {
            rgba: frame.into_buffer().into_raw(),
            delay,
        });
    }
    if frames.is_empty() {
        return Err(LoadError::Unsupported);
    }
    Ok(frames)
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

    /// APNG with one solid color per frame and the given delays in ms.
    fn apng(colors: &[[u8; 4]], delays_ms: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = ::png::Encoder::new(&mut out, 2, 2);
        enc.set_color(::png::ColorType::Rgba);
        enc.set_depth(::png::BitDepth::Eight);
        enc.set_animated(colors.len() as u32, 0).unwrap();
        let mut w = enc.write_header().unwrap();
        for (c, &d) in colors.iter().zip(delays_ms) {
            w.set_frame_delay(d, 1000).unwrap();
            w.write_image_data(&c.repeat(4)).unwrap();
        }
        w.finish().unwrap();
        out
    }

    fn gif(colors: &[[u8; 4]], delay_cs: u16) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            for c in colors {
                let buf = image::RgbaImage::from_pixel(2, 2, image::Rgba(*c));
                let delay = image::Delay::from_numer_denom_ms(u32::from(delay_cs) * 10, 1);
                enc.encode_frame(image::Frame::from_parts(buf, 0, 0, delay))
                    .unwrap();
            }
        }
        out
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    #[test]
    fn decodes_valid_png_and_keeps_original_bytes() {
        let bytes = png(3, 2);
        let p = decode(bytes.clone(), Limits::default()).unwrap();
        assert_eq!((p.kind, p.width, p.height), (Kind::Png, 3, 2));
        assert_eq!(p.frames.len(), 1);
        assert_eq!(&p.frames[0].rgba[..4], &[255, 0, 0, 128]);
        assert_eq!(&*p.bytes, &bytes[..]);
        assert_eq!(p.duration(), Duration::ZERO);
    }

    #[test]
    fn decodes_apng_frames_and_delays() {
        let p = decode(
            apng(&[RED, BLUE, CLEAR], &[100, 250, 50]),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(p.kind, Kind::Apng);
        assert_eq!(p.kind.mime(), "image/apng");
        let delays: Vec<_> = p.frames.iter().map(|f| f.delay.as_millis()).collect();
        assert_eq!(delays, [100, 250, 50]);
        assert_eq!(&p.frames[1].rgba[..4], &BLUE);
        assert_eq!(p.frames[2].rgba[3], 0, "transparency kept");
        assert_eq!(p.duration(), Duration::from_millis(400));
    }

    #[test]
    fn decodes_gif_frames_and_delays() {
        let p = decode(gif(&[RED, BLUE], 20), Limits::default()).unwrap();
        assert_eq!(p.kind, Kind::Gif);
        assert_eq!(p.frames.len(), 2);
        assert_eq!(p.frames[0].delay, Duration::from_millis(200));
        assert_eq!(&p.frames[1].rgba[..3], &BLUE[..3]);
    }

    #[test]
    fn tiny_delays_follow_chromium() {
        let p = decode(apng(&[RED, BLUE], &[0, 10]), Limits::default()).unwrap();
        assert!(
            p.frames
                .iter()
                .all(|f| f.delay == Duration::from_millis(100))
        );
        assert_eq!(
            browser_delay(Duration::from_millis(11)),
            Duration::from_millis(11)
        );
    }

    #[test]
    fn frame_at_loops_over_delays() {
        let p = decode(
            apng(&[RED, BLUE, CLEAR], &[100, 250, 50]),
            Limits::default(),
        )
        .unwrap();
        let at = |ms| p.frame_at(Duration::from_millis(ms));
        assert_eq!(
            [at(0), at(99), at(100), at(349), at(350), at(399)],
            [0, 0, 1, 1, 2, 2]
        );
        assert_eq!(at(400), 0, "wraps after one loop");
        assert_eq!(at(400 * 1000 + 120), 1);
    }

    #[test]
    fn animation_limits_stop_early() {
        let colors = [RED; 5];
        let few = Limits {
            max_frames: 4,
            ..Limits::default()
        };
        assert!(matches!(
            decode(apng(&colors, &[100; 5]), few),
            Err(LoadError::TooManyFrames { max: 4 })
        ));
        // 2×2×4 = 16 bytes per frame.
        let small = Limits {
            max_decoded_bytes: 16 * 3,
            ..Limits::default()
        };
        assert!(matches!(
            decode(apng(&colors, &[100; 5]), small),
            Err(LoadError::TooMuchMemory { .. })
        ));
        let short = Limits {
            max_duration: Duration::from_millis(300),
            ..Limits::default()
        };
        assert!(matches!(
            decode(apng(&colors, &[100; 5]), short),
            Err(LoadError::TooLong { .. })
        ));
        assert!(decode(apng(&colors, &[100; 5]), Limits::default()).is_ok());
    }

    #[test]
    fn rejects_other_formats() {
        let mut jpeg_header = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg_header.resize(64, 0);
        assert!(matches!(
            decode(jpeg_header, Limits::default()),
            Err(LoadError::Unsupported)
        ));
        assert!(matches!(
            decode(b"hello".to_vec(), Limits::default()),
            Err(LoadError::Unsupported)
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
