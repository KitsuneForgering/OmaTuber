//! Renders the window offscreen in a few fake states, for visual review
//! without opening a window on the desktop.
//!
//!     cargo run --example render_view -- <output-dir>
//!     OMATUBER_THEME=/usr/share/omarchy/themes/white/colors.toml cargo run --example render_view -- <dir>

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{ColorImage, TextureOptions};
use egui_kittest::Harness;
use omatuber::library::{Ending, Library, Object, Sentiment, Source};
use omatuber::library_view::LibraryView;
use omatuber::mouth::{GateParams, Mouth};
use omatuber::obs::{AudioInput, Connection, Status};
use omatuber::view::{self, Page, SlotView, ViewState};
use omatuber::{example, theme};
use uuid::Uuid;

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
        page: Default::default(),
        library: Default::default(),
    }
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: render_view <output-dir>"),
    );
    std::fs::create_dir_all(&out).unwrap();
    // OMATUBER_THEME=/path/to/colors.toml renders with another theme.
    let theme_path = std::env::var_os("OMATUBER_THEME")
        .map(PathBuf::from)
        .or_else(theme::omarchy_colors_path);
    let palette = theme::load(theme_path.as_deref());
    let mic = Uuid::from_u128(1);
    let inputs = vec![
        AudioInput {
            uuid: mic,
            name: "Yeti Nano".to_owned(),
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
        receiving: false,
    };
    let live = |level_db: f32, mouth: Mouth| Status {
        connection: Connection::Connected,
        inputs: inputs.clone(),
        level_db,
        mouth,
        receiving: true,
    };
    let configured = || ViewState {
        selected: Some(mic),
        ..base(palette)
    };

    let scenes: Vec<Scene> = vec![
        Scene {
            name: "1-first-run",
            size: [1100.0, 760.0],
            status: idle.clone(),
            state: ViewState {
                show_connection: true,
                ..base(palette)
            },
            images: false,
        },
        Scene {
            name: "2-live-talking",
            size: [1100.0, 760.0],
            status: live(-17.0, Mouth::Open),
            state: configured(),
            images: true,
        },
        Scene {
            name: "3-live-quiet",
            size: [1100.0, 760.0],
            status: live(-44.0, Mouth::Closed),
            state: configured(),
            images: true,
        },
        Scene {
            name: "4-no-signal",
            size: [1100.0, 760.0],
            status: Status {
                receiving: false,
                ..live(f32::NEG_INFINITY, Mouth::Closed)
            },
            state: configured(),
            images: true,
        },
        Scene {
            name: "5-narrow",
            size: [520.0, 1000.0],
            status: live(-17.0, Mouth::Open),
            state: configured(),
            images: true,
        },
        Scene {
            name: "6-errors",
            size: [1100.0, 760.0],
            status: Status {
                connection: Connection::Failed("senha do OBS recusada".to_owned()),
                ..idle.clone()
            },
            state: ViewState {
                selected: Some(mic),
                show_connection: true,
                server: Err("Não foi possível usar a porta 39017: Address in use".to_owned()),
                prefs_error: Some("Preferências ignoradas: expected value at line 1".to_owned()),
                slots: [
                    SlotView {
                        error: Some("avatar.jpg: o arquivo não é um PNG".to_owned()),
                        ..Default::default()
                    },
                    SlotView::default(),
                ],
                ..base(palette)
            },
            images: false,
        },
        Scene {
            name: "7-retrying",
            size: [1100.0, 760.0],
            status: Status {
                connection: Connection::Retrying {
                    error: "OBS não respondeu".to_owned(),
                    after: Duration::from_secs(4),
                },
                ..idle
            },
            state: configured(),
            images: true,
        },
    ];

    // Library page: two sentiments (one on air by shortcut) and an object.
    let mut lib = Library::default();
    let mut happy = Sentiment::new("Animado");
    happy.quiet = Some("falando.png".into());
    happy.talking = Some("falando.png".into());
    let mut tired = Sentiment::new("Cansado");
    tired.quiet = Some("quieto.png".into());
    tired.ending = Ending::UntilNext;
    tired.priority = 2;
    let mug = Object::new("Caneca", "quieto.png".into());
    happy.objects = vec![mug.id];
    happy.sound = Some("risada".to_owned());
    let on_air = happy.id;
    lib.sentiments = vec![happy, tired];
    lib.objects = vec![mug];
    let mut draft = lib.clone();
    draft.sentiments[1].name = "Cansado demais".to_owned();
    let mut scenes = scenes;
    scenes.push(Scene {
        name: "8-library",
        size: [1100.0, 1500.0],
        status: live(-40.0, Mouth::Closed),
        state: ViewState {
            page: Page::Library,
            library: LibraryView {
                draft,
                applied: lib,
                active: Some((on_air, Source::Shortcut)),
                voice: omatuber::library_view::VoiceView {
                    available: true,
                    enabled: true,
                    running: true,
                    classes: vec!["risada".to_owned(), "susto".to_owned()],
                    heard: Some(("risada".to_owned(), 0.82)),
                    examples: vec![
                        ("fala".to_owned(), 4),
                        ("outros".to_owned(), 5),
                        ("risada".to_owned(), 4),
                        ("susto".to_owned(), 2),
                    ],
                    report: Some(omatuber::library_view::TrainingReport {
                        per_class: vec![
                            ("fala".to_owned(), 4, 4),
                            ("outros".to_owned(), 4, 5),
                            ("risada".to_owned(), 4, 4),
                            ("susto".to_owned(), 0, 2),
                        ],
                        warnings: vec!["susto: só 2 exemplo(s); grave pelo menos 3".to_owned()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..configured()
        },
        images: true,
    });

    // Main page with that sentiment on air and its object shown.
    let mut applied = scenes.last().unwrap().state.library.applied.clone();
    applied.objects[0].x = 0.78;
    applied.objects[0].y = 0.8;
    applied.objects[0].scale = 0.35;
    let mug_id = applied.objects[0].id;
    scenes.push(Scene {
        name: "9-live-reaction",
        size: [1100.0, 760.0],
        status: live(-17.0, Mouth::Open),
        state: ViewState {
            library: LibraryView {
                draft: applied.clone(),
                applied,
                active: Some((on_air, Source::Shortcut)),
                objects_on: vec![mug_id],
                ..Default::default()
            },
            ..configured()
        },
        images: true,
    });

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
                theme::apply(ui.ctx(), &state.palette);
                if !loaded {
                    for (i, open) in [(0, false), (1, true)] {
                        let img = example::render(open);
                        let size = [img.width() as usize, img.height() as usize];
                        let color = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                        state.slots[i].size = Some([size[0] as u32, size[1] as u32]);
                        state.slots[i].file =
                            Some(if open { "falando.png" } else { "quieto.png" }.to_owned());
                        let tex = ui.ctx().load_texture(
                            format!("lib{i}"),
                            ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
                            TextureOptions::LINEAR,
                        );
                        let file = if open { "falando.png" } else { "quieto.png" };
                        state
                            .library
                            .textures
                            .insert(file.into(), (tex, [size[0] as u32, size[1] as u32]));
                        state.slots[i].texture = Some(ui.ctx().load_texture(
                            format!("f{i}"),
                            color,
                            TextureOptions::LINEAR,
                        ));
                    }
                    loaded = true;
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
