mod common;
use common::{Input, URL, World, fake_obs};
use omatuber::{
    scene::{self, BrowserSettings},
    scene_sync,
};
use serde_json::json;
use uuid::Uuid;
async fn client(port: u16) -> obws::Client {
    obws::Client::connect("127.0.0.1", port, Some("certa"))
        .await
        .unwrap()
}
fn settings() -> BrowserSettings {
    BrowserSettings {
        url: URL.into(),
        width: 512,
        height: 512,
    }
}
fn input(w: &mut World, name: &str, kind: &str, settings: serde_json::Value) {
    w.inputs.push(Input {
        name: name.into(),
        uuid: Uuid::new_v4(),
        kind: kind.into(),
        settings,
    });
}
fn item(w: &mut World, scene: &str, name: &str, enabled: bool) -> i64 {
    let id = w.next_item;
    w.next_item += 1;
    w.items.push((scene.into(), id, name.into(), enabled));
    id
}
#[tokio::test]
async fn shares_one_native_source_across_all_scenes_and_is_idempotent() {
    let (port, _obs, world) = fake_obs("certa").await;
    let c = client(port).await;
    let report = scene_sync::configure(&c, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert_eq!(report.configured, 2);
    assert!(report.errors.is_empty());
    let initial = world.lock().unwrap().items.clone();
    let again = scene_sync::configure(&c, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert_eq!(again.created_items, 0);
    assert_eq!(again.configured, 2);
    let w = world.lock().unwrap();
    assert_eq!(
        w.inputs
            .iter()
            .filter(|i| i.kind == scene::NATIVE_KIND)
            .count(),
        1
    );
    assert_eq!(w.items, initial);
}
#[tokio::test]
async fn migrates_legacy_layout_and_visibility_and_keeps_other_inputs() {
    let (port, _obs, world) = fake_obs("certa").await;
    let (old, unrelated) = {
        let mut w = world.lock().unwrap();
        input(
            &mut w,
            "Avatar antigo renomeado",
            scene::KIND,
            json!({"url": URL}),
        );
        input(
            &mut w,
            "Outro site",
            scene::KIND,
            json!({"url": "https://example.com"}),
        );
        let old = item(&mut w, "Principal", "Avatar antigo renomeado", true);
        let unrelated = item(&mut w, "Principal", "Outro site", true);
        let mut transform = common::default_transform();
        transform["positionX"] = json!(281.0);
        transform["scaleX"] = json!(0.42);
        transform["cropLeft"] = json!(12);
        transform["rotation"] = json!(20.0);
        w.transforms.insert(("Principal".into(), old), transform);
        w.locks.insert(("Principal".into(), old), true);
        (old, unrelated)
    };
    let report = scene_sync::configure(&client(port).await, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let w = world.lock().unwrap();
    assert!(
        !w.items.iter().find(|i| i.1 == old).unwrap().3,
        "legacy kept hidden"
    );
    assert!(w.items.iter().find(|i| i.1 == unrelated).unwrap().3);
    let new = w
        .items
        .iter()
        .find(|i| i.0 == "Principal" && i.2 == report.source)
        .unwrap();
    assert!(new.3);
    let transform = &w.transforms[&("Principal".into(), new.1)];
    assert_eq!(transform["positionX"], 281.0);
    assert!((transform["scaleX"].as_f64().unwrap() - 0.42).abs() < 1e-6);
    assert_eq!(transform["cropLeft"], 12);
    assert_eq!(transform["rotation"], 20.0);
    assert!(w.locks[&("Principal".into(), new.1)]);
}
#[tokio::test]
async fn recognises_manual_native_source_and_disables_duplicate_consumers() {
    let (port, _obs, world) = fake_obs("certa").await;
    {
        let mut w = world.lock().unwrap();
        input(&mut w, "Meu avatar", scene::NATIVE_KIND, json!({}));
        input(&mut w, "Outro avatar", scene::NATIVE_KIND, json!({}));
        item(&mut w, "Principal", "Meu avatar", true);
        item(&mut w, "Principal", "Outro avatar", true);
    }
    let report = scene_sync::configure(&client(port).await, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert_eq!(report.source, "Meu avatar");
    let w = world.lock().unwrap();
    assert_eq!(
        w.inputs
            .iter()
            .filter(|i| i.kind == scene::NATIVE_KIND)
            .count(),
        2,
        "old sources are retained"
    );
    assert!(!w.items.iter().find(|i| i.2 == "Outro avatar").unwrap().3);
    assert_eq!(
        w.items
            .iter()
            .filter(|i| i.2 == "Meu avatar" && i.3)
            .count(),
        2
    );
}
#[tokio::test]
async fn group_and_nested_scenes_do_not_gain_extra_visible_copies() {
    let (port, _obs, world) = fake_obs("certa").await;
    {
        let mut w = world.lock().unwrap();
        w.groups.push("Personagem".into());
        input(&mut w, "Antigo", scene::KIND, json!({"url": URL}));
        item(&mut w, "Principal", "Personagem", true);
        item(&mut w, "Personagem", "Antigo", false);
        item(&mut w, "Intervalo", "Principal", true);
    }
    let c = client(port).await;
    let report = scene_sync::configure(&c, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert_eq!(report.configured, 2);
    assert!(report.errors.is_empty());
    {
        let w = world.lock().unwrap();
        let native: Vec<_> = w.items.iter().filter(|i| i.2 == report.source).collect();
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].0, "Personagem");
        assert!(!native[0].3, "hidden avatar stays hidden");
    }
    let again = scene_sync::configure(&c, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert_eq!(again.created_items, 0);
}
#[tokio::test]
async fn transform_failure_preserves_original_and_reports_partial_progress() {
    let (port, _obs, world) = fake_obs("certa").await;
    let old = {
        let mut w = world.lock().unwrap();
        input(&mut w, "Antigo", scene::KIND, json!({"url": URL}));
        w.rejected_request = Some("SetSceneItemTransform".into());
        item(&mut w, "Principal", "Antigo", true)
    };
    let report = scene_sync::configure(&client(port).await, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert_eq!(report.configured, 1);
    assert_eq!(report.errors.len(), 1);
    let w = world.lock().unwrap();
    assert!(w.items.iter().find(|i| i.1 == old).unwrap().3);
    assert!(
        !w.items
            .iter()
            .any(|i| i.0 == "Principal" && i.2 == report.source)
    );
}
#[tokio::test]
async fn missing_plugin_makes_no_changes() {
    let (port, _obs, world) = fake_obs("certa").await;
    world.lock().unwrap().input_kinds.clear();
    assert!(
        scene_sync::configure(&client(port).await, &settings(), scene::NATIVE_KIND)
            .await
            .is_err()
    );
    let w = world.lock().unwrap();
    assert!(w.items.is_empty());
    assert_eq!(w.inputs.len(), 2);
}

#[tokio::test]
async fn existing_nested_avatar_is_detected_and_shown_without_adding_duplicate() {
    let (port, _obs, world) = fake_obs("certa").await;
    {
        let mut w = world.lock().unwrap();
        w.groups.push("Grupo".into());
        input(&mut w, "Avatar", scene::KIND, json!({"url": URL}));
        item(&mut w, "Principal", "Grupo", false);
        item(&mut w, "Grupo", "Avatar", true);
    }
    let c = client(port).await;
    assert_eq!(
        scene::placement(&c, "Principal", URL).await.unwrap(),
        scene::Placement::Hidden
    );
    assert_eq!(
        scene::add(&c, "Principal", &settings()).await.unwrap(),
        scene::Added::Reused {
            name: "Avatar".into(),
            was_hidden: true
        }
    );
    assert_eq!(
        scene::placement(&c, "Principal", URL).await.unwrap(),
        scene::Placement::Visible
    );
    assert_eq!(world.lock().unwrap().items.len(), 2);
}

#[tokio::test]
async fn initial_settings_failure_cleans_up_seed_without_hiding_original() {
    let (port, _obs, world) = fake_obs("certa").await;
    let old = {
        let mut w = world.lock().unwrap();
        input(&mut w, "Antigo", scene::KIND, json!({"url": URL}));
        w.rejected_request = Some("SetInputSettings".into());
        item(&mut w, "Principal", "Antigo", true)
    };
    assert!(
        scene_sync::configure(&client(port).await, &settings(), scene::NATIVE_KIND)
            .await
            .is_err()
    );
    let w = world.lock().unwrap();
    assert_eq!(w.items.len(), 1);
    assert_eq!(w.items[0].1, old);
    assert!(w.items[0].3);
}

#[tokio::test]
async fn hidden_placeholder_does_not_suppress_visible_legacy_avatar() {
    let (port, _obs, world) = fake_obs("certa").await;
    {
        let mut w = world.lock().unwrap();
        input(&mut w, "OmaTuber", scene::NATIVE_KIND, json!({}));
        input(&mut w, "Antigo", scene::KIND, json!({"url": URL}));
        item(&mut w, "Principal", "OmaTuber", false);
        item(&mut w, "Principal", "Antigo", true);
    }
    let c = client(port).await;
    let report = scene_sync::configure(&c, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert!(report.errors.is_empty());
    let count = {
        let w = world.lock().unwrap();
        assert_eq!(
            w.items
                .iter()
                .filter(|i| i.0 == "Principal" && i.2 == "OmaTuber" && i.3)
                .count(),
            1
        );
        w.items.len()
    };
    let again = scene_sync::configure(&c, &settings(), scene::NATIVE_KIND)
        .await
        .unwrap();
    assert_eq!(again.created_items, 0);
    assert_eq!(world.lock().unwrap().items.len(), count);
}
