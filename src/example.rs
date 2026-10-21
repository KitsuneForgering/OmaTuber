//! A sample avatar drawn in code, so people can try the app before having
//! their own art. Being generated here, it carries the project's license.

use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

use image::{ImageFormat, Rgba, RgbaImage};

const SIZE: u32 = 512;
/// Subsamples per axis for antialiased edges.
const AA: u32 = 4;

const ORANGE: [u8; 3] = [0xf2, 0x8c, 0x38];
const CREAM: [u8; 3] = [0xfb, 0xee, 0xdc];
const DARK: [u8; 3] = [0x2b, 0x21, 0x2b];
const PINK: [u8; 3] = [0xf0, 0x7a, 0x8a];

fn ellipse(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> bool {
    ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0
}

fn triangle(x: f32, y: f32, [a, b, c]: [(f32, f32); 3]) -> bool {
    let side =
        |(x1, y1): (f32, f32), (x2, y2): (f32, f32)| (x - x2) * (y1 - y2) - (x1 - x2) * (y - y2);
    let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

/// Color of the topmost shape at a point, or `None` for background.
fn shade(x: f32, y: f32, open: bool) -> Option<[u8; 3]> {
    let mouth = if open {
        ellipse(x, y, 256.0, 392.0, 34.0, 30.0)
    } else {
        (x - 256.0).abs() <= 26.0 && (y - 388.0).abs() <= 4.0
    };
    if open && ellipse(x, y, 256.0, 406.0, 20.0, 12.0) && mouth {
        return Some(PINK);
    }
    if mouth || ellipse(x, y, 256.0, 352.0, 16.0, 11.0) {
        return Some(DARK); // mouth, nose
    }
    for ex in [186.0, 326.0] {
        if ellipse(x, y, ex, 286.0, 16.0, 21.0) {
            return Some(DARK);
        }
    }
    if ellipse(x, y, 256.0, 378.0, 92.0, 62.0) {
        return Some(CREAM); // muzzle
    }
    for (l, r, tip) in [
        ((110.0, 210.0), (205.0, 150.0), (118.0, 52.0)),
        ((402.0, 210.0), (307.0, 150.0), (394.0, 52.0)),
    ] {
        let inner = [
            (l.0 + (tip.0 - l.0) * 0.18, l.1 - 18.0),
            (r.0 + (tip.0 - r.0) * 0.18, r.1 - 10.0),
            (tip.0 + (256.0 - tip.0) * 0.06, tip.1 + 46.0),
        ];
        if triangle(x, y, inner) {
            return Some(DARK);
        }
        if triangle(x, y, [l, r, tip]) {
            return Some(ORANGE);
        }
    }
    ellipse(x, y, 256.0, 300.0, 170.0, 150.0).then_some(ORANGE)
}

pub fn render(open: bool) -> RgbaImage {
    RgbaImage::from_fn(SIZE, SIZE, |px, py| {
        let (mut sum, mut hits) = ([0u32; 3], 0u32);
        for sy in 0..AA {
            for sx in 0..AA {
                let x = px as f32 + (sx as f32 + 0.5) / AA as f32;
                let y = py as f32 + (sy as f32 + 0.5) / AA as f32;
                if let Some(c) = shade(x, y, open) {
                    for k in 0..3 {
                        sum[k] += c[k] as u32;
                    }
                    hits += 1;
                }
            }
        }
        if hits == 0 {
            return Rgba([0, 0, 0, 0]);
        }
        // Unpremultiplied: average color of covered subsamples, alpha = coverage.
        let c = sum.map(|v| (v / hits) as u8);
        Rgba([c[0], c[1], c[2], (hits * 255 / (AA * AA)) as u8])
    })
}

pub fn png(open: bool) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    render(open)
        .write_to(&mut out, ImageFormat::Png)
        .expect("encoding to memory cannot fail");
    out.into_inner()
}

/// Writes `quieto.png` and `falando.png` into `dir`; returns [closed, open].
pub fn write_to(dir: &Path) -> io::Result<[PathBuf; 2]> {
    std::fs::create_dir_all(dir)?;
    let closed = dir.join("quieto.png");
    let open = dir.join("falando.png");
    std::fs::write(&closed, png(false))?;
    std::fs::write(&open, png(true))?;
    Ok([closed, open])
}

/// `$XDG_DATA_HOME/omatuber/example`, falling back to `~/.local/share`.
pub fn default_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("omatuber").join("example"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_transparent_and_face_is_opaque() {
        let img = render(false);
        assert_eq!(img.get_pixel(0, 0)[3], 0);
        assert_eq!(img.get_pixel(SIZE - 1, SIZE - 1)[3], 0);
        assert_eq!(img.get_pixel(256, 230)[3], 255);
    }

    #[test]
    fn open_and_closed_differ_only_around_the_mouth() {
        let (a, b) = (render(false), render(true));
        let mut changed = 0;
        for (x, y, pa) in a.enumerate_pixels() {
            if pa != b.get_pixel(x, y) {
                changed += 1;
                assert!(
                    (200..=312).contains(&x) && (350..=440).contains(&y),
                    "changed at {x},{y}"
                );
            }
        }
        assert!(changed > 1000);
    }

    #[test]
    fn written_files_pass_avatar_validation() {
        let dir = std::env::temp_dir().join(format!("omatuber-example-{}", std::process::id()));
        let [closed, open] = write_to(&dir).unwrap();
        for path in [closed, open] {
            let png = crate::avatar::load(&path, crate::avatar::Limits::default()).unwrap();
            assert_eq!((png.width, png.height), (SIZE, SIZE));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
