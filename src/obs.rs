//! Background worker that owns the OBS connection and the mouth gate.
//!
//! The gate runs here rather than in the UI because eframe stops calling
//! `App::ui` while the window is hidden, and the stream must keep animating
//! with the app minimized.

use std::time::Duration;

use futures_util::StreamExt;
use obws::Client;
use obws::client::{ConnectConfig, HandshakeError};
use obws::events::{Event, EventStream, InputVolumeMeter};
use obws::requests::EventSubscription;
use obws::requests::inputs::InputId;
use obws::responses::WebSocketCloseCode;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};
use uuid::Uuid;

use crate::meter::{LevelKind, level_db};
use crate::mouth::{Gate, GateParams, Mouth};
use crate::server::Outputs;

/// obs-websocket meter cadence; the gate is also ticked at this rate.
const TICK: Duration = Duration::from_millis(50);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(10);
/// obws ends its event stream for good when this buffer overflows
/// (`EventStream::poll_next` drops `Lagged`), so leave ample room.
const EVENT_BUFFER: usize = 1024;

#[derive(Clone, Debug)]
pub struct AudioInput {
    pub uuid: Uuid,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    Idle,
    Connecting,
    Connected,
    /// Will retry by itself after the given delay.
    Retrying {
        error: String,
        after: Duration,
    },
    /// Needs user action (e.g. wrong password); no automatic retry.
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Status {
    pub connection: Connection,
    pub inputs: Vec<AudioInput>,
    /// Level of the selected input or simulation, in dBFS.
    pub level_db: f32,
    pub mouth: Mouth,
    /// The selected input (or the simulation) delivered a level within the
    /// gate's staleness window. False when OBS stopped reporting it, e.g. the
    /// source is not active in the current scene.
    pub receiving: bool,
}

pub enum Command {
    Connect {
        port: u16,
        password: String,
    },
    Disconnect,
    Select(Option<Uuid>),
    SetParams(GateParams),
    /// `Some(level)` replaces OBS levels with a fixed dBFS value.
    Simulate(Option<f32>),
}

pub struct Handle {
    pub commands: mpsc::UnboundedSender<Command>,
    pub status: watch::Receiver<Status>,
}

/// Starts the worker on the current tokio runtime. `notify` is called after
/// every status change, e.g. to request a UI repaint.
pub fn spawn(outputs: Outputs, notify: impl Fn() + Send + 'static) -> Handle {
    let (commands, rx) = mpsc::unbounded_channel();
    let (status_tx, status) = watch::channel(Status {
        connection: Connection::Idle,
        inputs: Vec::new(),
        level_db: f32::NEG_INFINITY,
        mouth: Mouth::Closed,
        receiving: false,
    });
    tokio::spawn(Worker::new(outputs, status_tx, notify).run(rx));
    Handle { commands, status }
}

struct Session {
    client: Client,
    events: EventStream,
    /// Name of the selected input; meter events carry only names in obws 0.15.
    selected_name: Option<String>,
}

type ConnectResult = Result<(Client, EventStream, Vec<AudioInput>), ConnectError>;

enum ConnectError {
    Retry(String),
    Fatal(String),
}

struct Worker<N> {
    outputs: Outputs,
    status: watch::Sender<Status>,
    notify: N,
    gate: Gate,
    start: Instant,
    target: Option<(u16, String)>,
    selected: Option<Uuid>,
    simulate: Option<f32>,
    session: Option<Session>,
    connecting: Option<JoinHandle<ConnectResult>>,
    retry_at: Option<Instant>,
    retry_delay: Duration,
}

impl<N: Fn()> Worker<N> {
    fn new(outputs: Outputs, status: watch::Sender<Status>, notify: N) -> Self {
        Self {
            outputs,
            status,
            notify,
            gate: Gate::new(GateParams::default()),
            start: Instant::now(),
            target: None,
            selected: None,
            simulate: None,
            session: None,
            connecting: None,
            retry_at: None,
            retry_delay: RETRY_MIN,
        }
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut tick = interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                cmd = commands.recv() => match cmd {
                    Some(cmd) => self.command(cmd),
                    None => return, // UI gone
                },
                _ = tick.tick() => self.tick(),
                event = next_event(&mut self.session) => self.event(event).await,
                result = join(&mut self.connecting) => self.connected(result),
                _ = sleep_until_opt(self.retry_at) => {
                    self.retry_at = None;
                    self.start_connect();
                }
            }
        }
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Connect { port, password } => {
                self.drop_connection();
                self.target = Some((port, password));
                self.retry_delay = RETRY_MIN;
                self.start_connect();
            }
            Command::Disconnect => {
                self.drop_connection();
                self.target = None;
                self.set_connection(Connection::Idle);
            }
            Command::Select(uuid) => {
                self.selected = uuid;
                self.gate.reset();
                let name = name_of(&self.status.borrow().inputs, uuid);
                if let Some(session) = &mut self.session {
                    session.selected_name = name;
                }
            }
            Command::SetParams(params) => self.gate.set_params(params),
            Command::Simulate(level) => {
                self.simulate = level;
                self.gate.reset();
            }
        }
    }

    fn tick(&mut self) {
        let now = self.now();
        if let Some(db) = self.simulate {
            self.gate.on_level(now, db);
            self.publish(db);
        } else {
            // Closes the mouth when meters for the selected input stopped coming.
            self.gate.on_tick(now);
            let level = if self.gate.is_stale(now) {
                f32::NEG_INFINITY
            } else {
                self.status.borrow().level_db
            };
            self.publish(level);
        }
    }

    async fn event(&mut self, event: Option<Event>) {
        let Some(event) = event else {
            // OBS closed, or obws dropped the stream after a lag.
            self.drop_connection();
            self.schedule_retry("conexão com o OBS perdida".to_owned());
            return;
        };
        match event {
            Event::InputVolumeMeters { inputs } => {
                if self.simulate.is_some() {
                    return;
                }
                let name = self
                    .session
                    .as_ref()
                    .and_then(|s| s.selected_name.as_deref());
                if let Some(db) = name.and_then(|n| selected_level(&inputs, n)) {
                    self.gate.on_level(self.now(), db);
                    self.publish(db);
                }
            }
            Event::InputCreated { .. }
            | Event::InputRemoved { .. }
            | Event::InputNameChanged { .. } => self.refresh_inputs().await,
            _ => {}
        }
    }

    async fn refresh_inputs(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        // A failure here surfaces as a closed event stream; keep the old list.
        if let Ok(inputs) = audio_inputs(&session.client).await {
            session.selected_name = name_of(&inputs, self.selected);
            self.status.send_modify(|s| s.inputs = inputs);
            (self.notify)();
        }
    }

    fn start_connect(&mut self) {
        let Some((port, password)) = self.target.clone() else {
            return;
        };
        self.set_connection(Connection::Connecting);
        self.connecting = Some(tokio::spawn(connect(port, password)));
    }

    fn connected(&mut self, result: ConnectResult) {
        self.connecting = None;
        match result {
            Ok((client, events, inputs)) => {
                self.retry_delay = RETRY_MIN;
                let selected_name = name_of(&inputs, self.selected);
                self.session = Some(Session {
                    client,
                    events,
                    selected_name,
                });
                self.status.send_modify(|s| {
                    s.connection = Connection::Connected;
                    s.inputs = inputs;
                });
                (self.notify)();
            }
            Err(ConnectError::Retry(e)) => self.schedule_retry(e),
            Err(ConnectError::Fatal(e)) => self.set_connection(Connection::Failed(e)),
        }
    }

    fn schedule_retry(&mut self, error: String) {
        if self.target.is_none() {
            return;
        }
        let after = self.retry_delay;
        self.retry_at = Some(Instant::now() + after);
        self.retry_delay = (after * 2).min(RETRY_MAX);
        self.set_connection(Connection::Retrying { error, after });
    }

    fn drop_connection(&mut self) {
        if let Some(task) = self.connecting.take() {
            task.abort();
        }
        self.session = None;
        self.retry_at = None;
        self.gate.reset();
        self.status.send_modify(|s| s.level_db = f32::NEG_INFINITY);
    }

    fn set_connection(&mut self, connection: Connection) {
        self.status.send_modify(|s| s.connection = connection);
        (self.notify)();
    }

    fn publish(&mut self, level_db: f32) {
        let mouth = self.gate.mouth();
        let receiving = !self.gate.is_stale(self.now());
        self.outputs.set_mouth(mouth);
        let changed = self.status.send_if_modified(|s| {
            let changed = s.mouth != mouth
                || s.level_db.to_bits() != level_db.to_bits()
                || s.receiving != receiving;
            s.mouth = mouth;
            s.level_db = level_db;
            s.receiving = receiving;
            changed
        });
        if changed {
            (self.notify)();
        }
    }
}

async fn connect(port: u16, password: String) -> ConnectResult {
    let config = ConnectConfig {
        host: "127.0.0.1",
        port,
        dangerous: None,
        password: (!password.is_empty()).then_some(password),
        event_subscriptions: Some(
            EventSubscription::INPUTS | EventSubscription::INPUT_VOLUME_METERS,
        ),
        broadcast_capacity: EVENT_BUFFER,
        connect_timeout: Duration::from_secs(5),
    };
    let client = Client::connect_with_config(config)
        .await
        .map_err(classify)?;
    let events = client
        .events()
        .map_err(|e| ConnectError::Retry(e.to_string()))?;
    let inputs = audio_inputs(&client)
        .await
        .map_err(|e| ConnectError::Retry(e.to_string()))?;
    Ok((client, events, inputs))
}

fn classify(error: obws::error::Error) -> ConnectError {
    if let obws::error::Error::Handshake(HandshakeError::ConnectionClosed(Some(details))) = &error
        && u16::from(details.code) == WebSocketCloseCode::AuthenticationFailed as u16
    {
        return ConnectError::Fatal("senha do OBS recusada".to_owned());
    }
    match error {
        obws::error::Error::Connect(_) | obws::error::Error::Timeout => ConnectError::Retry(
            "OBS não respondeu; confira se está aberto e com o servidor WebSocket ativado"
                .to_owned(),
        ),
        other => ConnectError::Retry(other.to_string()),
    }
}

/// Audio-capable inputs. GetInputList has no audio flag, so an input counts
/// as audio if GetInputVolume succeeds: one request per input.
async fn audio_inputs(client: &Client) -> obws::error::Result<Vec<AudioInput>> {
    let mut audio = Vec::new();
    for input in client.inputs().list(None).await? {
        if client
            .inputs()
            .volume(InputId::Uuid(input.id.uuid))
            .await
            .is_ok()
        {
            audio.push(AudioInput {
                uuid: input.id.uuid,
                name: input.id.name,
            });
        }
    }
    Ok(audio)
}

fn name_of(inputs: &[AudioInput], uuid: Option<Uuid>) -> Option<String> {
    let uuid = uuid?;
    inputs
        .iter()
        .find(|i| i.uuid == uuid)
        .map(|i| i.name.clone())
}

/// Level of the named input in one meter event; `None` if OBS did not report
/// it (inactive sources are left out of the event).
fn selected_level(inputs: &[InputVolumeMeter], name: &str) -> Option<f32> {
    let meter = inputs.iter().find(|m| m.name == name)?;
    Some(level_db(&meter.levels, LevelKind::Rms))
}

async fn next_event(session: &mut Option<Session>) -> Option<Event> {
    match session {
        Some(s) => s.events.next().await,
        None => std::future::pending().await,
    }
}

async fn join(task: &mut Option<JoinHandle<ConnectResult>>) -> ConnectResult {
    match task {
        Some(t) => t
            .await
            .unwrap_or_else(|e| Err(ConnectError::Retry(e.to_string()))),
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for(status: &mut watch::Receiver<Status>, f: impl Fn(&Status) -> bool) -> Status {
        tokio::time::timeout(Duration::from_secs(5), status.wait_for(|s| f(s)))
            .await
            .expect("timed out")
            .unwrap()
            .clone()
    }

    #[tokio::test]
    async fn refused_connection_retries_with_backoff() {
        // Port 1 is privileged and unused on a normal desktop: connect is refused.
        let handle = spawn(Outputs::default(), || {});
        let mut status = handle.status.clone();
        handle
            .commands
            .send(Command::Connect {
                port: 1,
                password: String::new(),
            })
            .unwrap();
        let s = wait_for(&mut status, |s| {
            matches!(s.connection, Connection::Retrying { .. })
        })
        .await;
        assert_eq!(
            s.connection,
            Connection::Retrying {
                error:
                    "OBS não respondeu; confira se está aberto e com o servidor WebSocket ativado"
                        .to_owned(),
                after: RETRY_MIN
            }
        );
        handle.commands.send(Command::Disconnect).unwrap();
        wait_for(&mut status, |s| s.connection == Connection::Idle).await;
    }

    #[tokio::test]
    async fn simulation_drives_mouth_and_outputs() {
        let outputs = Outputs::default();
        let handle = spawn(outputs.clone(), || {});
        let mut status = handle.status.clone();
        handle
            .commands
            .send(Command::Simulate(Some(-10.0)))
            .unwrap();
        let s = wait_for(&mut status, |s| s.mouth == Mouth::Open).await;
        assert_eq!(s.level_db, -10.0);
        assert_eq!(outputs.mouth(), Mouth::Open);

        handle.commands.send(Command::Simulate(None)).unwrap();
        wait_for(&mut status, |s| s.mouth == Mouth::Closed).await;
        assert_eq!(outputs.mouth(), Mouth::Closed);
    }

    #[test]
    fn selected_level_reads_only_the_named_input() {
        let meters: Vec<InputVolumeMeter> = serde_json::from_str(
            r#"[{"inputName":"Mic","inputLevelsMul":[[0.1,0.2,0.2]]},
                {"inputName":"Desktop","inputLevelsMul":[[1.0,1.0,1.0]]}]"#,
        )
        .unwrap();
        assert_eq!(selected_level(&meters, "Mic"), Some(-20.0));
        assert_eq!(selected_level(&meters, "Camera"), None);
    }
}
