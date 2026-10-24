//! Reconcile the current OBS collection around one shared OmaTuber input.
//! Old items are hidden, never deleted. Other sources are not modified.
use crate::scene::{self, BrowserSettings};
use obws::{
    Client,
    requests::{
        inputs::{Create, InputId, SetSettings},
        scene_items::{
            CreateSceneItem, SetBlendMode, SetEnabled, SetIndex, SetLocked, SetTransform,
        },
        scenes::SceneId,
        sources::SourceId,
    },
    responses::scene_items::SceneItem,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub struct Report {
    pub source: String,
    pub total: usize,
    pub configured: usize,
    pub created_items: usize,
    pub hidden_items: usize,
    pub errors: Vec<String>,
}
struct Container {
    name: String,
    items: Vec<SceneItem>,
}

/// `kind` is explicit so a migration can be tested without a GPU or process
/// environment changes. The UI uses the backend selected for this session.
pub async fn configure(
    client: &Client,
    settings: &BrowserSettings,
    kind: &str,
) -> Result<Report, String> {
    if !matches!(kind, scene::KIND | scene::NATIVE_KIND) {
        return Err("Backend OmaTuber desconhecido".into());
    }
    if !client
        .inputs()
        .list_kinds(false)
        .await
        .map_err(|e| e.to_string())?
        .iter()
        .any(|k| k == kind)
    {
        return Err(format!(
            "O OBS não carregou a fonte {kind}. Instale o plugin e reinicie o OBS."
        ));
    }
    let scenes = scene::scenes(client)
        .await
        .map_err(|e| e.to_string())?
        .names;
    if scenes.is_empty() {
        return Err("O OBS não informou nenhuma cena".into());
    }
    let inputs = client
        .inputs()
        .list(None)
        .await
        .map_err(|e| e.to_string())?;
    let mut owned = HashSet::new();
    let mut candidates = Vec::new();
    for input in &inputs {
        let ours = if input.kind == scene::NATIVE_KIND {
            true
        } else if input.kind == scene::KIND {
            let current = client
                .inputs()
                .settings::<serde_json::Value>(InputId::Uuid(input.id.uuid))
                .await
                .map_err(|e| e.to_string())?;
            current.settings.get("url").and_then(|v| v.as_str()) == Some(settings.url.as_str())
        } else {
            false
        };
        if ours {
            owned.insert(input.id.name.clone());
            if input.kind == kind {
                candidates.push(input.id.name.clone());
            }
        }
    }
    candidates.sort();
    // Snapshot every container before making changes. Groups are listed once
    // even when shared by several scenes; reachability handles nested scenes.
    let mut containers = Vec::new();
    for name in &scenes {
        containers.push(Container {
            name: name.clone(),
            items: client
                .scene_items()
                .list(SceneId::Name(name))
                .await
                .map_err(|e| e.to_string())?,
        });
    }
    for name in client
        .scenes()
        .list_groups()
        .await
        .map_err(|e| e.to_string())?
    {
        containers.push(Container {
            items: client
                .scene_items()
                .list_group(SceneId::Name(&name))
                .await
                .map_err(|e| e.to_string())?,
            name,
        });
    }
    let graph: HashMap<_, Vec<_>> = containers
        .iter()
        .map(|c| {
            (
                c.name.clone(),
                c.items
                    .iter()
                    .filter(|i| i.source_type == obws::responses::scene_items::SourceType::Scene)
                    .map(|i| i.source_name.clone())
                    .collect(),
            )
        })
        .collect();
    let mut planned: HashSet<String> = containers
        .iter()
        .filter(|c| c.items.iter().any(|i| owned.contains(&i.source_name)))
        .map(|c| c.name.clone())
        .collect();
    // Visit dependencies first: a scene containing another scene with the
    // avatar must not get an extra copy on top of it.
    for name in postorder(&scenes, &graph) {
        if scenes.contains(&name) && !covered(&name, &graph, &planned) {
            planned.insert(name);
        }
    }
    let mut report = Report {
        total: scenes.len(),
        ..Default::default()
    };
    let mut seed = None;
    let source = if let Some(name) = candidates.first() {
        name.clone()
    } else {
        let taken: HashSet<_> = inputs.iter().map(|i| i.id.name.as_str()).collect();
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    scene::NAME.into()
                } else {
                    format!("{} {n}", scene::NAME)
                }
            })
            .find(|n: &String| !taken.contains(n.as_str()))
            .unwrap();
        let id = client
            .inputs()
            .create(Create {
                scene: SceneId::Name(&scenes[0]),
                input: &name,
                kind,
                settings: Some(settings),
                enabled: Some(false),
            })
            .await
            .map_err(|e| e.to_string())?;
        seed = Some((scenes[0].clone(), id.scene_item_id));
        name
    };
    report.source = source.clone();
    if let Err(error) = client
        .inputs()
        .set_settings(SetSettings {
            input: InputId::Name(&source),
            settings,
            overlay: Some(true),
        })
        .await
    {
        let mut message = error.to_string();
        if let Some((name, id)) = &seed
            && let Err(cleanup) = client.scene_items().remove(SceneId::Name(name), *id).await
        {
            message.push_str(&format!("; limpar vínculo temporário: {cleanup}"));
        }
        return Err(message);
    }
    let mut finished = HashSet::new();
    let mut seed_used = false;
    for c in &containers {
        if !planned.contains(&c.name) {
            continue;
        }
        let seed_id = seed
            .as_ref()
            .filter(|(name, _)| name == &c.name)
            .map(|(_, id)| *id);
        match reconcile(client, c, &source, &owned, seed_id).await {
            Ok((created, hidden)) => {
                finished.insert(c.name.clone());
                report.created_items += usize::from(created);
                report.hidden_items += hidden;
                if seed_id.is_some() {
                    seed_used = true;
                }
            }
            Err(error) => report.errors.push(format!("{}: {error}", c.name)),
        }
    }
    // A temporary scene item anchors a newly created input until the final
    // links exist. Do not leave that extra copy in an ancestor scene.
    if let Some((name, id)) = seed
        && !seed_used
        && let Err(e) = client.scene_items().remove(SceneId::Name(&name), id).await
    {
        report.errors.push(format!(
            "Não foi possível remover o vínculo temporário de {name}: {e}"
        ));
    }
    report.configured = scenes
        .iter()
        .filter(|name| covered(name, &graph, &finished))
        .count();
    Ok(report)
}

fn covered(root: &str, graph: &HashMap<String, Vec<String>>, local: &HashSet<String>) -> bool {
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(name) = stack.pop() {
        if !seen.insert(name) {
            continue;
        }
        if local.contains(name) {
            return true;
        }
        if let Some(children) = graph.get(name) {
            stack.extend(children.iter().map(String::as_str));
        }
    }
    false
}
fn postorder(roots: &[String], graph: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let mut stack = vec![(root.as_str(), false)];
        while let Some((name, expanded)) = stack.pop() {
            if expanded {
                out.push(name.to_owned());
                continue;
            }
            if !seen.insert(name) {
                continue;
            }
            stack.push((name, true));
            if let Some(children) = graph.get(name) {
                stack.extend(children.iter().map(|n| (n.as_str(), false)));
            }
        }
    }
    out
}

async fn reconcile(
    client: &Client,
    c: &Container,
    source: &str,
    owned: &HashSet<String>,
    seed: Option<i64>,
) -> Result<(bool, usize), String> {
    let old: Vec<_> = c
        .items
        .iter()
        .filter(|i| owned.contains(&i.source_name))
        .collect();
    let mut states = Vec::new();
    for item in &old {
        let enabled = client
            .scene_items()
            .enabled(SceneId::Name(&c.name), item.id)
            .await
            .map_err(|e| e.to_string())?;
        states.push((item.id, enabled));
    }
    // A hidden placeholder from an interrupted run must not hide a legacy
    // avatar that is still visible. Prefer a visible canonical item, otherwise
    // migrate that visible legacy placement into a fresh link.
    let existing = old
        .iter()
        .find(|i| i.source_name == source && states.iter().any(|(id, on)| *id == i.id && *on))
        .copied()
        .or_else(|| {
            (!states.iter().any(|(_, on)| *on))
                .then(|| old.iter().find(|i| i.source_name == source).copied())
                .flatten()
        });
    let template = existing
        .or_else(|| {
            old.iter()
                .find(|i| states.iter().any(|(id, on)| *id == i.id && *on))
                .copied()
        })
        .or_else(|| old.first().copied());
    // Read everything before creating a replacement. Older obws cannot write
    // cropToBounds; fail without changing that item rather than lose its crop.
    let properties = if existing.is_none() {
        if let Some(item) = template {
            let transform = client
                .scene_items()
                .transform(SceneId::Name(&c.name), item.id)
                .await
                .map_err(|e| e.to_string())?;
            if transform.crop_to_bounds {
                return Err(
                    "Crop to bounds exige migração manual; a fonte original foi preservada".into(),
                );
            }
            let locked = client
                .scene_items()
                .locked(SceneId::Name(&c.name), item.id)
                .await
                .map_err(|e| e.to_string())?;
            let blend = client
                .scene_items()
                .blend_mode(SceneId::Name(&c.name), item.id)
                .await
                .map_err(|e| e.to_string())?;
            Some((transform, locked, blend, item.index))
        } else {
            None
        }
    } else {
        None
    };
    let created = existing.is_none();
    let id = if let Some(item) = existing {
        item.id
    } else if let Some(id) = seed {
        id
    } else {
        client
            .scene_items()
            .create(CreateSceneItem {
                scene: SceneId::Name(&c.name),
                source: SourceId::Name(source),
                enabled: Some(false),
            })
            .await
            .map_err(|e| e.to_string())?
    };
    let mut disabled = Vec::new();
    let result = async {
        if let Some((transform, locked, mode, index)) = properties {
            client
                .scene_items()
                .set_transform(SetTransform {
                    scene: SceneId::Name(&c.name),
                    item_id: id,
                    transform: transform.into(),
                })
                .await?;
            client
                .scene_items()
                .set_locked(SetLocked {
                    scene: SceneId::Name(&c.name),
                    item_id: id,
                    locked,
                })
                .await?;
            client
                .scene_items()
                .set_blend_mode(SetBlendMode {
                    scene: SceneId::Name(&c.name),
                    item_id: id,
                    mode,
                })
                .await?;
            client
                .scene_items()
                .set_index(SetIndex {
                    scene: SceneId::Name(&c.name),
                    item_id: id,
                    index,
                })
                .await?;
        }
        if created {
            let enabled =
                template.is_none_or(|t| states.iter().any(|(item, on)| *item == t.id && *on));
            client
                .scene_items()
                .set_enabled(SetEnabled {
                    scene: SceneId::Name(&c.name),
                    item_id: id,
                    enabled,
                })
                .await?;
        }
        for (old_id, was_enabled) in &states {
            if *old_id != id && *was_enabled {
                client
                    .scene_items()
                    .set_enabled(SetEnabled {
                        scene: SceneId::Name(&c.name),
                        item_id: *old_id,
                        enabled: false,
                    })
                    .await?;
                disabled.push(*old_id);
            }
        }
        Ok::<_, obws::error::Error>(())
    }
    .await;
    if let Err(error) = result {
        let mut failures = vec![error.to_string()];
        for old_id in disabled {
            if let Err(e) = client
                .scene_items()
                .set_enabled(SetEnabled {
                    scene: SceneId::Name(&c.name),
                    item_id: old_id,
                    enabled: true,
                })
                .await
            {
                failures.push(format!("restaurar item {old_id}: {e}"));
            }
        }
        if created {
            let cleanup = if seed.is_some() {
                client
                    .scene_items()
                    .set_enabled(SetEnabled {
                        scene: SceneId::Name(&c.name),
                        item_id: id,
                        enabled: false,
                    })
                    .await
            } else {
                client
                    .scene_items()
                    .remove(SceneId::Name(&c.name), id)
                    .await
            };
            if let Err(e) = cleanup {
                failures.push(format!("limpar substituição {id}: {e}"));
            }
        }
        return Err(failures.join("; "));
    }
    Ok((created, disabled.len()))
}
