//! The OBS worker against a fake obs-websocket 5.x server that behaves like
//! OBS: authentication, GetVersion, GetInputList, GetInputVolume and
//! InputVolumeMeters events. Message shapes follow the protocol
//! (obs-websocket `docs/generated/protocol.md`) and the mock in obws's own
//! integration tests. The local OBS was never running while this was
//! written; this exercises the connection path in its absence.

use std::net::Ipv4Addr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use omatuber::mouth::Mouth;
use omatuber::obs::{self, Command, Connection, Status};
use omatuber::server::Outputs;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_websockets::{CloseCode, Message, ServerBuilder, WebSocketStream};
use uuid::Uuid;

const MIC: Uuid = Uuid::from_u128(0x0a);
const CAMERA: Uuid = Uuid::from_u128(0x0b);

/// What the test tells the fake OBS to do.
enum Control {
    /// One InputVolumeMeters event with this RMS level for the microphone.
    Meter(f32),
    /// Close the connection, as when OBS quits.
    Drop,
}

/// Accepts connections one after another, like OBS staying up across
/// client reconnects.
async fn fake_obs(password: &'static str) -> (u16, mpsc::UnboundedSender<Control>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let Ok((_, ws)) = ServerBuilder::new().accept(tcp).await else {
                continue;
            };
            if !session(ws, password, &mut rx).await {
                return;
            }
        }
    });
    (port, tx)
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
                    let (status, data) = respond(&req["d"]);
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

fn respond(req: &Value) -> (Value, Value) {
    let ok = json!({ "result": true, "code": 100 });
    match req["requestType"].as_str() {
        Some("GetVersion") => (
            ok,
            json!({
                "obsStudioVersion": "32.2.2",
                "obsWebSocketVersion": "5.7.4",
                "rpcVersion": 1,
                "availableRequests": [],
                "supportedImageFormats": [],
                "platform": "fake",
                "platformDescription": "",
            }),
        ),
        Some("GetInputList") => (
            ok,
            json!({ "inputs": [
                { "inputName": "Mic/Aux", "inputUuid": MIC,
                  "inputKind": "pulse_input_capture", "unversionedInputKind": "pulse_input_capture" },
                { "inputName": "Camera", "inputUuid": CAMERA,
                  "inputKind": "v4l2_input", "unversionedInputKind": "v4l2_input" },
            ]}),
        ),
        Some("GetInputVolume") if req["requestData"]["inputUuid"] == json!(MIC) => {
            (ok, json!({ "inputVolumeMul": 1.0, "inputVolumeDb": 0.0 }))
        }
        // Video inputs have no volume: OBS answers with a failed status.
        Some("GetInputVolume") => (
            json!({ "result": false, "code": 604, "comment": "The specified input does not support audio." }),
            Value::Null,
        ),
        _ => (json!({ "result": false, "code": 204 }), Value::Null),
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

#[tokio::test]
async fn connects_and_lists_only_audio_inputs() {
    let (port, _obs) = fake_obs("certa").await;
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
    let (port, obs_ctl) = fake_obs("certa").await;
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
    let (port, _obs) = fake_obs("certa").await;
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
    let (port, obs_ctl) = fake_obs("certa").await;
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
