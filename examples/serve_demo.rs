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
    // DEMO_CLOSED=/path/to/file serves that image (PNG, APNG or GIF) instead.
    match std::env::var_os("DEMO_CLOSED") {
        Some(path) => {
            let png =
                omatuber::avatar::load(path.as_ref(), Default::default()).expect("DEMO_CLOSED");
            outputs.set_image(Mouth::Closed, png.bytes, png.kind.mime());
        }
        None => outputs.set_image(Mouth::Closed, face(false), "image/png"),
    }
    // DEMO_PAD=n draws the open face on a canvas n px larger on each side,
    // as art exported from a different document would be.
    let pad: u32 = std::env::var("DEMO_PAD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let open = if pad == 0 {
        face(true)
    } else {
        let small = image::load_from_memory(&face(true)).unwrap().into_rgba8();
        let mut big = RgbaImage::new(240 + 2 * pad, 280 + 2 * pad);
        image::imageops::overlay(&mut big, &small, i64::from(pad), i64::from(pad));
        let mut out = Cursor::new(Vec::new());
        big.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner().into()
    };
    outputs.set_image(Mouth::Open, open, "image/png");
    outputs.set_layout(
        [Some([240, 280]), Some([240 + 2 * pad, 280 + 2 * pad])],
        [0, 0],
    );
    // DEMO_SCENE=1 adds a sentiment (the open-mouth fox as its quiet image)
    // and a red disc object near the top of the avatar.
    if std::env::var_os("DEMO_SCENE").is_some() {
        let disc = RgbaImage::from_fn(64, 64, |x, y| {
            let (dx, dy) = (x as f32 - 31.5, y as f32 - 31.5);
            if dx * dx + dy * dy <= 30.0 * 30.0 {
                Rgba([230, 30, 40, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        });
        let mut out = Cursor::new(Vec::new());
        disc.write_to(&mut out, ImageFormat::Png).unwrap();
        let mut assets = std::collections::HashMap::new();
        assets.insert(
            "s-q".to_owned(),
            (Arc::from(omatuber::example::png(true)), "image/png"),
        );
        assets.insert(
            "disc".to_owned(),
            (Arc::from(out.into_inner()), "image/png"),
        );
        outputs.set_assets(assets);
        outputs.set_scene(server::Scene {
            sentiment: Some(server::SceneSentiment {
                quiet: Some("s-q".to_owned()),
                talking: None,
                trigger: 1,
            }),
            objects: vec![server::SceneObject {
                asset: "disc".to_owned(),
                x: 0.5,
                y: 0.15,
                scale: 0.25,
            }],
            ..Default::default()
        });
    }
    let listener = server::bind(port).await.expect("bind");
    eprintln!("http://127.0.0.1:{port}/");
    tokio::spawn(server::serve(listener, outputs.clone()));

    // DEMO_BLINK adds an eyes-closed version of the closed face and keeps
    // the eyes closed, to check the page draws it.
    if std::env::var_os("DEMO_BLINK").is_some() {
        let mut img = image::load_from_memory(&face(false)).unwrap().into_rgba8();
        for (x, y, px) in img.enumerate_pixels_mut() {
            let (fx, fy) = (x as f32 - 120.0, y as f32 - 150.0);
            if [-40.0f32, 40.0]
                .iter()
                .any(|ex| (fx - ex).powi(2) + (fy + 25.0).powi(2) <= 110.0)
            {
                *px = Rgba([250, 214, 180, 255]);
            }
        }
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        outputs.set_blink_image(Mouth::Closed, Some((out.into_inner().into(), "image/png")));
        outputs.set_blink(true);
    }
    // DEMO_OPEN starts with the mouth open (use a long toggle to keep it).
    let mut mouth = if std::env::var_os("DEMO_OPEN").is_some() {
        Mouth::Open
    } else {
        Mouth::Closed
    };
    outputs.set_mouth(mouth);
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
