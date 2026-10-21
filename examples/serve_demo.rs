//! Serves the browser source with generated avatar PNGs and a mouth that
//! toggles on a timer, for checking the page in a real browser without OBS.
//!
//!     cargo run --example serve_demo -- [port] [toggle-ms]

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use image::{ImageFormat, Rgba, RgbaImage};
use omatuber::mouth::Mouth;
use omatuber::server::{self, Outputs};

/// Opaque face on a transparent background; `open` draws an open mouth.
fn face(open: bool) -> Arc<[u8]> {
    let mut img = RgbaImage::new(240, 280);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 - 120.0, y as f32 - 150.0);
        let mut c = Rgba([0, 0, 0, 0]);
        if (fx / 105.0).powi(2) + (fy / 120.0).powi(2) <= 1.0 {
            c = Rgba([250, 214, 180, 255]);
        }
        let eye = [-40.0, 40.0]
            .iter()
            .any(|ex| (fx - ex).powi(2) + (fy + 25.0).powi(2) <= 110.0);
        let mouth = if open {
            (fx / 28.0).powi(2) + ((fy - 50.0) / 22.0).powi(2) <= 1.0
        } else {
            fx.abs() <= 30.0 && (fy - 50.0).abs() <= 3.0
        };
        if eye || mouth {
            c = Rgba([40, 30, 40, 255]);
        }
        *px = c;
    }
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner().into()
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let port = args
        .next()
        .map_or(server::DEFAULT_PORT + 1, |p| p.parse().expect("port"));
    let toggle = Duration::from_millis(args.next().map_or(500, |t| t.parse().expect("ms")));

    let outputs = Outputs::default();
    outputs.set_image(Mouth::Closed, face(false));
    outputs.set_image(Mouth::Open, face(true));
    let listener = server::bind(port).await.expect("bind");
    eprintln!("http://127.0.0.1:{port}/");
    tokio::spawn(server::serve(listener, outputs.clone()));

    let mut mouth = Mouth::Closed;
    loop {
        tokio::time::sleep(toggle).await;
        mouth = if mouth == Mouth::Open {
            Mouth::Closed
        } else {
            Mouth::Open
        };
        outputs.set_mouth(mouth);
    }
}
