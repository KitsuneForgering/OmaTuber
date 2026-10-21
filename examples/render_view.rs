//! Renders the window offscreen in a few fake states, for visual review
//! without opening a window on the desktop.
//!
//!     cargo run --example render_view -- <output-dir>

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, ColorImage, TextureOptions};
use egui_kittest::Harness;
use omatuber::mouth::{GateParams, Mouth};
use omatuber::obs::{AudioInput, Connection, Status};
use omatuber::theme;
use omatuber::view::{self, SlotView, ViewState};
use uuid::Uuid;

/// A simple face with a transparent background; `open` draws an open mouth.
fn face(open: bool) -> ColorImage {
    let (w, h) = (240usize, 280usize);
    let mut px = vec![egui::Color32::TRANSPARENT; w * h];
    let skin = egui::Color32::from_rgb(250, 214, 180);
    let dark = egui::Color32::from_rgb(40, 30, 40);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 - 120.0, y as f32 - 150.0);
            let mut c = egui::Color32::TRANSPARENT;
            if (fx / 105.0).powi(2) + (fy / 120.0).powi(2) <= 1.0 {
                c = skin;
            }
            for ex in [-40.0, 40.0] {
                if (fx - ex).powi(2) + (fy + 25.0).powi(2) <= 110.0 {
                    c = dark;
                }
            }
            let mouth = if open {
                (fx / 28.0).powi(2) + ((fy - 50.0) / 22.0).powi(2) <= 1.0
            } else {
                fx.abs() <= 30.0 && (fy - 50.0).abs() <= 3.0
            };
            if mouth {
                c = dark;
            }
            px[y * w + x] = c;
        }
    }
    ColorImage::new([w, h], px)
}

struct Scene {
    name: &'static str,
    size: [f32; 2],
    status: Status,
    state: ViewState,
    images: bool,
}

fn base(palette: theme::Palette) -> ViewState {
    ViewState {
        palette,
        obs_port: 4455,
        obs_password: String::new(),
        selected: None,
        params: GateParams::default(),
        testing: false,
        slots: Default::default(),
        picking: false,
        server: Ok("http://127.0.0.1:39017/".to_owned()),
        prefs_error: None,
        show_connection: false,
        copied_at: None,
    }
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: render_view <output-dir>"),
    );
    std::fs::create_dir_all(&out).unwrap();
    let palette = theme::load(theme::omarchy_colors_path().as_deref());
    let mic = Uuid::from_u128(1);
    let inputs = vec![
        AudioInput {
            uuid: mic,
            name: "Mic/Aux".to_owned(),
        },
        AudioInput {
            uuid: Uuid::from_u128(2),
            name: "Desktop Audio".to_owned(),
        },
    ];
    let idle = Status {
        connection: Connection::Idle,
        inputs: Vec::new(),
        level_db: f32::NEG_INFINITY,
        mouth: Mouth::Closed,
    };

    let scenes = vec![
        Scene {
            name: "1-first-run",
            size: [620.0, 720.0],
            status: idle.clone(),
            state: ViewState { show_connection: true, ..base(palette) },
            images: false,
        },
        Scene {
            name: "2-talking",
            size: [620.0, 720.0],
            status: Status {
                connection: Connection::Connected,
                inputs: inputs.clone(),
                level_db: -17.0,
                mouth: Mouth::Open,
            },
            state: ViewState { selected: Some(mic), ..base(palette) },
            images: true,
        },
        Scene {
            name: "3-quiet-narrow",
            size: [420.0, 720.0],
            status: Status {
                connection: Connection::Connected,
                inputs,
                level_db: -45.0,
                mouth: Mouth::Closed,
            },
            state: ViewState { selected: Some(mic), ..base(palette) },
            images: true,
        },
        Scene {
            name: "4-errors",
            size: [620.0, 760.0],
            status: Status {
                connection: Connection::Retrying {
                    error: "OBS não respondeu; confira se está aberto e com o servidor WebSocket ativado"
                        .to_owned(),
                    after: Duration::from_secs(4),
                },
                ..idle
            },
            state: ViewState {
                selected: Some(mic),
                show_connection: true,
                server: Err("não foi possível usar a porta 39017: Address in use".to_owned()),
                prefs_error: Some("preferências ignoradas: expected value at line 1".to_owned()),
                slots: [
                    SlotView {
                        error: Some("/home/x/avatar.jpg: o arquivo não é um PNG".to_owned()),
                        ..Default::default()
                    },
                    SlotView::default(),
                ],
                ..base(palette)
            },
            images: false,
        },
    ];

    for scene in scenes {
        let Scene {
            name,
            size,
            status,
            state,
            images,
        } = scene;
        let mut loaded = !images;
        let mut harness = Harness::builder().with_size(size).build_ui_state(
            move |ui, state: &mut ViewState| {
                if !loaded {
                    theme::apply(ui.ctx(), &state.palette);
                    for (i, open) in [(0, false), (1, true)] {
                        let img = face(open);
                        state.slots[i].size = Some([img.size[0] as u32, img.size[1] as u32]);
                        state.slots[i].file = Some(format!("face-{i}.png"));
                        state.slots[i].texture = Some(ui.ctx().load_texture(
                            format!("f{i}"),
                            img,
                            TextureOptions::LINEAR,
                        ));
                    }
                    loaded = true;
                } else {
                    theme::apply(ui.ctx(), &state.palette);
                }
                view::draw(ui, state, &status);
            },
            state,
        );
        harness.run_steps(3);
        let image = harness.render().expect("render");
        let path = out.join(format!("{name}.png"));
        image.save(&path).unwrap();
        println!("{}", path.display());
    }
}
