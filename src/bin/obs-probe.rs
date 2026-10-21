//! Step 1 probe: checks the obs-websocket connection and logs meter samples
//! for one audio input. No audio is recorded, only the levels OBS reports.
//!
//! The password is read from `OBS_PASSWORD` so it stays out of shell history
//! and process listings; the port from `OBS_PORT` (default 4455).
//!
//!     obs-probe list
//!     obs-probe record <input-uuid> [seconds]
//!
//! `record` prints CSV to stdout, one row per meter event:
//! `t_ms,gap_ms,present,channels,rms_db,peak_db,mouth`.

use std::env;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures_util::{StreamExt, pin_mut};
use obws::Client;
use obws::client::ConnectConfig;
use obws::events::Event;
use obws::requests::EventSubscription;
use obws::requests::inputs::InputId;
use omatuber::meter::{LevelKind, level_db};
use omatuber::mouth::{Gate, GateParams, Mouth};
use uuid::Uuid;

const USAGE: &str = "usage: obs-probe list | obs-probe record <input-uuid> [seconds]";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["list"] => list().await,
        ["record", uuid] => record(uuid, 30).await,
        ["record", uuid, secs] => match secs.parse() {
            Ok(secs) => record(uuid, secs).await,
            Err(_) => Err(format!("invalid seconds: {secs}")),
        },
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn connect(subscriptions: EventSubscription) -> Result<Client, String> {
    let port = match env::var("OBS_PORT") {
        Ok(p) => p.parse().map_err(|_| format!("invalid OBS_PORT: {p}"))?,
        Err(_) => 4455,
    };
    let config = ConnectConfig {
        host: "127.0.0.1",
        port,
        dangerous: None,
        password: env::var("OBS_PASSWORD").ok(),
        event_subscriptions: Some(subscriptions),
        broadcast_capacity: obws::client::DEFAULT_BROADCAST_CAPACITY,
        connect_timeout: Duration::from_secs(5),
    };
    Client::connect_with_config(config).await.map_err(|e| {
        format!(
            "cannot connect to obs-websocket on 127.0.0.1:{port}: {e}\n\
             Is OBS running with Tools > WebSocket Server Settings > Enable enabled, \
             and is OBS_PASSWORD set?"
        )
    })
}

/// Audio-capable inputs as (uuid, name, kind). OBS has no audio flag in
/// GetInputList, so an input counts as audio if GetInputVolume succeeds.
async fn audio_inputs(client: &Client) -> Result<Vec<(Uuid, String, String)>, String> {
    let inputs = client
        .inputs()
        .list(None)
        .await
        .map_err(|e| e.to_string())?;
    let mut audio = Vec::new();
    for input in inputs {
        if client
            .inputs()
            .volume(InputId::Uuid(input.id.uuid))
            .await
            .is_ok()
        {
            audio.push((input.id.uuid, input.id.name, input.kind));
        }
    }
    Ok(audio)
}

async fn list() -> Result<(), String> {
    let client = connect(EventSubscription::NONE).await?;
    let version = client
        .general()
        .version()
        .await
        .map_err(|e| e.to_string())?;
    eprintln!(
        "OBS {} / obs-websocket {} / rpc {}",
        version.obs_studio_version, version.obs_web_socket_version, version.rpc_version
    );
    let inputs = audio_inputs(&client).await?;
    if inputs.is_empty() {
        eprintln!("no audio inputs found");
    }
    for (uuid, name, kind) in inputs {
        println!("{uuid}\t{kind}\t{name}");
    }
    Ok(())
}

async fn record(uuid: &str, secs: u64) -> Result<(), String> {
    let uuid: Uuid = uuid.parse().map_err(|_| format!("invalid uuid: {uuid}"))?;
    let client =
        connect(EventSubscription::INPUT_VOLUME_METERS | EventSubscription::INPUTS).await?;

    // Meter events only carry the input name in obws 0.15, so keep the
    // name for our UUID current through rename events.
    let mut name = audio_inputs(&client)
        .await?
        .into_iter()
        .find(|(u, _, _)| *u == uuid)
        .map(|(_, n, _)| n)
        .ok_or_else(|| format!("no audio input with uuid {uuid}; run `obs-probe list`"))?;
    eprintln!("recording meters for {name:?} for {secs} s");

    let events = client.events().map_err(|e| e.to_string())?;
    pin_mut!(events);
    let start = Instant::now();
    let deadline = start + Duration::from_secs(secs);
    let mut last_meter: Option<Duration> = None;
    let mut gate = Gate::new(GateParams::default());

    println!("t_ms,gap_ms,present,channels,rms_db,peak_db,mouth");
    loop {
        let event = match tokio::time::timeout_at(deadline.into(), events.next()).await {
            Err(_) => break,
            Ok(None) => return Err("OBS closed the connection".to_owned()),
            Ok(Some(event)) => event,
        };
        let now = start.elapsed();
        match event {
            Event::InputNameChanged {
                uuid: u, new_name, ..
            } if u == uuid => {
                eprintln!("input renamed to {new_name:?}");
                name = new_name;
            }
            Event::InputVolumeMeters { inputs } => {
                let gap = last_meter.map_or(0, |t| (now - t).as_millis());
                last_meter = Some(now);
                let ours = inputs.iter().find(|i| i.name == name);
                let (present, channels, rms, peak, mouth) = match ours {
                    Some(m) => {
                        let rms = level_db(&m.levels, LevelKind::Rms);
                        let peak = level_db(&m.levels, LevelKind::Peak);
                        (1, m.levels.len(), rms, peak, gate.on_level(now, rms))
                    }
                    None => (
                        0,
                        0,
                        f32::NEG_INFINITY,
                        f32::NEG_INFINITY,
                        gate.on_tick(now),
                    ),
                };
                let mouth = if mouth == Mouth::Open {
                    "open"
                } else {
                    "closed"
                };
                println!(
                    "{},{gap},{present},{channels},{rms:.1},{peak:.1},{mouth}",
                    now.as_millis()
                );
            }
            _ => {}
        }
    }
    Ok(())
}
