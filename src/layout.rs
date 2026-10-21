//! Where the two base images sit relative to each other.
//!
//! Both are drawn at the same scale on a shared canvas, centered, so a
//! character drawn at the same pixel size in both files keeps its size when
//! the mouth moves even if the files differ in size. Fitting each file to the
//! frame on its own (what `object-fit: contain` does) would shrink the larger
//! file's character and make the avatar "jump" on every syllable. The open
//! image can be nudged by a pixel offset to fix art that is not aligned.

use serde::{Deserialize, Serialize};

/// Offset of the open-mouth image, in its own pixels; positive is right/down.
pub type Offset = [i32; 2];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
}

/// Placement of one image on the canvas, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Canvas holding both images; `None` sizes are ignored.
pub fn canvas(closed: Option<[u32; 2]>, open: Option<[u32; 2]>) -> Option<Canvas> {
    let sizes = [closed, open];
    let mut known = sizes.iter().flatten();
    let first = known.next()?;
    let (w, h) = known.fold((first[0], first[1]), |(w, h), s| (w.max(s[0]), h.max(s[1])));
    Some(Canvas {
        width: w.max(1),
        height: h.max(1),
    })
}

/// Where an image of `size` goes on `canvas`: centered, plus `offset`.
pub fn place(canvas: Canvas, size: [u32; 2], offset: Offset) -> Placed {
    Placed {
        x: (canvas.width as f32 - size[0] as f32) / 2.0 + offset[0] as f32,
        y: (canvas.height as f32 - size[1] as f32) / 2.0 + offset[1] as f32,
        width: size[0] as f32,
        height: size[1] as f32,
    }
}

/// Scale and top-left corner that fit `canvas` inside a `frame` of the given
/// width and height, centered (what `object-fit: contain` does for one image,
/// applied to the shared canvas instead).
pub fn fit(canvas: Canvas, frame: [f32; 2]) -> (f32, [f32; 2]) {
    let scale = (frame[0] / canvas.width as f32).min(frame[1] / canvas.height as f32);
    let origin = [
        (frame[0] - canvas.width as f32 * scale) / 2.0,
        (frame[1] - canvas.height as f32 * scale) / 2.0,
    ];
    (scale, origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_spans_both_images() {
        assert_eq!(
            canvas(Some([512, 512]), Some([600, 480])),
            Some(Canvas {
                width: 600,
                height: 512
            })
        );
        assert_eq!(
            canvas(None, Some([10, 20])),
            Some(Canvas {
                width: 10,
                height: 20
            })
        );
        assert_eq!(canvas(None, None), None);
    }

    #[test]
    fn same_pixels_keep_the_same_size_across_files() {
        // A 400 px character in a 512 canvas and in a 600 canvas.
        let c = canvas(Some([512, 512]), Some([600, 600])).unwrap();
        let (scale, _) = fit(c, [300.0, 300.0]);
        let closed = place(c, [512, 512], [0, 0]);
        let open = place(c, [600, 600], [0, 0]);
        // Both scaled by the same factor: 400 px of art is 200 px on screen
        // in either file. Fitting each alone would give 234 vs 200.
        assert_eq!(scale, 0.5);
        assert_eq!((closed.width * scale, open.width * scale), (256.0, 300.0));
        assert_eq!((closed.x, closed.y), (44.0, 44.0), "smaller one centered");
        assert_eq!((open.x, open.y), (0.0, 0.0));
    }

    #[test]
    fn offset_moves_only_that_image() {
        let c = Canvas {
            width: 100,
            height: 100,
        };
        let p = place(c, [100, 100], [3, -2]);
        assert_eq!((p.x, p.y), (3.0, -2.0));
    }

    #[test]
    fn fit_centers_the_canvas_in_the_frame() {
        let (scale, origin) = fit(
            Canvas {
                width: 200,
                height: 100,
            },
            [400.0, 400.0],
        );
        assert_eq!(scale, 2.0);
        assert_eq!(origin, [0.0, 100.0]);
    }
}
