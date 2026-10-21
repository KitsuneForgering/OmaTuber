//! Non-secret preferences, stored as JSON in the user's config directory.
//! The OBS password is deliberately not part of this file.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub obs_port: u16,
    /// Selected audio input; a UUID survives renames in OBS.
    pub audio_input: Option<Uuid>,
    pub open_db: f32,
    pub closed_png: Option<PathBuf>,
    pub open_png: Option<PathBuf>,
    /// Scene to add the avatar to; `None` is the one on air.
    pub scene: Option<String>,
    /// Offset of the open image on the shared canvas (`layout.rs`).
    pub open_offset: crate::layout::Offset,
    /// Optional eyes-closed versions of the two base images.
    pub blink_closed_png: Option<PathBuf>,
    pub blink_open_png: Option<PathBuf>,
    /// Bounce when the mouth opens, as a fraction of the avatar height
    /// (`layout.rs`); 0 is off.
    pub bounce: f32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            obs_port: 4455,
            audio_input: None,
            open_db: crate::mouth::GateParams::default().open_db,
            closed_png: None,
            open_png: None,
            scene: None,
            open_offset: [0, 0],
            blink_closed_png: None,
            blink_open_png: None,
            bounce: 0.0,
        }
    }
}

/// `$XDG_CONFIG_HOME/omatuber/prefs.json`, falling back to `~/.config`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omatuber").join("prefs.json"))
}

/// A missing file gives the defaults; an unreadable or invalid one is an error
/// so the caller can report it instead of silently overwriting it.
pub fn load(path: &Path) -> io::Result<Prefs> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Prefs::default()),
        Err(e) => Err(e),
    }
}

pub fn save(path: &Path, prefs: &Prefs) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(prefs).map_err(io::Error::other)?;
    write_atomic(path, &json)
}

/// Writes to a temporary file in the same directory, syncs it and renames it
/// over the target, so an interrupted save leaves either the old or the new
/// file, never a truncated one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    // Persist the rename itself.
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omatuber-prefs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        assert_eq!(load(&dir.join("prefs.json")).unwrap(), Prefs::default());
    }

    #[test]
    fn round_trip_creates_directory_and_leaves_no_temp_file() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("nested").join("prefs.json");
        let prefs = Prefs {
            obs_port: 4460,
            audio_input: Some(Uuid::from_u128(7)),
            open_db: -27.0,
            closed_png: Some("/a/closed.png".into()),
            open_png: None,
            scene: Some("Principal".to_owned()),
            open_offset: [4, -2],
            blink_closed_png: None,
            blink_open_png: Some("/a/blink-open.png".into()),
            bounce: 0.08,
        };
        save(&path, &prefs).unwrap();
        assert_eq!(load(&path).unwrap(), prefs);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["prefs.json"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_fields_take_defaults() {
        let dir = temp_dir("partial");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prefs.json");
        fs::write(&path, r#"{"obs_port": 4500}"#).unwrap();
        let prefs = load(&path).unwrap();
        assert_eq!(prefs.obs_port, 4500);
        assert_eq!(prefs.open_db, Prefs::default().open_db);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_file_is_an_error_not_defaults() {
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prefs.json");
        fs::write(&path, b"{\"obs_port\": 44").unwrap();
        assert!(load(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn overwrite_replaces_previous_contents() {
        let dir = temp_dir("overwrite");
        let path = dir.join("prefs.json");
        save(
            &path,
            &Prefs {
                obs_port: 1,
                ..Prefs::default()
            },
        )
        .unwrap();
        save(
            &path,
            &Prefs {
                obs_port: 2,
                ..Prefs::default()
            },
        )
        .unwrap();
        assert_eq!(load(&path).unwrap().obs_port, 2);
        fs::remove_dir_all(&dir).unwrap();
    }
}
