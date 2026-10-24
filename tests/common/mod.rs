//! A fake obs-websocket 5.x server that behaves like OBS, shared by the
//! integration tests. Message shapes follow the protocol (obs-websocket
//! `docs/generated/protocol.md`) and the mock in obws's own tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use omatuber::scene;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_websockets::{CloseCode, Message, ServerBuilder, WebSocketStream};
use uuid::Uuid;

pub const MIC: Uuid = Uuid::from_u128(0x0a);
pub const CAMERA: Uuid = Uuid::from_u128(0x0b);
pub const URL: &str = "http://127.0.0.1:39017/";

/// OBS state shared by every connection, inspectable by the test.
pub struct World {
    pub scenes: Vec<String>,
    pub program: String,
    pub inputs: Vec<Input>,
    /// (scene, item id, source name, enabled)
    pub items: Vec<(String, i64, String, bool)>,
    pub next_item: i64,
    pub mic_muted: bool,
    pub groups: Vec<String>,
    pub transforms: HashMap<(String, i64), Value>,
    pub locks: HashMap<(String, i64), bool>,
    pub blends: HashMap<(String, i64), String>,
    pub rejected_request: Option<String>,
    pub input_kinds: Vec<String>,
}

pub struct Input {
    pub name: String,
    pub uuid: Uuid,
    pub kind: String,
    pub settings: Value,
}

impl World {
    pub fn new() -> Self {
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
            mic_muted: false,
            groups: Vec::new(),
            transforms: HashMap::new(),
            locks: HashMap::new(),
            blends: HashMap::new(),
            rejected_request: None,
            input_kinds: vec![scene::KIND.into(), scene::NATIVE_KIND.into()],
        }
    }

    pub fn ours(&self) -> Vec<&Input> {
        self.inputs
            .iter()
            .filter(|i| i.kind == scene::KIND && i.settings["url"] == URL)
            .collect()
    }
}

/// What the test tells the fake OBS to do.
pub enum Control {
    /// One InputVolumeMeters event with this RMS level for the microphone.
    Meter(f32),
    /// Close the connection, as when OBS quits.
    Drop,
    /// The person (un)mutes the microphone in OBS's mixer.
    Mute(bool),
}

/// Nagle stays on, as in obs-websocket: neither its `WebSocketServer.cpp`
/// nor websocketpp's asio transport (`connection.hpp`, `endpoint.hpp`)
/// sets TCP_NODELAY. A write right after another unacknowledged one then
/// waits for the client's delayed ACK, ~40 ms on Linux; the latency test
/// shows it once, on the first meter event after a request's response.
pub const NODELAY: bool = false;

pub type Shared = Arc<Mutex<World>>;

/// Accepts connections one after another, like OBS staying up across
/// client reconnects.
pub async fn fake_obs(password: &'static str) -> (u16, mpsc::UnboundedSender<Control>, Shared) {
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
            let _ = tcp.set_nodelay(NODELAY);
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
                Some(Control::Mute(muted)) => {
                    world.lock().unwrap().mic_muted = muted;
                    send(&mut ws, 5, json!({
                        "eventType": "InputMuteStateChanged",
                        "eventIntent": 1 << 3,
                        "eventData": { "inputName": "Mic/Aux", "inputUuid": MIC, "inputMuted": muted },
                    })).await;
                }
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
    if w.rejected_request.as_deref() == req["requestType"].as_str() {
        return fail(500, "Injected request failure");
    }
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
        "GetInputKindList" => (ok, json!({ "inputKinds": w.input_kinds })),
        "GetGroupList" => (ok, json!({ "groups": w.groups })),
        "SetInputSettings" => {
            let Some(i) = find_input(w) else {
                return fail(600, "No source was found.");
            };
            if data["overlay"] == false {
                w.inputs[i].settings = json!({});
            }
            for (key, value) in data["inputSettings"].as_object().unwrap() {
                w.inputs[i].settings[key] = value.clone();
            }
            (ok, Value::Null)
        }
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
        "GetInputMute" if data["inputUuid"] == json!(MIC) => {
            (ok, json!({ "inputMuted": w.mic_muted }))
        }
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
        "GetSceneItemList" | "GetGroupSceneItemList" => {
            let items: Vec<Value> = w
                .items
                .iter()
                .filter(|(s, ..)| data["sceneName"] == *s)
                .enumerate()
                .map(|(idx, (_, id, source, _))| {
                    let group = w.groups.contains(source);
                    let nested = group || w.scenes.contains(source);
                    let kind = w.inputs.iter().find(|i| &i.name == source).map(|i| &i.kind);
                    json!({ "sceneItemId": id, "sceneItemIndex": idx, "sourceName": source,
                        "sourceType": if nested { "OBS_SOURCE_TYPE_SCENE" } else { "OBS_SOURCE_TYPE_INPUT" },
                        "inputKind": kind, "isGroup": if nested { Some(group) } else { None },
                    })
                })
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
        "GetSceneItemTransform"
        | "SetSceneItemTransform"
        | "GetSceneItemLocked"
        | "SetSceneItemLocked"
        | "GetSceneItemBlendMode"
        | "SetSceneItemBlendMode" => {
            let key = (
                data["sceneName"].as_str().unwrap().to_owned(),
                data["sceneItemId"].as_i64().unwrap(),
            );
            if !w.items.iter().any(|i| i.0 == key.0 && i.1 == key.1) {
                return fail(600, "No scene item was found.");
            }
            match req["requestType"].as_str().unwrap() {
                "GetSceneItemTransform" => (
                    ok,
                    json!({ "sceneItemTransform": w.transforms.get(&key).cloned().unwrap_or_else(default_transform) }),
                ),
                "SetSceneItemTransform" => {
                    let transform = w.transforms.entry(key).or_insert_with(default_transform);
                    for (k, v) in data["sceneItemTransform"].as_object().unwrap() {
                        transform[k] = v.clone();
                    }
                    (ok, Value::Null)
                }
                "GetSceneItemLocked" => (
                    ok,
                    json!({ "sceneItemLocked": w.locks.get(&key).copied().unwrap_or(false) }),
                ),
                "SetSceneItemLocked" => {
                    w.locks
                        .insert(key, data["sceneItemLocked"].as_bool().unwrap());
                    (ok, Value::Null)
                }
                "GetSceneItemBlendMode" => (
                    ok,
                    json!({ "sceneItemBlendMode": w.blends.get(&key).map_or("OBS_BLEND_NORMAL", String::as_str) }),
                ),
                _ => {
                    w.blends
                        .insert(key, data["sceneItemBlendMode"].as_str().unwrap().to_owned());
                    (ok, Value::Null)
                }
            }
        }
        "SetSceneItemIndex" => {
            let Some(old) = w
                .items
                .iter()
                .position(|i| data["sceneName"] == i.0 && data["sceneItemId"] == i.1)
            else {
                return fail(600, "No scene item was found.");
            };
            let item = w.items.remove(old);
            let index = data["sceneItemIndex"].as_u64().unwrap() as usize;
            let dest = w
                .items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.0 == item.0)
                .nth(index)
                .map_or(w.items.len(), |(i, _)| i);
            w.items.insert(dest, item);
            (ok, Value::Null)
        }
        "RemoveSceneItem" => {
            let Some(index) = w
                .items
                .iter()
                .position(|i| data["sceneName"] == i.0 && data["sceneItemId"] == i.1)
            else {
                return fail(600, "No scene item was found.");
            };
            w.items.remove(index);
            (ok, Value::Null)
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

pub fn default_transform() -> Value {
    json!({ "sourceWidth": 512.0, "sourceHeight": 512.0, "positionX": 0.0, "positionY": 0.0,
        "rotation": 0.0, "scaleX": 1.0, "scaleY": 1.0, "width": 512.0, "height": 512.0,
        "alignment": 5, "boundsType": "OBS_BOUNDS_NONE", "boundsAlignment": 0,
        "boundsWidth": 0.0, "boundsHeight": 0.0, "cropLeft": 0, "cropRight": 0,
        "cropTop": 0, "cropBottom": 0, "cropToBounds": false })
}
