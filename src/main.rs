use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, TextureOptions};
use omatuber::avatar::{self, LoadError, Png};
use omatuber::example;
use omatuber::library_view::{FileTarget, LibraryView};
use omatuber::mouth::{GateParams, Mouth};
use omatuber::obs::{self, Command, Connection};
use omatuber::prefs::{self, Prefs};
use omatuber::server::{self, Outputs};
use omatuber::theme;
use omatuber::view::{self, Action, FLOOR_DB, HYSTERESIS_DB, Page, SlotView, ViewState};
use omatuber::{cli, library, reactions, secret};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = cli::run(&args, server::DEFAULT_PORT) {
        std::process::exit(code);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OmaTuber")
            .with_app_id("omatuber")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([420.0, 520.0]),
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
    /// Library reactions; the editor will send `Apply` and read its status.
    reactions: reactions::Handle,
    /// Sound detection; dropping it stops the microphone.
    #[cfg(feature = "sound")]
    voice: Option<omatuber::voice::Handle>,
    #[cfg(feature = "sound")]
    teach: Teach,
    view: ViewState,
    /// Configured file per slot, kept even when loading it failed.
    paths: [Option<PathBuf>; 2],
    loaded_tx: Sender<Loaded>,
    loaded_rx: Receiver<Loaded>,
    theme_rx: Receiver<theme::Palette>,
    secret_tx: Sender<Result<SecretDone, String>>,
    secret_rx: Receiver<Result<SecretDone, String>>,
    lib_tx: Sender<LibLoaded>,
    lib_rx: Receiver<LibLoaded>,
    last_connection: Connection,
    prefs_path: Option<PathBuf>,
    /// Last state written to disk; a save happens when the current one differs.
    saved: Prefs,
}

impl App {
    fn new(runtime: tokio::runtime::Runtime, ctx: &egui::Context) -> Self {
        let palette = theme::load(theme::omarchy_colors_path().as_deref());
        theme::apply(ctx, &palette);
        let theme_rx = watch_theme(ctx.clone());

        let outputs = Outputs::default();
        let obs = {
            let _guard = runtime.enter();
            let ctx = ctx.clone();
            obs::spawn(outputs.clone(), move || ctx.request_repaint())
        };
        let (library, library_error) = match library::default_path().as_deref().map(library::load) {
            Some(Ok(lib)) => (lib, None),
            // Start empty and leave the bad file alone; nothing edits it yet.
            Some(Err(e)) => (
                Default::default(),
                Some(format!("Biblioteca ignorada: {e}")),
            ),
            None => (Default::default(), None),
        };
        let reactions = {
            let _guard = runtime.enter();
            let ctx = ctx.clone();
            reactions::spawn(outputs.clone(), library.clone(), move || {
                ctx.request_repaint()
            })
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
        let (lib_tx, lib_rx) = channel();
        let (secret_tx, secret_rx) = channel();
        let mut app = Self {
            runtime,
            outputs,
            obs,
            reactions,
            #[cfg(feature = "sound")]
            voice: None,
            #[cfg(feature = "sound")]
            teach: Teach::default(),
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
                prefs_error: prefs_error.or(library_error),
                // The password is not saved, so every session starts here.
                show_connection: true,
                copied_at: None,
                page: Page::Avatar,
                remember_password: false,
                secret_note: None,
                library: LibraryView {
                    draft: library.clone(),
                    applied: library.clone(),
                    ..Default::default()
                },
            },
            paths: [None, None],
            loaded_tx,
            loaded_rx,
            theme_rx,
            secret_tx,
            secret_rx,
            lib_tx,
            lib_rx,
            last_connection: Connection::Idle,
            prefs_path,
            saved: saved.clone(),
        };
        app.start_server();
        app.view.library.voice = initial_voice_view();
        app.secret_op(SecretOp::Load);
        app.load_library_textures(ctx, &library);
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
        // Plan: animated formats stay out of the main flow until a recording in
        // OBS confirms timing, transparency and restarts. Testers opt in.
        let result = result.and_then(|png| {
            if png.kind.animated() && std::env::var_os("OMATUBER_ANIMATED").is_none() {
                Err(LoadError::AnimationNotEnabled)
            } else {
                Ok(png)
            }
        });
        let slot = &mut self.view.slots[i];
        match result {
            Ok(png) => {
                let size = [png.width as usize, png.height as usize];
                let mut textures = png.frames.iter().enumerate().map(|(f, frame)| {
                    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, &frame.rgba);
                    let name = format!("avatar-{i}-{f}");
                    (
                        ctx.load_texture(name, pixels, TextureOptions::LINEAR),
                        frame.delay,
                    )
                });
                let first = textures.next().map(|(t, _)| t);
                let frames = if png.kind.animated() {
                    first
                        .iter()
                        .cloned()
                        .map(|t| (t, png.frames[0].delay))
                        .chain(textures)
                        .collect()
                } else {
                    Vec::new()
                };
                self.outputs
                    .set_image(mouth, png.bytes.clone(), png.kind.mime());
                *slot = SlotView {
                    file: Some(file_name(&path)),
                    size: Some([png.width, png.height]),
                    texture: first,
                    frames,
                    started: ctx.input(|i| i.time),
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
            Action::Pick(mouth) => self.load_async(ctx, mouth, None),
            Action::Dropped(mouth, path) => self.load_async(ctx, mouth, Some(path)),
            Action::UseExample => self.use_example(ctx),
            Action::RetryServer => self.start_server(),
            Action::PickLibraryFile(target) => {
                let tx = self.lib_tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let picked = rfd::FileDialog::new()
                        .add_filter("PNG", &["png"])
                        .pick_file();
                    if let Some(path) = picked {
                        let _ = tx.send(LibLoaded::Picked(target, path.clone()));
                        let _ = tx.send(LibLoaded::Texture(path.clone(), first_frame(&path)));
                        ctx.request_repaint();
                    }
                });
            }
            Action::ApplyLibrary => self.apply_library(),
            Action::TriggerSentiment(id) => {
                let _ = self.reactions.commands.send(reactions::Command::Trigger {
                    target: id.to_string(),
                    source: library::Source::Manual,
                    reply: None,
                });
            }
            Action::ClearReaction => {
                let _ = self.reactions.commands.send(reactions::Command::Clear);
            }
            Action::ToggleObject(id) => {
                let _ = self
                    .reactions
                    .commands
                    .send(reactions::Command::ToggleObject(id));
            }
            Action::SetVoice(on) => self.set_voice(ctx, on),
            Action::SetRemember(true) => {
                if self.last_connection == Connection::Connected {
                    self.secret_op(SecretOp::Save(self.view.obs_password.clone()));
                } else {
                    self.view.secret_note =
                        Some("A senha será guardada quando a conexão der certo.".to_owned());
                }
            }
            Action::SetRemember(false) => self.secret_op(SecretOp::Forget),
            Action::RecordExample(_)
            | Action::RemoveLastExample(_)
            | Action::TrainSounds
            | Action::AcceptModel
            | Action::DiscardModel => self.teach_action(ctx, action),
        }
    }

    /// Writes the generated sample avatar to the data directory and loads it
    /// like any other pair of PNGs, so it is saved and served the same way.
    fn use_example(&mut self, ctx: &egui::Context) {
        let written = example::default_dir()
            .ok_or_else(|| "pasta de dados desconhecida".to_owned())
            .and_then(|dir| example::write_to(&dir).map_err(|e| e.to_string()));
        match written {
            Ok(paths) => {
                for (mouth, path) in [Mouth::Closed, Mouth::Open].into_iter().zip(paths) {
                    let result = avatar::load(&path, avatar::Limits::default());
                    self.apply_png(ctx, mouth, path, result);
                }
            }
            Err(e) => {
                self.view.slots[0].error = Some(format!("Não foi possível criar o exemplo: {e}"));
            }
        }
    }

    #[cfg(feature = "sound")]
    fn set_voice(&mut self, ctx: &egui::Context, on: bool) {
        use omatuber::{sound, voice};
        let v = &mut self.view.library.voice;
        v.error = None;
        if !on {
            self.voice = None;
            v.running = false;
            return;
        }
        match sound::load_model() {
            Ok(model) => {
                let ctx = ctx.clone();
                let source = std::env::var("OMATUBER_SOURCE").ok();
                let commands = self.reactions.commands.clone();
                self.voice = Some(voice::start(model, source, commands, move || {
                    ctx.request_repaint()
                }));
            }
            Err(e) => {
                v.enabled = false;
                v.error = Some(format!(
                    "Nenhum modelo de sons treinado ({e}). Grave exemplos com `sound-probe record <som>` e treine com `sound-probe train`."
                ));
            }
        }
    }

    #[cfg(not(feature = "sound"))]
    fn set_voice(&mut self, _ctx: &egui::Context, _on: bool) {}

    #[cfg(not(feature = "sound"))]
    fn teach_action(&mut self, _ctx: &egui::Context, _action: Action) {}

    /// Recording and training run on threads; results come back through
    /// `self.teach.rx`. A trained model waits in `pending` until accepted.
    #[cfg(feature = "sound")]
    fn teach_action(&mut self, ctx: &egui::Context, action: Action) {
        use omatuber::{sound, training};
        let data = match sound::data_dir() {
            Ok(d) => d,
            Err(e) => {
                self.view.library.voice.teach_message = Some(e);
                return;
            }
        };
        let v = &mut self.view.library.voice;
        v.teach_message = None;
        let tx = self.teach.tx.clone();
        let repaint = ctx.clone();
        match action {
            Action::RecordExample(class) => {
                v.recording = Some(class.clone());
                let source = std::env::var("OMATUBER_SOURCE").ok();
                std::thread::spawn(move || {
                    let result = training::record(source, std::time::Duration::from_millis(2500))
                        .and_then(|samples| training::save_clip(&data, &class, &samples))
                        .map(|_| format!("Exemplo de {class} salvo."));
                    let _ = tx.send(TeachMsg::Recorded(result));
                    repaint.request_repaint();
                });
            }
            Action::RemoveLastExample(class) => {
                if let Err(e) = training::remove_last(&data, &class) {
                    v.teach_message = Some(e);
                }
                self.refresh_examples();
            }
            Action::TrainSounds => {
                v.training = true;
                v.report = None;
                std::thread::spawn(move || {
                    let _ = tx.send(TeachMsg::Trained(training::train(&data)));
                    repaint.request_repaint();
                });
            }
            Action::AcceptModel => {
                let Some(model) = self.teach.pending.take() else {
                    return;
                };
                v.report = None;
                match training::accept(&data, &model) {
                    Ok(()) => {
                        v.classes = omatuber::voice::reaction_classes(&model);
                        v.teach_message = Some("Modelo novo em uso.".to_owned());
                        // Restart listening so it uses the new model.
                        if self.voice.is_some() {
                            self.set_voice(ctx, false);
                            self.view.library.voice.enabled = true;
                            self.set_voice(ctx, true);
                        }
                    }
                    Err(e) => {
                        v.teach_message = Some(format!("Não foi possível salvar o modelo: {e}"))
                    }
                }
            }
            Action::DiscardModel => {
                self.teach.pending = None;
                v.report = None;
                v.teach_message = Some("Modelo descartado; o anterior continua em uso.".to_owned());
            }
            _ => {}
        }
    }

    #[cfg(feature = "sound")]
    fn receive_teach(&mut self) {
        while let Ok(msg) = self.teach.rx.try_recv() {
            let v = &mut self.view.library.voice;
            match msg {
                TeachMsg::Recorded(result) => {
                    v.recording = None;
                    v.teach_message = Some(result.unwrap_or_else(|e| e));
                    self.refresh_examples();
                }
                TeachMsg::Trained(Ok(report)) => {
                    v.training = false;
                    v.report = Some(omatuber::library_view::TrainingReport {
                        per_class: report
                            .classes
                            .iter()
                            .zip(&report.correct)
                            .zip(&report.clips)
                            .map(|((c, ok), n)| (c.clone(), *ok, *n))
                            .collect(),
                        warnings: report.warnings.clone(),
                    });
                    self.teach.pending = Some(report.model);
                }
                TeachMsg::Trained(Err(e)) => {
                    v.training = false;
                    v.teach_message = Some(e);
                }
            }
        }
    }

    /// Example counts from disk, keeping sounds added in the editor that
    /// have no examples yet.
    #[cfg(feature = "sound")]
    fn refresh_examples(&mut self) {
        let v = &mut self.view.library.voice;
        v.examples = example_counts(&v.examples);
    }

    fn apply_library(&mut self) {
        let mut lib = self.view.library.draft.clone();
        lib.sanitize();
        self.view.library.save_error = library::default_path().and_then(|path| {
            library::save(&path, &lib)
                .err()
                .map(|e| format!("Aplicado, mas não foi possível salvar: {e}"))
        });
        let _ = self
            .reactions
            .commands
            .send(reactions::Command::Apply(lib.clone()));
        self.view.library.applied = lib.clone();
        self.view.library.draft = lib;
    }

    /// Picked files and decoded thumbnails from background threads, and the
    /// reactions task's state.
    fn receive_library(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.lib_rx.try_recv() {
            match msg {
                LibLoaded::Picked(target, path) => self.view.library.set_file(target, path),
                LibLoaded::Texture(path, Ok((image, size))) => {
                    let name = format!("library-{}", path.display());
                    let texture = ctx.load_texture(name, image, TextureOptions::LINEAR);
                    self.view.library.textures.insert(path, (texture, size));
                }
                // The reactions task reports the reason next to the item.
                LibLoaded::Texture(_, Err(_)) => {}
            }
        }
        #[cfg(feature = "sound")]
        self.receive_teach();
        #[cfg(feature = "sound")]
        if let Some(handle) = &self.voice {
            let st = handle.status.borrow().clone();
            let v = &mut self.view.library.voice;
            v.running = st.running;
            v.classes = st.classes;
            v.heard = st.stats.heard;
            if st.error.is_some() && !st.running {
                v.enabled = false;
                v.error = st
                    .error
                    .map(|e| format!("Não foi possível ouvir o microfone: {e}"));
            }
        }
        let status = self.reactions.status.borrow().clone();
        let lib = &mut self.view.library;
        lib.errors = status.errors;
        lib.active = status.active;
        lib.objects_on = status.objects;
    }

    /// Decodes thumbnails for every file of `lib` not loaded yet.
    fn load_library_textures(&self, ctx: &egui::Context, lib: &library::Library) {
        let mut paths: Vec<PathBuf> = lib
            .sentiments
            .iter()
            .flat_map(|s| [s.quiet.clone(), s.talking.clone()])
            .flatten()
            .chain(lib.objects.iter().map(|o| o.file.clone()))
            .filter(|p| !p.as_os_str().is_empty() && !self.view.library.textures.contains_key(p))
            .collect();
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return;
        }
        let tx = self.lib_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for path in paths {
                let _ = tx.send(LibLoaded::Texture(path.clone(), first_frame(&path)));
            }
            ctx.request_repaint();
        });
    }

    /// Opens the connection panel when it needs attention and folds it away
    /// once connected.
    fn follow_connection(&mut self, connection: &Connection) {
        if *connection == self.last_connection {
            return;
        }
        match connection {
            Connection::Connected => {
                self.view.show_connection = false;
                // Stored only after OBS accepted it, so a typo is never kept.
                if self.view.remember_password {
                    self.secret_op(SecretOp::Save(self.view.obs_password.clone()));
                }
            }
            Connection::Failed(_) => self.view.show_connection = true,
            _ => {}
        }
        self.last_connection = connection.clone();
    }

    /// Keyring calls may block on D-Bus or an unlock prompt: run them on a
    /// thread and report back through `secret_rx`.
    fn secret_op(&self, op: SecretOp) {
        let tx = self.secret_tx.clone();
        std::thread::spawn(move || {
            let result = secret::system_store().and_then(|store| match op {
                SecretOp::Load => secret::load(&*store).map(SecretDone::Loaded),
                SecretOp::Save(pw) => secret::save(&*store, &pw).map(|()| SecretDone::Saved),
                SecretOp::Forget => secret::forget(&*store).map(|()| SecretDone::Forgotten),
            });
            let _ = tx.send(result);
        });
    }

    fn receive_secret(&mut self) {
        while let Ok(result) = self.secret_rx.try_recv() {
            let v = &mut self.view;
            match result {
                Ok(SecretDone::Loaded(Some(pw))) => {
                    v.obs_password = pw;
                    v.remember_password = true;
                    v.secret_note = Some("Senha lembrada deste computador.".to_owned());
                    // Plan: try the local OBS by itself when it can.
                    if self.last_connection == Connection::Idle {
                        let _ = self.obs.commands.send(Command::Connect {
                            port: v.obs_port,
                            password: v.obs_password.clone(),
                        });
                    }
                }
                Ok(SecretDone::Loaded(None)) => {}
                Ok(SecretDone::Saved) => {
                    v.secret_note = Some("Senha guardada no chaveiro do sistema.".to_owned());
                }
                Ok(SecretDone::Forgotten) => {
                    v.secret_note = Some("Senha apagada deste computador.".to_owned());
                }
                Err(e) => {
                    v.remember_password = false;
                    v.secret_note = Some(format!(
                        "Não dá para lembrar a senha ({e}); ela será pedida a cada sessão."
                    ));
                }
            }
        }
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

/// Polls the Omarchy theme file once a second and sends a new palette when its
/// contents change, so switching themes restyles the open window. Reading a
/// ~1 KiB file per second is cheaper than wiring up inotify for a directory
/// that Omarchy replaces wholesale on theme change.
fn watch_theme(ctx: egui::Context) -> Receiver<theme::Palette> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let path = theme::omarchy_colors_path();
        let read = || {
            path.as_deref()
                .and_then(|p| std::fs::read_to_string(p).ok())
        };
        let mut last = read();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let now = read();
            if now != last {
                let palette = now
                    .as_deref()
                    .map(theme::parse)
                    .unwrap_or_else(|| theme::load(None));
                if tx.send(palette).is_err() {
                    return; // window closed
                }
                ctx.request_repaint();
                last = now;
            }
        }
    });
    rx
}

/// Whether sound detection is built in and which trained classes exist;
/// detection itself starts off (plan).
fn initial_voice_view() -> omatuber::library_view::VoiceView {
    #[cfg(feature = "sound")]
    {
        let classes = omatuber::sound::load_model()
            .map(|m| omatuber::voice::reaction_classes(&m))
            .unwrap_or_default();
        omatuber::library_view::VoiceView {
            available: true,
            classes,
            examples: example_counts(&[]),
            ..Default::default()
        }
    }
    #[cfg(not(feature = "sound"))]
    omatuber::library_view::VoiceView::default()
}

#[cfg(feature = "sound")]
struct Teach {
    tx: Sender<TeachMsg>,
    rx: Receiver<TeachMsg>,
    /// Trained model awaiting acceptance.
    pending: Option<omatuber::sound::Model>,
}

#[cfg(feature = "sound")]
impl Default for Teach {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            pending: None,
        }
    }
}

#[cfg(feature = "sound")]
enum TeachMsg {
    Recorded(Result<String, String>),
    Trained(Result<omatuber::training::Report, String>),
}

/// Saved examples per sound, plus suggested and editor-added sounds with
/// none yet, in a stable order.
#[cfg(feature = "sound")]
fn example_counts(keep: &[(String, usize)]) -> Vec<(String, usize)> {
    let saved = omatuber::sound::data_dir()
        .map(|d| omatuber::training::clips(&d))
        .unwrap_or_default();
    let mut names: Vec<String> = omatuber::library_view::SUGGESTED_SOUNDS
        .iter()
        .map(|s| s.to_string())
        .chain(keep.iter().map(|(c, _)| c.clone()))
        .chain(saved.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|c| {
            let n = saved.get(&c).map_or(0, Vec::len);
            (c, n)
        })
        .collect()
}

enum SecretOp {
    Load,
    Save(String),
    Forget,
}

enum SecretDone {
    Loaded(Option<String>),
    Saved,
    Forgotten,
}

/// Results from library background threads.
enum LibLoaded {
    Picked(FileTarget, PathBuf),
    Texture(PathBuf, Result<(egui::ColorImage, [u32; 2]), String>),
}

/// First frame of an image file, decoded for a thumbnail.
fn first_frame(path: &Path) -> Result<(egui::ColorImage, [u32; 2]), String> {
    let png = avatar::load(path, avatar::Limits::default()).map_err(|e| e.to_string())?;
    let size = [png.width as usize, png.height as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &png.frames[0].rgba);
    Ok((image, [png.width, png.height]))
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
        self.receive_library(&ctx);
        self.receive_secret();
        if let Some(palette) = self.theme_rx.try_iter().last() {
            theme::apply(&ctx, &palette);
            self.view.palette = palette;
        }
        let status = self.obs.status.borrow().clone();
        self.follow_connection(&status.connection);
        for action in view::draw(ui, &mut self.view, &status) {
            self.handle(&ctx, action);
        }
        self.save_prefs_if_changed(&ctx);
    }
}
