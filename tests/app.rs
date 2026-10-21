//! The whole app, driven through its window with egui_kittest. Every test
//! gets its own temporary directory, a free server port and no keyring, so
//! nothing on the machine is read or written.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use omatuber::app::{App, Config};

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omatuber-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn config(&self) -> Config {
        Config {
            server_port: 0,
            prefs_path: Some(self.dir.join("prefs.json")),
            library_path: Some(self.dir.join("library.json")),
            example_dir: Some(self.dir.join("example")),
            theme_path: None,
            keyring: false,
        }
    }

    fn harness(&self) -> Harness<'static, App> {
        let config = self.config();
        Harness::builder()
            .with_size([1100.0, 900.0])
            .build_eframe(move |cc| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .unwrap();
                App::new(runtime, &cc.egui_ctx, config)
            })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Steps the app until `done` holds, for work finishing on other threads.
fn step_until(
    h: &mut Harness<'static, App>,
    what: &str,
    done: impl Fn(&mut Harness<'static, App>) -> bool,
) {
    let start = Instant::now();
    while !done(h) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The browser-source URL shown in the scene step.
fn server_url(h: &Harness<'static, App>) -> String {
    let node = h.get_by_label_contains("http://127.0.0.1:");
    let n = node.accesskit_node();
    n.label()
        .or_else(|| n.value())
        .map(|s| s.to_string())
        .expect("URL label")
}

fn http_get(url: &str, path: &str) -> (u16, String) {
    let hostport = url.trim_start_matches("http://").trim_end_matches('/');
    let mut s = TcpStream::connect(hostport).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let head = String::from_utf8_lossy(&buf[..buf.len().min(400)]).to_string();
    let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, head)
}

#[test]
fn example_avatar_is_shown_served_and_remembered() {
    let fx = Fixture::new("example");
    let mut h = fx.harness();
    h.run();
    let url = server_url(&h);
    assert_eq!(http_get(&url, "/closed.png").0, 404, "nothing to serve yet");

    h.get_by_label("Usar exemplo").click();
    step_until(&mut h, "thumbnails", |h| {
        h.query_by_label("Imagem quieto: quieto.png").is_some()
    });

    let (code, head) = http_get(&url, "/closed.png");
    assert_eq!(code, 200);
    assert!(
        head.to_lowercase().contains("content-type: image/png"),
        "{head}"
    );
    assert_eq!(http_get(&url, "/open.png").0, 200);

    // Preferences land in the test directory, with the example's paths.
    step_until(&mut h, "prefs saved", |_| {
        fx.dir.join("prefs.json").exists()
    });
    let prefs = std::fs::read_to_string(fx.dir.join("prefs.json")).unwrap();
    assert!(
        prefs.contains("quieto.png") && prefs.contains("falando.png"),
        "{prefs}"
    );
    assert!(fx.dir.join("example/quieto.png").exists());
}

#[test]
fn applying_a_new_sentiment_writes_the_library() {
    let fx = Fixture::new("library");
    let mut h = fx.harness();
    h.run();
    h.get_by_label("Reações e objetos").click();
    h.run();
    h.get_by_label("Adicionar sentimento").click();
    h.run();
    assert!(
        !fx.dir.join("library.json").exists(),
        "draft only until Aplicar"
    );
    h.get_by_label("Aplicar").click();
    h.run();
    let json = std::fs::read_to_string(fx.dir.join("library.json")).unwrap();
    assert!(json.contains("\"Sentimento 1\""), "{json}");
    // Now it can be shown, and the main page says what is on air.
    h.get_by_label("Mostrar agora").click();
    step_until(&mut h, "on air", |h| {
        h.query_by_label_contains("No ar: Sentimento 1").is_some()
    });
}

#[test]
fn starts_with_defaults_when_files_are_missing_and_never_claims_obs() {
    let fx = Fixture::new("fresh");
    let mut h = fx.harness();
    h.run();
    h.get_by_label("SEM OBS");
    h.get_by_label("Prévia do avatar: Comece pelas imagens");
    assert!(
        !fx.dir.join("prefs.json").exists(),
        "nothing saved before a change"
    );
}

/// The plan's first-use path, through the window, against a fake OBS:
/// sample avatar, password, microphone, voice moving the mouth, and the
/// avatar added to the scene on air, once.
#[test]
fn first_use_from_example_to_scene() {
    let fx = Fixture::new("first-use");
    let obs_rt = tokio::runtime::Runtime::new().unwrap();
    let (obs_port, obs, world) = obs_rt.block_on(common::fake_obs("senha-do-obs"));
    std::fs::write(
        fx.dir.join("prefs.json"),
        format!("{{\"obs_port\": {obs_port}}}"),
    )
    .unwrap();

    let mut h = fx.harness();
    h.run();
    let url = server_url(&h);

    // 1. Avatar
    h.get_by_label("Usar exemplo").click();
    step_until(&mut h, "example", |h| {
        h.query_by_label("Imagem quieto: quieto.png").is_some()
    });

    // 2. OBS
    let password = h.get_by_label("Senha");
    password.focus();
    password.type_text("senha-do-obs");
    h.run();
    h.get_by_label("Conectar").click();
    step_until(&mut h, "connected", |h| {
        h.query_by_label("OBS conectado. Recebendo dados do OBS")
            .is_some()
    });

    // 3. Microphone: the combo shares the "Sua voz" heading as its label.
    let combo = h
        .get_all_by_label("Sua voz")
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::ComboBox)
        .expect("microphone picker");
    combo.click();
    h.run();
    h.get_by_label("Mic/Aux").click();
    h.run();

    // The voice: meters for the microphone, about -10 dBFS.
    let start = Instant::now();
    while h
        .query_by_label("Prévia do avatar: Falando  ·  voz detectada")
        .is_none()
    {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "mouth never opened"
        );
        obs.send(common::Control::Meter(0.3)).unwrap();
        h.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    h.get_by_label("Microfone ativo. Mic/Aux · som recebido agora");

    // 4. Scene
    h.get_by_label("Adicionar à cena Principal").click();
    step_until(&mut h, "visible", |h| {
        h.query_by_label("Visível na cena Principal. Avatar adicionado ao OBS")
            .is_some()
    });
    let w = world.lock().unwrap();
    let ours: Vec<_> = w
        .inputs
        .iter()
        .filter(|i| i.kind == "browser_source")
        .collect();
    assert_eq!(ours.len(), 1, "one browser source");
    assert_eq!(
        ours[0].settings["url"],
        url.as_str(),
        "pointing at this app"
    );
    assert_eq!(
        (
            ours[0].settings["width"].as_u64(),
            ours[0].settings["height"].as_u64()
        ),
        (Some(512), Some(512))
    );
    drop(w);
    drop(obs_rt);
}
