//! The OBS worker and scene setup against a fake obs-websocket 5.x server
//! that behaves like OBS: authentication, GetVersion, inputs, scenes, scene
//! items and InputVolumeMeters events. Message shapes follow the protocol
//! (obs-websocket `docs/generated/protocol.md`) and the mock in obws's own
//! integration tests. The local OBS was never running while this was
//! written; this exercises the protocol path in its absence.

mod common;

use std::time::Duration;

use common::{Control, Input, MIC, URL, fake_obs};

use omatuber::mouth::Mouth;
use omatuber::obs::{self, Command, Connection, Status};
use omatuber::scene::{self, Added, BrowserSettings, Placement};
use omatuber::server::Outputs;
use serde_json::json;
use tokio::sync::watch;
use uuid::Uuid;

async fn wait_for(
    status: &mut watch::Receiver<Status>,
    what: &str,
    f: impl Fn(&Status) -> bool,
) -> Status {
    let found = tokio::time::timeout(Duration::from_secs(8), status.wait_for(|s| f(s)))
        .await
        .ok()
        .and_then(Result::ok)
        .map(|s| s.clone());
    found.unwrap_or_else(|| {
        panic!(
            "timed out waiting for {what}; last status: {:?}",
            *status.borrow()
        )
    })
}

fn connect(port: u16, password: &str) -> Command {
    Command::Connect {
        port,
        password: password.to_owned(),
    }
}

// ------------------------------------------------------------- connection

#[tokio::test]
async fn connects_and_lists_only_audio_inputs() {
    let (port, _obs, _) = fake_obs("certa").await;
    let worker = obs::spawn(Outputs::default(), || {});
    let mut status = worker.status.clone();
    worker.commands.send(connect(port, "certa")).unwrap();
    let s = wait_for(&mut status, "connected", |s| {
        s.connection == Connection::Connected
    })
    .await;
    let names: Vec<_> = s.inputs.iter().map(|i| (i.uuid, i.name.as_str())).collect();
    assert_eq!(
        names,
        [(MIC, "Mic/Aux")],
        "the camera has no audio and is left out"
    );
}

#[tokio::test]
async fn meters_drive_the_mouth_and_receiving_flag() {
    let (port, obs_ctl, _) = fake_obs("certa").await;
    let outputs = Outputs::default();
    let worker = obs::spawn(outputs.clone(), || {});
    let mut status = worker.status.clone();
    worker.commands.send(connect(port, "certa")).unwrap();
    wait_for(&mut status, "connected", |s| {
        s.connection == Connection::Connected
    })
    .await;
    worker.commands.send(Command::Select(Some(MIC))).unwrap();

    // 0.3 linear RMS is about -10 dBFS, above the default -30 dB threshold.
    for _ in 0..3 {
        obs_ctl.send(Control::Meter(0.3)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let s = wait_for(&mut status, "mouth open", |s| s.mouth == Mouth::Open).await;
    assert!(s.receiving);
    assert!((s.level_db + 10.46).abs() < 0.1, "{}", s.level_db);
    assert_eq!(outputs.mouth(), Mouth::Open, "browser source follows");

    // Silence (a muted source sends zeros) closes it after the hold.
    for _ in 0..8 {
        obs_ctl.send(Control::Meter(0.0)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let s = wait_for(&mut status, "mouth closed", |s| s.mouth == Mouth::Closed).await;
    assert!(s.receiving, "zeros still count as receiving");

    // No more meters for this source (not active in the scene): not receiving.
    wait_for(&mut status, "not receiving", |s| !s.receiving).await;
}

#[tokio::test]
async fn wrong_password_fails_without_retrying() {
    let (port, _obs, _) = fake_obs("certa").await;
    let worker = obs::spawn(Outputs::default(), || {});
    let mut status = worker.status.clone();
    worker.commands.send(connect(port, "errada")).unwrap();
    let s = wait_for(&mut status, "failed", |s| {
        matches!(s.connection, Connection::Failed(_))
    })
    .await;
    assert_eq!(
        s.connection,
        Connection::Failed("senha do OBS recusada".to_owned())
    );
}

#[tokio::test]
async fn dropped_connection_reconnects_by_itself() {
    let (port, obs_ctl, _) = fake_obs("certa").await;
    let worker = obs::spawn(Outputs::default(), || {});
    let mut status = worker.status.clone();
    worker.commands.send(connect(port, "certa")).unwrap();
    wait_for(&mut status, "connected", |s| {
        s.connection == Connection::Connected
    })
    .await;
    obs_ctl.send(Control::Drop).unwrap();
    wait_for(&mut status, "retrying", |s| {
        matches!(s.connection, Connection::Retrying { .. })
    })
    .await;
    // The fake keeps listening; the 1 s retry finds it again.
    wait_for(&mut status, "connected again", |s| {
        s.connection == Connection::Connected
    })
    .await;
}

// ------------------------------------------------------------------ scene

async fn client(port: u16) -> obws::Client {
    obws::Client::connect("127.0.0.1", port, Some("certa"))
        .await
        .unwrap()
}

fn settings() -> BrowserSettings {
    BrowserSettings {
        url: URL.to_owned(),
        width: 512,
        height: 512,
    }
}

#[tokio::test]
async fn lists_scenes_in_obs_order_with_the_program_scene() {
    let (port, _obs, _) = fake_obs("certa").await;
    let s = scene::scenes(&client(port).await).await.unwrap();
    assert_eq!(s.names, ["Principal", "Intervalo"]);
    assert_eq!(s.program.as_deref(), Some("Principal"));
}

#[tokio::test]
async fn adding_twice_creates_one_source_and_reuses_it() {
    let (port, _obs, world) = fake_obs("certa").await;
    let c = client(port).await;
    assert_eq!(
        scene::placement(&c, "Principal", URL).await.unwrap(),
        Placement::NotAdded
    );

    let first = scene::add(&c, "Principal", &settings()).await.unwrap();
    assert_eq!(
        first,
        Added::Created {
            name: "OmaTuber".to_owned()
        }
    );
    {
        let w = world.lock().unwrap();
        let ours = w.ours();
        assert_eq!(ours.len(), 1);
        assert_eq!(
            ours[0].settings,
            json!({ "url": URL, "width": 512, "height": 512 })
        );
    }
    assert_eq!(
        scene::placement(&c, "Principal", URL).await.unwrap(),
        Placement::Visible
    );

    let again = scene::add(&c, "Principal", &settings()).await.unwrap();
    assert_eq!(
        again,
        Added::Reused {
            name: "OmaTuber".to_owned(),
            was_hidden: false
        }
    );
    assert_eq!(world.lock().unwrap().ours().len(), 1, "no duplicate");
    assert_eq!(world.lock().unwrap().items.len(), 1);
}

#[tokio::test]
async fn other_scene_links_the_same_source_and_hidden_is_shown() {
    let (port, _obs, world) = fake_obs("certa").await;
    let c = client(port).await;
    scene::add(&c, "Principal", &settings()).await.unwrap();

    let linked = scene::add(&c, "Intervalo", &settings()).await.unwrap();
    assert_eq!(
        linked,
        Added::Linked {
            name: "OmaTuber".to_owned()
        }
    );
    assert_eq!(
        world.lock().unwrap().ours().len(),
        1,
        "same input in two scenes"
    );

    world.lock().unwrap().items[0].3 = false; // hidden in Principal by the person
    assert_eq!(
        scene::placement(&c, "Principal", URL).await.unwrap(),
        Placement::Hidden
    );
    let shown = scene::add(&c, "Principal", &settings()).await.unwrap();
    assert_eq!(
        shown,
        Added::Reused {
            name: "OmaTuber".to_owned(),
            was_hidden: true
        }
    );
    assert_eq!(
        scene::placement(&c, "Principal", URL).await.unwrap(),
        Placement::Visible
    );
}

#[tokio::test]
async fn renamed_source_is_still_recognised_and_names_never_clash() {
    let (port, _obs, world) = fake_obs("certa").await;
    let c = client(port).await;
    // Someone already has an unrelated input called "OmaTuber".
    world.lock().unwrap().inputs.push(Input {
        name: "OmaTuber".to_owned(),
        uuid: Uuid::new_v4(),
        kind: "text_ft2_source_v2".to_owned(),
        settings: json!({}),
    });
    let created = scene::add(&c, "Principal", &settings()).await.unwrap();
    assert_eq!(
        created,
        Added::Created {
            name: "OmaTuber 2".to_owned()
        }
    );

    // Renamed in OBS: recognised by its URL, not its name.
    {
        let mut w = world.lock().unwrap();
        let i = w
            .inputs
            .iter()
            .position(|i| i.name == "OmaTuber 2")
            .unwrap();
        w.inputs[i].name = "Meu avatar".to_owned();
        w.items[0].2 = "Meu avatar".to_owned();
    }
    let again = scene::add(&c, "Principal", &settings()).await.unwrap();
    assert_eq!(
        again,
        Added::Reused {
            name: "Meu avatar".to_owned(),
            was_hidden: false
        }
    );
    assert_eq!(world.lock().unwrap().ours().len(), 1);
}

#[tokio::test]
async fn worker_adds_to_the_scene_on_air_and_reports_placement() {
    let (port, _obs, world) = fake_obs("certa").await;
    let worker = obs::spawn(Outputs::default(), || {});
    let mut status = worker.status.clone();
    worker
        .commands
        .send(Command::SceneTarget {
            scene: None,
            settings: settings(),
        })
        .unwrap();
    worker.commands.send(connect(port, "certa")).unwrap();
    let s = wait_for(&mut status, "placement known", |s| s.placement.is_some()).await;
    assert_eq!(s.scenes.program.as_deref(), Some("Principal"));
    assert_eq!(
        s.placement,
        Some(("Principal".to_owned(), Placement::NotAdded))
    );

    worker.commands.send(Command::AddToScene).unwrap();
    let s = wait_for(&mut status, "visible", |s| {
        s.placement
            .as_ref()
            .is_some_and(|(_, p)| *p == Placement::Visible)
    })
    .await;
    assert_eq!(
        s.scene_message.as_deref(),
        Some("Fonte \"OmaTuber\" criada na cena Principal")
    );

    // A chosen scene other than the one on air.
    worker
        .commands
        .send(Command::SceneTarget {
            scene: Some("Intervalo".to_owned()),
            settings: settings(),
        })
        .unwrap();
    wait_for(&mut status, "intervalo", |s| {
        s.placement == Some(("Intervalo".to_owned(), Placement::NotAdded))
    })
    .await;
    worker.commands.send(Command::AddToScene).unwrap();
    wait_for(&mut status, "linked", |s| {
        s.placement == Some(("Intervalo".to_owned(), Placement::Visible))
    })
    .await;
    assert_eq!(world.lock().unwrap().ours().len(), 1);
}

#[tokio::test]
async fn collects_levels_for_calibration() {
    let (port, obs_ctl, _) = fake_obs("certa").await;
    let worker = obs::spawn(Outputs::default(), || {});
    let mut status = worker.status.clone();
    worker.commands.send(connect(port, "certa")).unwrap();
    wait_for(&mut status, "connected", |s| {
        s.connection == Connection::Connected
    })
    .await;
    worker.commands.send(Command::Select(Some(MIC))).unwrap();
    worker
        .commands
        .send(Command::CollectLevels(Duration::from_millis(500)))
        .unwrap();
    wait_for(&mut status, "collecting", |s| s.collecting).await;
    for _ in 0..8 {
        obs_ctl.send(Control::Meter(0.1)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let s = wait_for(&mut status, "batch ready", |s| s.batches == 1).await;
    assert!(!s.collecting);
    assert!(
        (6..=10).contains(&s.collected.len()),
        "{} levels",
        s.collected.len()
    );
    assert!(
        s.collected.iter().all(|db| (db + 20.0).abs() < 0.1),
        "{:?}",
        s.collected
    );
}

#[tokio::test]
async fn mute_state_follows_obs() {
    let (port, obs_ctl, world) = fake_obs("certa").await;
    world.lock().unwrap().mic_muted = true;
    let worker = obs::spawn(Outputs::default(), || {});
    let mut status = worker.status.clone();
    worker.commands.send(Command::Select(Some(MIC))).unwrap();
    worker.commands.send(connect(port, "certa")).unwrap();
    wait_for(&mut status, "muted at connect", |s| s.muted == Some(true)).await;
    obs_ctl.send(Control::Mute(false)).unwrap();
    wait_for(&mut status, "unmuted by event", |s| s.muted == Some(false)).await;
    obs_ctl.send(Control::Mute(true)).unwrap();
    wait_for(&mut status, "muted by event", |s| s.muted == Some(true)).await;
    worker.commands.send(Command::Disconnect).unwrap();
    wait_for(&mut status, "unknown when disconnected", |s| {
        s.muted.is_none()
    })
    .await;
}
