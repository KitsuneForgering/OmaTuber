//! Background task that owns the library and its `Director`, turns the
//! composition into the browser scene and expires reactions on time.
//!
//! Files are validated when a library is applied. An item whose file fails
//! keeps the last valid version in use and reports the error (plan: "Um item
//! ausente ou inválido mantém a última versão válida em uso").

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, sleep_until};
use uuid::Uuid;

use crate::avatar::{self, Limits};
use crate::library::{Director, Library, Source};
use crate::mouth::Mouth;
use crate::server::{Outputs, Scene, SceneObject, SceneSentiment, Served};

pub enum Command {
    /// Sentiment by ID or by display name (case-insensitive). The reply says
    /// whether it took effect (unknown, disabled or outranked: false).
    Trigger {
        target: String,
        source: Source,
        reply: Option<oneshot::Sender<bool>>,
    },
    /// A trained sound class was heard; shows the sentiment bound to it.
    Sound(String),
    /// Back to the base avatar.
    Clear,
    ToggleObject(Uuid),
    /// Replaces the library after an edit; files are (re)validated.
    Apply(Library),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub active: Option<(Uuid, Source)>,
    pub objects: Vec<Uuid>,
    /// Per item, why its file could not be used; the previous version, if
    /// any, stays in use.
    pub errors: HashMap<Uuid, String>,
    /// Triggers so far; a change means the sentiment's image restarted.
    pub triggers: u64,
    /// Libraries applied so far; 0 until the first one is loaded, so
    /// callers can wait for the task to be ready.
    pub generation: u64,
}

pub struct Handle {
    pub commands: mpsc::UnboundedSender<Command>,
    pub status: watch::Receiver<Status>,
}

/// Asset keys for an item: `<id>-q` and `<id>-t` for a sentiment's quiet and
/// talking images, `<id>` for an object.
fn key(id: Uuid, suffix: &str) -> String {
    format!("{id}{suffix}")
}

/// Starts the task on the current runtime and connects the server's
/// `/react` and `/base` endpoints to it.
pub fn spawn(outputs: Outputs, library: Library, notify: impl Fn() + Send + 'static) -> Handle {
    let (commands, rx) = mpsc::unbounded_channel();
    let (status_tx, status) = watch::channel(Status::default());
    outputs.set_reaction_handler(commands.clone());
    tokio::spawn(run(outputs, library, rx, status_tx, notify));
    Handle { commands, status }
}

async fn run(
    outputs: Outputs,
    library: Library,
    mut rx: mpsc::UnboundedReceiver<Command>,
    status: watch::Sender<Status>,
    notify: impl Fn(),
) {
    let start = Instant::now();
    let mut lib = Library::default();
    let mut assets: HashMap<String, Served> = HashMap::new();
    let mut errors = HashMap::new();
    let mut director = Director::default();
    apply(library, &mut lib, &mut assets, &mut errors, &outputs).await;
    // Bumped per applied library, so replaced files reach the page.
    let mut generation: u64 = 1;
    // Silence: time since the mouth last opened, and which silence sentiment
    // already fired in this silent stretch (so one with a duration does not
    // fire again the moment it ends).
    let mut mouth = outputs.watch_mouth();
    let mut last_voice = start.elapsed();
    let mut silence_fired: Option<Uuid> = None;
    loop {
        let now = start.elapsed();
        let silent_for = now.saturating_sub(last_voice);
        if let Some(id) = lib.for_silence(silent_for)
            && silence_fired != Some(id)
        {
            director.compose(&lib, Mouth::Closed, now); // expire first
            let free = match director.active() {
                None => true,
                Some((_, source)) => source == Source::Silence,
            };
            if free && director.trigger(&lib, id, Source::Silence, now) {
                silence_fired = Some(id);
            }
        }
        publish(
            &mut director,
            &lib,
            &assets,
            &errors,
            generation,
            now,
            &outputs,
            &status,
            &notify,
        );
        let next_silence = lib.next_silence(silent_for).map(|t| t - silent_for);
        let wait = match (director.next_change(now), next_silence) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let deadline = wait.map(|d| Instant::now() + d);
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                let now = start.elapsed();
                match cmd {
                    Command::Trigger { target, source, reply } => {
                        let took = resolve(&lib, &target)
                            .is_some_and(|id| director.trigger(&lib, id, source, now));
                        if let Some(reply) = reply {
                            let _ = reply.send(took);
                        }
                    }
                    Command::Sound(class) => {
                        if let Some(id) = lib.for_sound(&class) {
                            director.trigger(&lib, id, Source::Voice, now);
                        }
                    }
                    Command::Clear => director.clear(),
                    Command::ToggleObject(id) => director.toggle_object(&lib, id),
                    Command::Apply(new) => {
                        apply(new, &mut lib, &mut assets, &mut errors, &outputs).await;
                        generation += 1;
                    }
                }
            }
            Ok(()) = mouth.changed() => {
                if *mouth.borrow_and_update() == Mouth::Open {
                    last_voice = start.elapsed();
                    silence_fired = None;
                    // Speaking ends a silence sentiment, never one from a shortcut.
                    if director.active().is_some_and(|(_, source)| source == Source::Silence) {
                        director.clear();
                    }
                }
            }
            _ = sleep_until_opt(deadline) => {}
        }
    }
}

fn resolve(lib: &Library, target: &str) -> Option<Uuid> {
    let by_id = target
        .parse::<Uuid>()
        .ok()
        .filter(|id| lib.sentiment(*id).is_some());
    by_id.or_else(|| {
        lib.sentiments
            .iter()
            .find(|s| s.enabled && s.name.trim().eq_ignore_ascii_case(target.trim()))
            .map(|s| s.id)
    })
}

/// Validates every file of `new` off the async threads and swaps it in.
async fn apply(
    new: Library,
    lib: &mut Library,
    assets: &mut HashMap<String, Served>,
    errors: &mut HashMap<Uuid, String>,
    outputs: &Outputs,
) {
    let mut new = new;
    new.sanitize();
    let mut wanted: Vec<(Uuid, String, std::path::PathBuf)> = Vec::new();
    for s in &new.sentiments {
        wanted.extend(s.quiet.clone().map(|p| (s.id, key(s.id, "-q"), p)));
        wanted.extend(s.talking.clone().map(|p| (s.id, key(s.id, "-t"), p)));
    }
    // An object without a file yet is unfinished, not broken.
    for o in new
        .objects
        .iter()
        .filter(|o| !o.file.as_os_str().is_empty())
    {
        wanted.push((o.id, key(o.id, ""), o.file.clone()));
    }
    let loaded = tokio::task::spawn_blocking(move || {
        wanted
            .into_iter()
            .map(|(id, key, path)| (id, key, load(&path)))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    let mut next = HashMap::new();
    errors.clear();
    for (id, key, result) in loaded {
        match result {
            Ok(served) => {
                next.insert(key, served);
            }
            Err(e) => {
                // Keep the previous version of this file, if there was one.
                if let Some(old) = assets.remove(&key) {
                    next.insert(key, old);
                }
                errors.insert(id, e);
            }
        }
    }
    *assets = next;
    *lib = new;
    outputs.set_assets(assets.clone());
}

fn load(path: &Path) -> Result<Served, String> {
    let png = avatar::load(path, Limits::default()).map_err(|e| e.to_string())?;
    Ok((png.bytes, png.kind.mime()))
}

#[allow(clippy::too_many_arguments)]
fn publish(
    director: &mut Director,
    lib: &Library,
    assets: &HashMap<String, Served>,
    errors: &HashMap<Uuid, String>,
    generation: u64,
    now: Duration,
    outputs: &Outputs,
    status: &watch::Sender<Status>,
    notify: &impl Fn(),
) {
    // compose() expires and validates the active sentiment; the page then
    // picks quiet or talking from the mouth, so both keys go out.
    let c = director.compose(lib, Mouth::Closed, now);
    let available = |k: String| assets.contains_key(&k).then_some(k);
    let sentiment = director.active().and_then(|(id, _)| {
        let quiet = available(key(id, "-q"));
        let talking = available(key(id, "-t"));
        (quiet.is_some() || talking.is_some()).then_some(SceneSentiment {
            quiet,
            talking,
            trigger: director.triggers(),
        })
    });
    let objects = c
        .objects
        .iter()
        .filter_map(|id| {
            let o = lib.object(*id)?;
            Some(SceneObject {
                asset: available(key(o.id, ""))?,
                x: o.x,
                y: o.y,
                scale: o.scale,
            })
        })
        .collect();
    outputs.set_scene(Scene {
        sentiment,
        objects,
        assets: generation,
        ..Scene::default()
    });

    let new = Status {
        active: director.active(),
        objects: c.objects,
        errors: errors.clone(),
        triggers: director.triggers(),
        generation,
    };
    if status.send_if_modified(|s| std::mem::replace(s, new.clone()) != new) {
        notify();
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}
