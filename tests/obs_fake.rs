//! The OBS worker and scene setup against a fake obs-websocket 5.x server
//! that behaves like OBS: authentication, GetVersion, inputs, scenes, scene
//! items and InputVolumeMeters events. Message shapes follow the protocol
//! (obs-websocket `docs/generated/protocol.md`) and the mock in obws's own
//! integration tests. The local OBS was never running while this was
//! written; this exercises the protocol path in its absence.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use omatuber::mouth::Mouth;
use omatuber::obs::{self, Command, Connection, Status};
use omatuber::scene::{self, Added, BrowserSettings, Placement};
use omatuber::server::Outputs;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_websockets::{CloseCode, Message, ServerBuilder, WebSocketStream};
use uuid::Uuid;

const MIC: Uuid = Uuid::from_u128(0x0a);
const CAMERA: Uuid = Uuid::from_u128(0x0b);
const URL: &str = "http://127.0.0.1:39017/";

/// OBS state shared by every connection, inspectable by the test.
struct World {
    scenes: Vec<String>,
    program: String,
    inputs: Vec<Input>,
    /// (scene, item id, source name, enabled)
    items: Vec<(String, i64, String, bool)>,
    next_item: i64,
}

struct Input {
    name: String,
    uuid: Uuid,
    kind: String,
    settings: Value,
}

impl World {
    fn new() -> Self {
        let input = |name: &str, uuid, kind: &str| Input {
            name: name.to_owned(),
            uuid,
            kind: kind.to_owned(),
            settings: json!({}),
        };
        Self {
            scenes: vec!["Principal".to_owned(), "Intervalo".to_owned()],
            program: "Principal".to_owned(),
            inputs: vec![
                input("Mic/Aux", MIC, "pulse_input_capture"),
                input("Camera", CAMERA, "v4l2_input"),
            ],
            items: Vec::new(),
            next_item: 1,
        }
    }

    fn ours(&self) -> Vec<&Input> {
        self.inputs
            .iter()
            .filter(|i| i.kind == scene::KIND && i.settings["url"] == URL)
            .collect()
    }
}

/// What the test tells the fake OBS to do.
enum Control {
    /// One InputVolumeMeters event with this RMS level for the microphone.
    Meter(f32),
    /// Close the connection, as when OBS quits.
    Drop,
}

type Shared = Arc<Mutex<World>>;

/// Accepts connections one after another, like OBS staying up across
/// client reconnects.
async fn fake_obs(password: &'static str) -> (u16, mpsc::UnboundedSender<Control>, Shared) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let world: Shared = Arc::new(Mutex::new(World::new()));
    let w = world.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let Ok((_, ws)) = ServerBuilder::new().accept(tcp).await else {
                continue;
            };
            if !session(ws, password, &mut rx, &w).await {
                return;
            }
        }
    });
    (port, tx, world)
}

fn auth_response(password: &str, salt: &str, challenge: &str) -> String {
    let secret = STANDARD.encode(Sha256::digest(format!("{password}{salt}")));
    STANDARD.encode(Sha256::digest(format!("{secret}{challenge}")))
}

async fn send(ws: &mut WebSocketStream<TcpStream>, op: u8, d: Value) {
    let _ = ws
        .send(Message::text(json!({ "op": op, "d": d }).to_string()))
        .await;
}

/// One client session; returns false when the test dropped its controller.
async fn session(
    mut ws: WebSocketStream<TcpStream>,
    password: &str,
    control: &mut mpsc::UnboundedReceiver<Control>,
    world: &Shared,
) -> bool {
    let (salt, challenge) = ("fake-salt", "fake-challenge");
    send(
        &mut ws,
        0,
        json!({
            "obsStudioVersion": "32.2.2",
            "obsWebSocketVersion": "5.7.4",
            "rpcVersion": 1,
            "authentication": { "challenge": challenge, "salt": salt },
        }),
    )
    .await;
    let Some(Ok(identify)) = ws.next().await else {
        return true;
    };
    let identify: Value = serde_json::from_str(identify.as_text().unwrap_or("{}")).unwrap();
    if identify["d"]["authentication"] != auth_response(password, salt, challenge) {
        // obs-websocket closes with WebSocketCloseCode::AuthenticationFailed.
        let _ = ws
            .send(Message::close(
                CloseCode::try_from(4009).ok(),
                "Authentication failed.",
            ))
            .await;
        return true;
    }
    send(&mut ws, 2, json!({ "negotiatedRpcVersion": 1 })).await;

    loop {
        tokio::select! {
            msg = ws.next() => {
                let Some(Ok(msg)) = msg else { return true };
                let Some(text) = msg.as_text() else { continue };
                let req: Value = serde_json::from_str(text).unwrap();
                if req["op"] == 6 {
                    let (status, data) = respond(&req["d"], &mut world.lock().unwrap());
                    send(&mut ws, 7, json!({
                        "requestType": req["d"]["requestType"],
                        "requestId": req["d"]["requestId"],
                        "requestStatus": status,
                        "responseData": data,
                    })).await;
                }
            }
            cmd = control.recv() => match cmd {
                None => return false,
                Some(Control::Drop) => return true,
                Some(Control::Meter(rms)) => {
                    send(&mut ws, 5, json!({
                        "eventType": "InputVolumeMeters",
                        "eventIntent": 1 << 16,
                        "eventData": { "inputs": [{
                            "inputName": "Mic/Aux",
                            "inputUuid": MIC,
                            "inputLevelsMul": [[rms, rms, rms]],
                        }]},
                    })).await;
                }
            }
        }
    }
}

fn fail(code: u16, comment: &str) -> (Value, Value) {
    (
        json!({ "result": false, "code": code, "comment": comment }),
        Value::Null,
    )
}

fn respond(req: &Value, w: &mut World) -> (Value, Value) {
    let ok = json!({ "result": true, "code": 100 });
    let data = &req["requestData"];
    let find_input = |w: &World| -> Option<usize> {
        w.inputs
            .iter()
            .position(|i| data["inputUuid"] == json!(i.uuid) || data["inputName"] == i.name)
    };
    match req["requestType"].as_str().unwrap_or("") {
        "GetVersion" => (
            ok,
            json!({
                "obsStudioVersion": "32.2.2", "obsWebSocketVersion": "5.7.4", "rpcVersion": 1,
                "availableRequests": [], "supportedImageFormats": [],
                "platform": "fake", "platformDescription": "",
            }),
        ),
        "GetInputList" => {
            let kind = data["inputKind"].as_str();
            let inputs: Vec<Value> = w
                .inputs
                .iter()
                .filter(|i| kind.is_none_or(|k| k == i.kind))
                .map(|i| json!({ "inputName": i.name, "inputUuid": i.uuid, "inputKind": i.kind, "unversionedInputKind": i.kind }))
                .collect();
            (ok, json!({ "inputs": inputs }))
        }
        "GetInputVolume" if data["inputUuid"] == json!(MIC) => {
            (ok, json!({ "inputVolumeMul": 1.0, "inputVolumeDb": 0.0 }))
        }
        // Video and browser inputs have no volume: OBS answers with a failed status.
        "GetInputVolume" => fail(604, "The specified input does not support audio."),
        "GetInputSettings" => match find_input(w) {
            Some(i) => (
                ok,
                json!({ "inputSettings": w.inputs[i].settings, "inputKind": w.inputs[i].kind }),
            ),
            None => fail(600, "No source was found."),
        },
        "GetSceneList" => {
            let n = w.scenes.len();
            let scenes: Vec<Value> = w
                .scenes
                .iter()
                .enumerate()
                // OBS lists scenes with the bottom of its list at index 0.
                .map(|(i, s)| json!({ "sceneName": s, "sceneUuid": Uuid::from_u128(100 + i as u128), "sceneIndex": n - 1 - i }))
                .collect();
            (
                ok,
                json!({
                    "currentProgramSceneName": w.program, "currentProgramSceneUuid": Uuid::from_u128(100),
                    "currentPreviewSceneName": null, "currentPreviewSceneUuid": null,
                    "scenes": scenes,
                }),
            )
        }
        "GetSceneItemList" => {
            let items: Vec<Value> = w
                .items
                .iter()
                .filter(|(s, ..)| data["sceneName"] == *s)
                .enumerate()
                .map(|(idx, (_, id, source, _))| json!({
                    "sceneItemId": id, "sceneItemIndex": idx, "sourceName": source,
                    "sourceType": "OBS_SOURCE_TYPE_INPUT", "inputKind": scene::KIND, "isGroup": null,
                }))
                .collect();
            (ok, json!({ "sceneItems": items }))
        }
        "GetSceneItemEnabled" | "SetSceneItemEnabled" => {
            let set = req["requestType"] == "SetSceneItemEnabled";
            let item = w
                .items
                .iter_mut()
                .find(|(s, id, ..)| data["sceneName"] == *s && data["sceneItemId"] == *id);
            match item {
                Some(item) if set => {
                    item.3 = data["sceneItemEnabled"].as_bool().unwrap();
                    (ok, Value::Null)
                }
                Some(item) => (ok, json!({ "sceneItemEnabled": item.3 })),
                None => fail(600, "No scene item was found."),
            }
        }
        "CreateInput" => {
            let name = data["inputName"].as_str().unwrap().to_owned();
            if w.inputs.iter().any(|i| i.name == name) {
                return fail(601, "A source already exists by that input name.");
            }
            let uuid = Uuid::new_v4();
            w.inputs.push(Input {
                name: name.clone(),
                uuid,
                kind: data["inputKind"].as_str().unwrap().to_owned(),
                settings: data["inputSettings"].clone(),
            });
            let id = w.next_item;
            w.next_item += 1;
            let enabled = data["sceneItemEnabled"].as_bool().unwrap_or(true);
            w.items.push((
                data["sceneName"].as_str().unwrap().to_owned(),
                id,
                name,
                enabled,
            ));
            (ok, json!({ "inputUuid": uuid, "sceneItemId": id }))
        }
        "CreateSceneItem" => {
            let id = w.next_item;
            w.next_item += 1;
            let enabled = data["sceneItemEnabled"].as_bool().unwrap_or(true);
            let source = data["sourceName"].as_str().unwrap().to_owned();
            w.items.push((
                data["sceneName"].as_str().unwrap().to_owned(),
                id,
                source,
                enabled,
            ));
            (ok, json!({ "sceneItemId": id }))
        }
        _ => fail(204, "Unknown request type."),
    }
}

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
