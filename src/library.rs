//! The avatar's library of sentiments and objects, and the rules that decide
//! what is on screen.
//!
//! Vocabulary follows the plan: a *sentiment* is the name of an expression the
//! person chose (e.g. "Animado"), not an inferred emotion; an *object* is a
//! visual layer (e.g. "Caneca"). Items have stable IDs, independent of their
//! display names. Triggers live elsewhere: anything that wants a sentiment on
//! screen calls `Director::trigger` with its `Source`.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::mouth::Mouth;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub sentiments: Vec<Sentiment>,
    pub objects: Vec<Object>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sentiment {
    pub id: Uuid,
    pub name: String,
    /// Shown while quiet; without it the base avatar shows through.
    pub quiet: Option<PathBuf>,
    /// Shown while talking; falls back to `quiet`.
    pub talking: Option<PathBuf>,
    /// Higher wins over a sentiment already on screen; ties go to the newest.
    pub priority: u8,
    pub ending: Ending,
    /// Objects that appear together with this sentiment.
    pub objects: Vec<Uuid>,
    pub enabled: bool,
    /// Trained sound class that triggers it (a sound the person taught, not
    /// an inferred emotion). Missing in libraries saved before this existed.
    #[serde(default)]
    pub sound: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ending {
    /// Returns to the base avatar after this long.
    After { ms: u32 },
    /// Stays until another sentiment or an explicit return to base.
    UntilNext,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Object {
    pub id: Uuid,
    pub name: String,
    pub file: PathBuf,
    /// Center, as fractions of the avatar's width and height.
    pub x: f32,
    pub y: f32,
    /// Width as a fraction of the avatar's width.
    pub scale: f32,
    pub enabled: bool,
}

impl Sentiment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            quiet: None,
            talking: None,
            priority: 1,
            ending: Ending::After { ms: 3000 },
            objects: Vec::new(),
            enabled: true,
            sound: None,
        }
    }
}

impl Object {
    pub fn new(name: impl Into<String>, file: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            file,
            x: 0.5,
            y: 0.5,
            scale: 0.3,
            enabled: true,
        }
    }
}

impl Library {
    pub fn sentiment(&self, id: Uuid) -> Option<&Sentiment> {
        self.sentiments.iter().find(|s| s.id == id && s.enabled)
    }

    /// Highest-priority enabled sentiment bound to a sound class; the first
    /// in library order wins a tie.
    pub fn for_sound(&self, class: &str) -> Option<Uuid> {
        self.sentiments
            .iter()
            .filter(|s| s.enabled && s.sound.as_deref() == Some(class))
            .fold(None::<&Sentiment>, |best, s| match best {
                Some(b) if b.priority >= s.priority => Some(b),
                _ => Some(s),
            })
            .map(|s| s.id)
    }

    pub fn object(&self, id: Uuid) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id && o.enabled)
    }

    /// Clamps placement to sane ranges; called before saving or applying, so
    /// a hand-edited file cannot push a layer far off screen or to zero size.
    pub fn sanitize(&mut self) {
        for o in &mut self.objects {
            o.x = finite_or(o.x, 0.5).clamp(-0.5, 1.5);
            o.y = finite_or(o.y, 0.5).clamp(-0.5, 1.5);
            o.scale = finite_or(o.scale, 0.3).clamp(0.02, 4.0);
        }
        for s in &mut self.sentiments {
            if let Ending::After { ms } = &mut s.ending {
                *ms = (*ms).clamp(100, 600_000);
            }
        }
    }
}

fn finite_or(v: f32, default: f32) -> f32 {
    if v.is_finite() { v } else { default }
}

/// What asked for a sentiment; shown next to it in the UI (plan: "Acionado
/// por voz", "Atalho", "Teste").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Source {
    Manual,
    Shortcut,
    Voice,
    Test,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Active {
    id: Uuid,
    source: Source,
    priority: u8,
    until: Option<Duration>,
}

/// What the preview and every browser source draw. Resolved here, once, so
/// they cannot disagree (plan: priority and return to base "resolvidos antes
/// do envio").
#[derive(Clone, Debug, PartialEq)]
pub struct Composition {
    pub mouth: Mouth,
    /// Sentiment on screen and whether its talking image is the one shown.
    pub sentiment: Option<(Uuid, bool)>,
    pub source: Option<Source>,
    /// Objects in library order, which is their stacking order.
    pub objects: Vec<Uuid>,
}

#[derive(Debug, Default)]
pub struct Director {
    active: Option<Active>,
    toggled: BTreeSet<Uuid>,
}

impl Director {
    /// Puts a sentiment on screen unless a higher-priority one is still
    /// showing. Returns whether it took effect.
    pub fn trigger(&mut self, lib: &Library, id: Uuid, source: Source, now: Duration) -> bool {
        self.expire(now);
        let Some(s) = lib.sentiment(id) else {
            return false;
        };
        if let Some(a) = self.active
            && a.priority > s.priority
        {
            return false;
        }
        let until = match s.ending {
            Ending::After { ms } => Some(now + Duration::from_millis(ms.into())),
            Ending::UntilNext => None,
        };
        self.active = Some(Active {
            id,
            source,
            priority: s.priority,
            until,
        });
        true
    }

    /// Back to the base avatar.
    pub fn clear(&mut self) {
        self.active = None;
    }

    pub fn toggle_object(&mut self, lib: &Library, id: Uuid) {
        if !self.toggled.remove(&id) && lib.object(id).is_some() {
            self.toggled.insert(id);
        }
    }

    fn expire(&mut self, now: Duration) {
        if self
            .active
            .and_then(|a| a.until)
            .is_some_and(|until| now >= until)
        {
            self.active = None;
        }
    }

    /// Sentiment currently on screen and what triggered it. Expiry and
    /// validity are applied by `compose`; call it first.
    pub fn active(&self) -> Option<(Uuid, Source)> {
        self.active.map(|a| (a.id, a.source))
    }

    /// Time until the composition changes by itself, for scheduling a redraw.
    pub fn next_change(&self, now: Duration) -> Option<Duration> {
        self.active?.until.map(|u| u.saturating_sub(now))
    }

    pub fn compose(&mut self, lib: &Library, mouth: Mouth, now: Duration) -> Composition {
        self.expire(now);
        // A sentiment deleted or disabled while showing falls back to base.
        let sentiment = self
            .active
            .and_then(|a| lib.sentiment(a.id).map(|s| (a, s)));
        if sentiment.is_none() {
            self.active = None;
        }
        let shown = sentiment.and_then(|(_, s)| {
            let talking = mouth == Mouth::Open && s.talking.is_some();
            (talking || s.quiet.is_some()).then_some((s.id, talking))
        });
        let with_sentiment: Vec<Uuid> = sentiment
            .map(|(_, s)| s.objects.clone())
            .unwrap_or_default();
        let objects = lib
            .objects
            .iter()
            .filter(|o| {
                o.enabled && (self.toggled.contains(&o.id) || with_sentiment.contains(&o.id))
            })
            .map(|o| o.id)
            .collect();
        Composition {
            mouth,
            sentiment: shown,
            source: sentiment.map(|(a, _)| a.source),
            objects,
        }
    }
}

/// `$XDG_CONFIG_HOME/omatuber/library.json`, next to the preferences.
pub fn default_path() -> Option<PathBuf> {
    Some(crate::prefs::default_path()?.with_file_name("library.json"))
}

/// A missing file is an empty library; an invalid one is an error, so the
/// caller can keep the last valid library instead of overwriting it.
pub fn load(path: &Path) -> io::Result<Library> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut lib: Library = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            lib.sanitize();
            Ok(lib)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Library::default()),
        Err(e) => Err(e),
    }
}

pub fn save(path: &Path, lib: &Library) -> io::Result<()> {
    let mut lib = lib.clone();
    lib.sanitize();
    let json = serde_json::to_vec_pretty(&lib).map_err(io::Error::other)?;
    crate::prefs::write_atomic(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn sentiment(name: &str, priority: u8, ending: Ending) -> Sentiment {
        Sentiment {
            quiet: Some(format!("{name}.png").into()),
            priority,
            ending,
            ..Sentiment::new(name)
        }
    }

    fn lib() -> (Library, Uuid, Uuid, Uuid) {
        let happy = sentiment("Animado", 1, Ending::After { ms: 1000 });
        let angry = sentiment("Irritado", 2, Ending::After { ms: 500 });
        let mug = Object::new("Caneca", "caneca.png".into());
        let ids = (happy.id, angry.id, mug.id);
        (
            Library {
                sentiments: vec![happy, angry],
                objects: vec![mug],
            },
            ids.0,
            ids.1,
            ids.2,
        )
    }

    #[test]
    fn base_when_nothing_triggered() {
        let (lib, ..) = lib();
        let c = Director::default().compose(&lib, Mouth::Open, ms(0));
        assert_eq!(
            c,
            Composition {
                mouth: Mouth::Open,
                sentiment: None,
                source: None,
                objects: vec![]
            }
        );
    }

    #[test]
    fn sentiment_shows_then_returns_to_base() {
        let (lib, happy, ..) = lib();
        let mut d = Director::default();
        assert!(d.trigger(&lib, happy, Source::Shortcut, ms(0)));
        let c = d.compose(&lib, Mouth::Closed, ms(999));
        assert_eq!(
            (c.sentiment, c.source),
            (Some((happy, false)), Some(Source::Shortcut))
        );
        assert_eq!(d.next_change(ms(400)), Some(ms(600)));
        assert_eq!(d.compose(&lib, Mouth::Closed, ms(1000)).sentiment, None);
        assert_eq!(d.next_change(ms(1000)), None);
    }

    #[test]
    fn higher_priority_blocks_lower_until_it_ends() {
        let (lib, happy, angry, _) = lib();
        let mut d = Director::default();
        assert!(d.trigger(&lib, angry, Source::Voice, ms(0)));
        assert!(
            !d.trigger(&lib, happy, Source::Voice, ms(100)),
            "lower priority ignored"
        );
        assert_eq!(
            d.compose(&lib, Mouth::Closed, ms(100)).sentiment,
            Some((angry, false))
        );
        assert!(
            d.trigger(&lib, happy, Source::Voice, ms(500)),
            "allowed once angry expired"
        );
        assert!(
            d.trigger(&lib, angry, Source::Manual, ms(600)),
            "higher replaces lower"
        );
    }

    #[test]
    fn equal_priority_goes_to_the_newest_and_restarts_the_timer() {
        let (lib, happy, ..) = lib();
        let mut d = Director::default();
        d.trigger(&lib, happy, Source::Voice, ms(0));
        assert!(d.trigger(&lib, happy, Source::Shortcut, ms(800)));
        let c = d.compose(&lib, Mouth::Closed, ms(1500));
        assert_eq!(
            (c.sentiment, c.source),
            (Some((happy, false)), Some(Source::Shortcut))
        );
    }

    #[test]
    fn talking_image_used_only_when_present() {
        let (mut lib, happy, ..) = lib();
        let mut d = Director::default();
        d.trigger(&lib, happy, Source::Test, ms(0));
        assert_eq!(
            d.compose(&lib, Mouth::Open, ms(1)).sentiment,
            Some((happy, false))
        );
        lib.sentiments[0].talking = Some("animado-falando.png".into());
        assert_eq!(
            d.compose(&lib, Mouth::Open, ms(2)).sentiment,
            Some((happy, true))
        );
        assert_eq!(
            d.compose(&lib, Mouth::Closed, ms(3)).sentiment,
            Some((happy, false))
        );
    }

    #[test]
    fn sentiment_without_images_lets_the_base_show() {
        let (mut lib, happy, ..) = lib();
        lib.sentiments[0].quiet = None;
        let mut d = Director::default();
        d.trigger(&lib, happy, Source::Manual, ms(0));
        let c = d.compose(&lib, Mouth::Closed, ms(1));
        assert_eq!((c.sentiment, c.source), (None, Some(Source::Manual)));
    }

    #[test]
    fn disabled_or_deleted_while_showing_falls_back_to_base() {
        let (mut lib, happy, ..) = lib();
        let mut d = Director::default();
        d.trigger(&lib, happy, Source::Manual, ms(0));
        lib.sentiments[0].enabled = false;
        assert_eq!(d.compose(&lib, Mouth::Closed, ms(1)).sentiment, None);
        lib.sentiments[0].enabled = true;
        assert_eq!(
            d.compose(&lib, Mouth::Closed, ms(2)).sentiment,
            None,
            "stays cleared"
        );
        assert!(
            !d.trigger(&lib, Uuid::from_u128(42), Source::Manual, ms(3)),
            "unknown id"
        );
    }

    #[test]
    fn objects_from_toggle_and_sentiment_in_library_order() {
        let (mut lib, happy, _, mug) = lib();
        let glasses = Object::new("Óculos", "oculos.png".into());
        let glasses_id = glasses.id;
        lib.objects.insert(0, glasses);
        lib.sentiments[0].objects = vec![mug];
        let mut d = Director::default();
        d.toggle_object(&lib, glasses_id);
        assert_eq!(d.compose(&lib, Mouth::Closed, ms(0)).objects, [glasses_id]);
        d.trigger(&lib, happy, Source::Manual, ms(0));
        assert_eq!(
            d.compose(&lib, Mouth::Closed, ms(1)).objects,
            [glasses_id, mug]
        );
        d.toggle_object(&lib, glasses_id);
        assert_eq!(d.compose(&lib, Mouth::Closed, ms(2)).objects, [mug]);
        assert!(
            d.compose(&lib, Mouth::Closed, ms(1000)).objects.is_empty(),
            "mug left with sentiment"
        );
    }

    #[test]
    fn sound_class_picks_highest_priority_enabled_sentiment() {
        let (mut lib, happy, angry, _) = lib();
        assert_eq!(lib.for_sound("risada"), None);
        lib.sentiments[0].sound = Some("risada".to_owned());
        lib.sentiments[1].sound = Some("risada".to_owned());
        assert_eq!(lib.for_sound("risada"), Some(angry), "priority 2 beats 1");
        lib.sentiments[1].enabled = false;
        assert_eq!(lib.for_sound("risada"), Some(happy));
        assert_eq!(lib.for_sound("grito"), None);
    }

    #[test]
    fn libraries_saved_without_sound_still_load() {
        let json = r#"{"sentiments":[{"id":"00000000-0000-0000-0000-000000000001","name":"A","quiet":null,"talking":null,"priority":1,"ending":"UntilNext","objects":[],"enabled":true}],"objects":[]}"#;
        let lib: Library = serde_json::from_str(json).unwrap();
        assert_eq!(lib.sentiments[0].sound, None);
    }

    #[test]
    fn sanitize_clamps_bad_values() {
        let mut lib = Library::default();
        lib.objects.push(Object {
            x: f32::NAN,
            y: 9.0,
            scale: 0.0,
            ..Object::new("x", "x.png".into())
        });
        lib.sentiments.push(Sentiment {
            ending: Ending::After { ms: 0 },
            ..Sentiment::new("s")
        });
        lib.sanitize();
        let o = &lib.objects[0];
        assert_eq!((o.x, o.y, o.scale), (0.5, 1.5, 0.02));
        assert_eq!(lib.sentiments[0].ending, Ending::After { ms: 100 });
    }

    #[test]
    fn save_and_load_round_trip_and_reject_corruption() {
        let dir = std::env::temp_dir().join(format!("omatuber-lib-{}", std::process::id()));
        let path = dir.join("library.json");
        let (lib, ..) = lib();
        save(&path, &lib).unwrap();
        assert_eq!(load(&path).unwrap(), lib);
        std::fs::write(&path, b"{\"sentiments\": [").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(load(&dir.join("missing.json")).unwrap(), Library::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
