use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, TextureOptions};
use omatuber::avatar::{self, LoadError, Png};
use omatuber::mouth::{GateParams, Mouth};
use omatuber::obs::{self, Command, Connection};
use omatuber::prefs::{self, Prefs};
use omatuber::server::{self, Outputs};
use omatuber::theme;
use omatuber::view::{self, Action, FLOOR_DB, HYSTERESIS_DB, SlotView, TEST_DB, ViewState};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OmaTuber")
            .with_app_id("omatuber")
            .with_inner_size([640.0, 680.0])
            .with_min_inner_size([400.0, 480.0]),
        ..Default::default()
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            std::process::exit(1);
        }
    };
    eframe::run_native(
        "OmaTuber",
        options,
        Box::new(|cc| Ok(Box::new(App::new(runtime, &cc.egui_ctx)))),
    )
}

/// A finished file load; `None` when the dialog was cancelled.
type Loaded = (Mouth, Option<(PathBuf, Result<Png, LoadError>)>);

struct App {
    runtime: tokio::runtime::Runtime,
    outputs: Outputs,
    obs: obs::Handle,
    view: ViewState,
    /// Configured file per slot, kept even when loading it failed.
    paths: [Option<PathBuf>; 2],
    loaded_tx: Sender<Loaded>,
    loaded_rx: Receiver<Loaded>,
    last_connection: Connection,
    prefs_path: Option<PathBuf>,
    /// Last state written to disk; a save happens when the current one differs.
    saved: Prefs,
}

impl App {
    fn new(runtime: tokio::runtime::Runtime, ctx: &egui::Context) -> Self {
        let palette = theme::load(theme::omarchy_colors_path().as_deref());
        theme::apply(ctx, &palette);

        let outputs = Outputs::default();
        let obs = {
            let _guard = runtime.enter();
            let ctx = ctx.clone();
            obs::spawn(outputs.clone(), move || ctx.request_repaint())
        };

        let prefs_path = prefs::default_path();
        let (saved, prefs_error) = match prefs_path.as_deref().map(prefs::load) {
            Some(Ok(p)) => (p, None),
            // Keep defaults in memory and leave the bad file alone until the
            // user changes a setting.
            Some(Err(e)) => (
                Prefs::default(),
                Some(format!("Preferências ignoradas: {e}")),
            ),
            None => (
                Prefs::default(),
                Some("Pasta de configuração desconhecida; nada será salvo.".to_owned()),
            ),
        };
        let mut params = GateParams::default();
        params.open_db = saved.open_db.clamp(FLOOR_DB + HYSTERESIS_DB, 0.0);
        params.close_db = params.open_db - HYSTERESIS_DB;
        let _ = obs.commands.send(Command::SetParams(params));
        let _ = obs.commands.send(Command::Select(saved.audio_input));

        let (loaded_tx, loaded_rx) = channel();
        let mut app = Self {
            runtime,
            outputs,
            obs,
            view: ViewState {
                palette,
                obs_port: saved.obs_port,
                obs_password: String::new(),
                selected: saved.audio_input,
                params,
                testing: false,
                slots: Default::default(),
                picking: false,
                server: Err(String::new()),
                prefs_error,
                // The password is not saved, so every session starts here.
                show_connection: true,
                copied_at: None,
            },
            paths: [None, None],
            loaded_tx,
            loaded_rx,
            last_connection: Connection::Idle,
            prefs_path,
            saved: saved.clone(),
        };
        app.start_server();
        for (mouth, path) in [
            (Mouth::Closed, saved.closed_png),
            (Mouth::Open, saved.open_png),
        ] {
            if let Some(path) = path {
                let result = avatar::load(&path, avatar::Limits::default());
                app.apply_png(ctx, mouth, path, result);
            }
        }
        app
    }

    fn send(&self, cmd: Command) {
        // Fails only if the worker panicked; the status then stops updating.
        let _ = self.obs.commands.send(cmd);
    }

    /// Binds before reporting a URL, so a busy port is shown as an error
    /// instead of a URL that points at some other program.
    fn start_server(&mut self) {
        let port = server::DEFAULT_PORT;
        self.view.server = match self.runtime.block_on(server::bind(port)) {
            Ok(listener) => {
                let outputs = self.outputs.clone();
                self.runtime.spawn(async move {
                    if let Err(e) = server::serve(listener, outputs).await {
                        eprintln!("error: local server stopped: {e}");
                    }
                });
                Ok(format!("http://127.0.0.1:{port}/"))
            }
            Err(e) => Err(format!("Não foi possível usar a porta {port}: {e}")),
        };
    }

    /// Opens the portal file chooser (when `path` is `None`) and decodes the
    /// file off the UI thread, so the window keeps repainting.
    fn load_async(&mut self, ctx: &egui::Context, mouth: Mouth, path: Option<PathBuf>) {
        self.view.picking = true;
        let tx = self.loaded_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("PNG", &["png"])
                    .pick_file()
            });
            let loaded = path.map(|path| {
                let result = avatar::load(&path, avatar::Limits::default());
                (path, result)
            });
            let _ = tx.send((mouth, loaded));
            ctx.request_repaint();
        });
    }

    fn receive_loaded(&mut self, ctx: &egui::Context) {
        while let Ok((mouth, loaded)) = self.loaded_rx.try_recv() {
            self.view.picking = false;
            if let Some((path, result)) = loaded {
                self.apply_png(ctx, mouth, path, result);
            }
        }
    }

    fn apply_png(
        &mut self,
        ctx: &egui::Context,
        mouth: Mouth,
        path: PathBuf,
        result: Result<Png, LoadError>,
    ) {
        let i = mouth as usize;
        let slot = &mut self.view.slots[i];
        match result {
            Ok(png) => {
                let pixels = egui::ColorImage::from_rgba_unmultiplied(
                    [png.width as usize, png.height as usize],
                    &png.rgba,
                );
                let texture =
                    ctx.load_texture(format!("avatar-{i}"), pixels, TextureOptions::LINEAR);
                self.outputs.set_image(mouth, png.bytes.clone());
                *slot = SlotView {
                    file: Some(file_name(&path)),
                    size: Some([png.width, png.height]),
                    texture: Some(texture),
                    error: None,
                };
                self.paths[i] = Some(path);
            }
            Err(e) => {
                slot.error = Some(format!("{}: {e}", file_name(&path)));
                // Keep a previous good image and its path; a bad pick should
                // not blank the avatar. Without one, remember the path anyway
                // so a file that is temporarily missing at startup is not
                // dropped from the saved preferences.
                if slot.texture.is_none() {
                    self.paths[i] = Some(path);
                }
            }
        }
    }

    fn handle(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Connect => self.send(Command::Connect {
                port: self.view.obs_port,
                password: self.view.obs_password.clone(),
            }),
            Action::Disconnect => self.send(Command::Disconnect),
            Action::Select(uuid) => self.send(Command::Select(uuid)),
            Action::SetParams(params) => self.send(Command::SetParams(params)),
            Action::Test(held) => self.send(Command::Simulate(held.then_some(TEST_DB))),
            Action::Pick(mouth) => self.load_async(ctx, mouth, None),
            Action::Dropped(mouth, path) => self.load_async(ctx, mouth, Some(path)),
            Action::RetryServer => self.start_server(),
        }
    }

    /// Opens the connection panel when it needs attention and folds it away
    /// once connected.
    fn follow_connection(&mut self, connection: &Connection) {
        if *connection == self.last_connection {
            return;
        }
        match connection {
            Connection::Connected => self.view.show_connection = false,
            Connection::Failed(_) => self.view.show_connection = true,
            _ => {}
        }
        self.last_connection = connection.clone();
    }

    fn current_prefs(&self) -> Prefs {
        Prefs {
            obs_port: self.view.obs_port,
            audio_input: self.view.selected,
            open_db: self.view.params.open_db,
            closed_png: self.paths[Mouth::Closed as usize].clone(),
            open_png: self.paths[Mouth::Open as usize].clone(),
        }
    }

    /// Saves when a setting changed, but not mid-drag, to avoid one write
    /// per frame while the threshold moves.
    fn save_prefs_if_changed(&mut self, ctx: &egui::Context) {
        let current = self.current_prefs();
        if current == self.saved || ctx.dragged_id().is_some() {
            return;
        }
        let Some(path) = &self.prefs_path else {
            return;
        };
        self.view.prefs_error = prefs::save(path, &current)
            .err()
            .map(|e| format!("Não foi possível salvar as preferências: {e}"));
        // Record the attempt either way, so a failing disk is not retried every frame.
        self.saved = current;
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.receive_loaded(&ctx);
        let status = self.obs.status.borrow().clone();
        self.follow_connection(&status.connection);
        for action in view::draw(ui, &mut self.view, &status) {
            self.handle(&ctx, action);
        }
        self.save_prefs_if_changed(&ctx);
    }
}
