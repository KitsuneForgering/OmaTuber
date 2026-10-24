//! "Adicionar avatar ao OBS": find, create or reuse the browser source that
//! shows the avatar, and report whether it is visible in a scene.
//!
//! Our sources are recognised by their URL setting, not by name, so renaming
//! one in OBS does not lead to a duplicate. Setting names come from the
//! obs-browser plugin (`obs-browser-plugin.cpp`: id `browser_source`, `url`,
//! `width`, `height`).

use obws::Client;
use obws::requests::inputs::{Create, InputId};
use obws::requests::scene_items::{CreateSceneItem, SetEnabled};
use obws::requests::scenes::SceneId;
use obws::requests::sources::SourceId;
use serde::{Deserialize, Serialize};

pub const KIND: &str = "browser_source";
pub const NATIVE_KIND: &str = "omatuber_dmabuf";
pub const NAME: &str = "OmaTuber";

pub(crate) fn source_kind() -> &'static str {
    #[cfg(feature = "dmabuf")]
    if crate::dmabuf::enabled() {
        return NATIVE_KIND;
    }
    KIND
}
/// The browser source never needs to be taller than a 1080p canvas.
pub const MAX_HEIGHT: u32 = 1080;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserSettings {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// Where our source stands in one scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    NotAdded,
    Visible,
    Hidden,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Added {
    /// A new browser source was created in the scene.
    Created { name: String },
    /// Our existing source was added to this scene too.
    Linked { name: String },
    /// It was already in the scene; made visible if it was hidden.
    Reused { name: String, was_hidden: bool },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scenes {
    pub names: Vec<String>,
    /// Scene on air, if OBS reported one.
    pub program: Option<String>,
}

/// Browser source size for an avatar: its own size, scaled down to fit
/// `MAX_HEIGHT` with the aspect kept.
pub fn source_size(width: u32, height: u32) -> (u32, u32) {
    if height <= MAX_HEIGHT || height == 0 {
        return (width.max(1), height.max(1));
    }
    let w = (u64::from(width) * u64::from(MAX_HEIGHT) / u64::from(height)) as u32;
    (w.max(1), MAX_HEIGHT)
}

pub async fn scenes(client: &Client) -> obws::error::Result<Scenes> {
    let list = client.scenes().list().await?;
    let mut scenes: Vec<_> = list.scenes;
    // OBS reports scenes bottom-up; show them in the order of its scene list.
    scenes.sort_by_key(|s| std::cmp::Reverse(s.index));
    Ok(Scenes {
        names: scenes.into_iter().map(|s| s.id.name).collect(),
        program: list.current_program_scene.map(|s| s.name),
    })
}

/// Names of browser sources showing `url`.
pub async fn ours(client: &Client, url: &str) -> obws::error::Result<Vec<String>> {
    let mut found = Vec::new();
    for input in client.inputs().list(Some(source_kind())).await? {
        // Native sources share this user's one endpoint, including ones made
        // by hand without a URL marker.
        if input.kind == NATIVE_KIND {
            found.push(input.id.name);
            continue;
        }
        let settings = client
            .inputs()
            .settings::<serde_json::Value>(InputId::Uuid(input.id.uuid))
            .await?;
        if settings.settings.get("url").and_then(|u| u.as_str()) == Some(url) {
            found.push(input.id.name);
        }
    }
    Ok(found)
}

struct Found {
    name: String,
    // Ancestor groups/scenes followed by the avatar item itself.
    path: Vec<(String, i64)>,
    visible: bool,
}

async fn item_in(
    client: &Client,
    scene: &str,
    ours: &[String],
) -> obws::error::Result<Option<Found>> {
    let mut pending = vec![(scene.to_owned(), false, Vec::new(), true)];
    let mut seen = std::collections::HashSet::new();
    let mut hidden = None;
    while let Some((container, group, path, visible)) = pending.pop() {
        // The same nested scene can occur through both hidden and visible paths.
        if !seen.insert((container.clone(), visible)) {
            continue;
        }
        let items = if group {
            client
                .scene_items()
                .list_group(SceneId::Name(&container))
                .await?
        } else {
            client.scene_items().list(SceneId::Name(&container)).await?
        };
        for item in items {
            let ours = ours.contains(&item.source_name);
            let nested = item.source_type == obws::responses::scene_items::SourceType::Scene;
            if !ours && !nested {
                continue;
            }
            let enabled = client
                .scene_items()
                .enabled(SceneId::Name(&container), item.id)
                .await?;
            let mut path = path.clone();
            path.push((container.clone(), item.id));
            if ours {
                let found = Found {
                    name: item.source_name,
                    path,
                    visible: visible && enabled,
                };
                if found.visible {
                    return Ok(Some(found));
                }
                hidden.get_or_insert(found);
            } else {
                pending.push((
                    item.source_name,
                    item.is_group.unwrap_or(false),
                    path,
                    visible && enabled,
                ));
            }
        }
    }
    Ok(hidden)
}

pub async fn placement(client: &Client, scene: &str, url: &str) -> obws::error::Result<Placement> {
    let ours = ours(client, url).await?;
    Ok(match item_in(client, scene, &ours).await? {
        None => Placement::NotAdded,
        Some(found) if found.visible => Placement::Visible,
        Some(_) => Placement::Hidden,
    })
}

/// Puts the avatar in `scene`, never creating a second source for the same
/// URL.
pub async fn add(
    client: &Client,
    scene: &str,
    settings: &BrowserSettings,
) -> obws::error::Result<Added> {
    let ours = ours(client, &settings.url).await?;
    if let Some(found) = item_in(client, scene, &ours).await? {
        if !found.visible {
            // Explicit "show" also opens hidden ancestor containers; never
            // create a second avatar because the existing one is nested.
            for (container, id) in &found.path {
                if !client
                    .scene_items()
                    .enabled(SceneId::Name(container), *id)
                    .await?
                {
                    client
                        .scene_items()
                        .set_enabled(SetEnabled {
                            scene: SceneId::Name(container),
                            item_id: *id,
                            enabled: true,
                        })
                        .await?;
                }
            }
        }
        return Ok(Added::Reused {
            name: found.name,
            was_hidden: !found.visible,
        });
    }
    if let Some(name) = ours.into_iter().next() {
        client
            .scene_items()
            .create(CreateSceneItem {
                scene: SceneId::Name(scene),
                source: SourceId::Name(&name),
                enabled: Some(true),
            })
            .await?;
        return Ok(Added::Linked { name });
    }
    let name = free_name(client).await?;
    client
        .inputs()
        .create(Create {
            scene: SceneId::Name(scene),
            input: &name,
            kind: source_kind(),
            settings: Some(settings),
            enabled: Some(true),
        })
        .await?;
    Ok(Added::Created { name })
}

/// "OmaTuber", or "OmaTuber 2", … if another input already uses the name.
async fn free_name(client: &Client) -> obws::error::Result<String> {
    let taken: Vec<String> = client
        .inputs()
        .list(None)
        .await?
        .into_iter()
        .map(|i| i.id.name)
        .collect();
    Ok((1..)
        .map(|n| {
            if n == 1 {
                NAME.to_owned()
            } else {
                format!("{NAME} {n}")
            }
        })
        .find(|n| !taken.contains(n))
        .expect("unbounded"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_size_keeps_aspect_within_1080() {
        assert_eq!(source_size(512, 512), (512, 512));
        assert_eq!(source_size(2048, 4096), (540, 1080));
        assert_eq!(source_size(0, 0), (1, 1));
    }
}
