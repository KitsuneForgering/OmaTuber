//! Local HTTP server for the OBS browser source.
//!
//! Serves a transparent page, the two avatar PNGs on fixed routes, and an SSE
//! stream with the mouth state. It listens on loopback only.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::serve::ListenerExt;
use futures_util::Stream;
use futures_util::stream;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, Interval, interval_at};

use crate::library::Source;
use crate::mouth::Mouth;
use crate::reactions;

pub const DEFAULT_PORT: u16 = 39017;

/// The state is resent this often so the page can detect a dead stream;
/// the page closes the mouth after missing a few of these (see `page.html`).
const HEARTBEAT: Duration = Duration::from_secs(1);

const PAGE: &str = include_str!("page.html");

/// Validated image bytes and their media type.
pub type Served = (Arc<[u8]>, &'static str);

#[derive(Clone, Default)]
pub struct Images {
    pub closed: Option<Served>,
    pub open: Option<Served>,
    /// Bumped on every change so the page can bypass its image cache.
    pub rev: u64,
    /// Pixel sizes of [closed, open], for the shared canvas (`layout.rs`).
    pub sizes: [Option<[u32; 2]>; 2],
    /// Offset of the open image on that canvas, in its pixels.
    pub offset: crate::layout::Offset,
    /// Optional eyes-closed versions of [closed, open], same sizes as those.
    pub blink: [Option<Served>; 2],
}

/// Sentiment and objects drawn instead of or over the base avatar, already
/// resolved by `library::Director`. Asset keys are opaque names registered
/// with `Outputs::set_assets`; the page builds `/asset/<key>` URLs from them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Scene {
    /// Bumped on every change so the page reloads replaced files.
    pub rev: u64,
    pub sentiment: Option<SceneSentiment>,
    pub objects: Vec<SceneObject>,
    /// Changes whenever library files are re-applied. Keys stay the same
    /// when a file is replaced, so without this the scene would look
    /// unchanged and the page would keep showing the old image.
    pub assets: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SceneSentiment {
    pub quiet: Option<String>,
    pub talking: Option<String>,
    /// Changes on every trigger, even of the same sentiment, so the page
    /// loads a fresh image and an APNG/GIF starts from its first frame.
    pub trigger: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SceneObject {
    pub asset: String,
    /// Center and width as fractions of the drawn avatar.
    pub x: f32,
    pub y: f32,
    pub scale: f32,
}

/// Handle the app uses to publish state to connected pages.
#[derive(Clone)]
pub struct Outputs {
    mouth: watch::Sender<Mouth>,
    /// Eyes closed right now (`blink.rs` decides when).
    blink: watch::Sender<bool>,
    images: watch::Sender<Images>,
    scene: watch::Sender<Scene>,
    /// Validated files by key; the only files `/asset/{key}` can return.
    assets: Arc<RwLock<HashMap<String, Served>>>,
    /// Where `/react` and `/base` send requests, once reactions are running.
    reactions: Arc<Mutex<Option<mpsc::UnboundedSender<reactions::Command>>>>,
}

impl Default for Outputs {
    fn default() -> Self {
        Self {
            mouth: watch::Sender::new(Mouth::Closed),
            blink: watch::Sender::new(false),
            images: watch::Sender::new(Images::default()),
            scene: watch::Sender::new(Scene::default()),
            assets: Arc::default(),
            reactions: Arc::default(),
        }
    }
}

impl Outputs {
    pub fn blinking(&self) -> bool {
        *self.blink.borrow()
    }

    pub fn set_blink(&self, on: bool) {
        self.blink
            .send_if_modified(|b| std::mem::replace(b, on) != on);
    }

    /// Sets or clears the eyes-closed image for one mouth state.
    pub fn set_blink_image(&self, mouth: Mouth, image: Option<Served>) {
        self.images.send_modify(|images| {
            images.blink[mouth as usize] = image;
            images.rev += 1;
        });
    }

    pub fn mouth(&self) -> Mouth {
        *self.mouth.borrow()
    }

    pub fn scene(&self) -> Scene {
        self.scene.borrow().clone()
    }

    pub fn set_reaction_handler(&self, tx: mpsc::UnboundedSender<reactions::Command>) {
        *self.reactions.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }

    fn send_reaction(&self, cmd: reactions::Command) -> bool {
        let guard = self.reactions.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().is_some_and(|tx| tx.send(cmd).is_ok())
    }

    /// Replaces the set of servable assets, e.g. after the library changed.
    pub fn set_assets(&self, assets: HashMap<String, Served>) {
        *self.assets.write().unwrap_or_else(|e| e.into_inner()) = assets;
    }

    /// Publishes a new scene; pages are only woken when it differs.
    pub fn set_scene(&self, mut scene: Scene) {
        self.scene.send_if_modified(|current| {
            scene.rev = current.rev;
            if *current == scene {
                return false;
            }
            scene.rev += 1;
            *current = scene;
            true
        });
    }

    /// Publishes the mouth state; pages are only woken when it changes.
    pub fn set_mouth(&self, mouth: Mouth) {
        self.mouth
            .send_if_modified(|m| std::mem::replace(m, mouth) != mouth);
    }

    /// `mime` is `image/png`, `image/apng` or `image/gif`; the route keeps its
    /// `.png` name so the OBS source URL never changes.
    pub fn set_image(&self, mouth: Mouth, bytes: Arc<[u8]>, mime: &'static str) {
        self.images.send_modify(|images| {
            let served = Some((bytes, mime));
            match mouth {
                Mouth::Closed => images.closed = served,
                Mouth::Open => images.open = served,
            }
            images.rev += 1;
        });
    }

    /// Image sizes and the open image's offset, so the page draws both on
    /// one canvas at one scale. Pages are only woken when these change.
    pub fn set_layout(&self, sizes: [Option<[u32; 2]>; 2], offset: crate::layout::Offset) {
        self.images.send_if_modified(|images| {
            if images.sizes == sizes && images.offset == offset {
                return false;
            }
            images.sizes = sizes;
            images.offset = offset;
            true
        });
    }
}

pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await
}

pub async fn serve(listener: TcpListener, outputs: Outputs) -> std::io::Result<()> {
    // SSE sends many tiny writes. With Nagle's algorithm, a write right after
    // an unacknowledged one can wait for the client's delayed ACK (~40 ms on
    // Linux), as measured on the fake OBS socket (tests/common). Mouth
    // changes can come within that window, so the page's socket skips it.
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    axum::serve(listener, router(outputs)).await
}

pub fn router(outputs: Outputs) -> Router {
    Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/closed.png", get(|s| image(s, Mouth::Closed)))
        .route("/open.png", get(|s| image(s, Mouth::Open)))
        .route("/blink-closed.png", get(|s| blink_image(s, Mouth::Closed)))
        .route("/blink-open.png", get(|s| blink_image(s, Mouth::Open)))
        .route("/events", get(events))
        .route("/asset/{key}", get(asset))
        .route("/react/{name}", post(react))
        .route("/base", post(base))
        .with_state(outputs)
}

async fn image(State(outputs): State<Outputs>, mouth: Mouth) -> Response {
    let images = outputs.images.borrow();
    let png = match mouth {
        Mouth::Closed => images.closed.clone(),
        Mouth::Open => images.open.clone(),
    };
    served(png)
}

async fn blink_image(State(outputs): State<Outputs>, mouth: Mouth) -> Response {
    let png = outputs.images.borrow().blink[mouth as usize].clone();
    served(png)
}

fn served(png: Option<Served>) -> Response {
    match png {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-store"),
            ],
            axum::body::Bytes::from_owner(bytes),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn asset(State(outputs): State<Outputs>, AxumPath(key): AxumPath<String>) -> Response {
    let found = outputs
        .assets
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    match found {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-store"),
            ],
            axum::body::Bytes::from_owner(bytes),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Requests that change the avatar must come from this machine's own tools:
/// the custom header forces a CORS preflight that this server never grants,
/// so web pages cannot send it cross-origin, and checking `Host` rejects DNS
/// rebinding (a foreign domain resolving to 127.0.0.1).
fn from_local_tool(headers: &HeaderMap) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    headers.contains_key(TOOL_HEADER) && matches!(name, "127.0.0.1" | "localhost")
}

/// Header `omatuber reagir` sends; see `from_local_tool`.
pub const TOOL_HEADER: &str = "x-omatuber";

async fn react(
    State(outputs): State<Outputs>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> StatusCode {
    if !from_local_tool(&headers) {
        return StatusCode::FORBIDDEN;
    }
    let (reply, rx) = oneshot::channel();
    let cmd = reactions::Command::Trigger {
        target: name,
        source: Source::Shortcut,
        reply: Some(reply),
    };
    if !outputs.send_reaction(cmd) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    match tokio::time::timeout(Duration::from_secs(2), rx).await {
        Ok(Ok(true)) => StatusCode::NO_CONTENT,
        Ok(Ok(false)) => StatusCode::NOT_FOUND,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn base(State(outputs): State<Outputs>, headers: HeaderMap) -> StatusCode {
    if !from_local_tool(&headers) {
        return StatusCode::FORBIDDEN;
    }
    if outputs.send_reaction(reactions::Command::Clear) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn events(
    State(outputs): State<Outputs>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(event_stream(outputs))
}

struct Subscriber {
    mouth: watch::Receiver<Mouth>,
    blink: watch::Receiver<bool>,
    images: watch::Receiver<Images>,
    scene: watch::Receiver<Scene>,
    heartbeat: Interval,
    /// Initial snapshot, popped from the end before waiting for changes.
    pending: Vec<Event>,
}

fn event_stream(outputs: Outputs) -> impl Stream<Item = Result<Event, Infallible>> {
    let mut mouth = outputs.mouth.subscribe();
    let mut blink = outputs.blink.subscribe();
    let mut images = outputs.images.subscribe();
    let mut scene = outputs.scene.subscribe();
    // Popped from the end: images, then scene, then the mouth state, so the
    // page has something to show when the state arrives.
    let pending = vec![
        mouth_event(*mouth.borrow_and_update()),
        blink_event(*blink.borrow_and_update()),
        scene_event(&scene.borrow_and_update()),
        images_event(&images.borrow_and_update()),
    ];
    let sub = Subscriber {
        mouth,
        blink,
        images,
        scene,
        heartbeat: interval_at(Instant::now() + HEARTBEAT, HEARTBEAT),
        pending,
    };
    stream::unfold(sub, |mut sub| async move {
        if let Some(event) = sub.pending.pop() {
            return Some((Ok(event), sub));
        }
        let event = tokio::select! {
            Ok(()) = sub.images.changed() => images_event(&sub.images.borrow_and_update()),
            Ok(()) = sub.scene.changed() => scene_event(&sub.scene.borrow_and_update()),
            Ok(()) = sub.mouth.changed() => mouth_event(*sub.mouth.borrow_and_update()),
            Ok(()) = sub.blink.changed() => blink_event(*sub.blink.borrow_and_update()),
            _ = sub.heartbeat.tick() => mouth_event(*sub.mouth.borrow()),
        };
        Some((Ok(event), sub))
    })
}

fn blink_event(on: bool) -> Event {
    Event::default()
        .event("blink")
        .data(if on { "1" } else { "0" })
}

fn mouth_event(mouth: Mouth) -> Event {
    let data = match mouth {
        Mouth::Closed => "closed",
        Mouth::Open => "open",
    };
    Event::default().event("mouth").data(data)
}

fn scene_event(scene: &Scene) -> Event {
    let data = serde_json::to_string(scene).expect("scene serializes");
    Event::default().event("scene").data(data)
}

fn images_event(images: &Images) -> Event {
    let data = serde_json::json!({
        "rev": images.rev,
        "closed": images.closed.is_some(),
        "open": images.open.is_some(),
        "sizes": images.sizes,
        "offset": images.offset,
        "blink": [images.blink[0].is_some(), images.blink[1].is_some()],
    });
    Event::default().event("images").data(data.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn get(outputs: &Outputs, uri: &str) -> Response {
        let req = Request::get(uri).body(Body::empty()).unwrap();
        router(outputs.clone()).oneshot(req).await.unwrap()
    }

    async fn body(resp: Response) -> Vec<u8> {
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn page_is_served() {
        let resp = get(&Outputs::default(), "/").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(body(resp).await).unwrap();
        assert!(html.contains("EventSource(\"/events\")"));
    }

    #[tokio::test]
    async fn missing_image_is_404() {
        let resp = get(&Outputs::default(), "/open.png").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn animated_images_keep_route_and_get_their_type() {
        let outputs = Outputs::default();
        outputs.set_image(Mouth::Closed, Arc::from(&b"GIF89a"[..]), "image/gif");
        let resp = get(&outputs, "/closed.png").await;
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/gif");
        assert_eq!(body(resp).await, b"GIF89a");
    }

    #[tokio::test]
    async fn serves_exact_bytes_on_fixed_routes_only() {
        let outputs = Outputs::default();
        outputs.set_image(Mouth::Open, Arc::from(&b"\x89PNG-open"[..]), "image/png");
        let resp = get(&outputs, "/open.png?rev=1").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(body(resp).await, b"\x89PNG-open");

        for uri in [
            "/../open.png",
            "/avatar/open.png",
            "/etc/passwd",
            "/closed.png",
        ] {
            assert_eq!(
                get(&outputs, uri).await.status(),
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn events_endpoint_is_sse() {
        let resp = get(&Outputs::default(), "/events").await;
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
    }

    #[tokio::test(start_paused = true)]
    async fn stream_sends_snapshot_then_changes_then_heartbeat() {
        let outputs = Outputs::default();
        outputs.set_image(Mouth::Closed, Arc::from(&b"c"[..]), "image/png");
        let mut body = get(&outputs, "/events").await.into_body();
        let mut next = async || {
            let frame = body.frame().await.unwrap().unwrap();
            String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
        };

        assert_eq!(
            next().await,
            "event: images\ndata: {\"blink\":[false,false],\"closed\":true,\"offset\":[0,0],\"open\":false,\"rev\":1,\"sizes\":[null,null]}\n\n"
        );
        assert_eq!(
            next().await,
            "event: scene\ndata: {\"rev\":0,\"sentiment\":null,\"objects\":[],\"assets\":0}\n\n"
        );
        assert_eq!(next().await, "event: blink\ndata: 0\n\n");
        assert_eq!(next().await, "event: mouth\ndata: closed\n\n");

        outputs.set_blink(true);
        assert_eq!(next().await, "event: blink\ndata: 1\n\n");

        outputs.set_mouth(Mouth::Open);
        assert_eq!(next().await, "event: mouth\ndata: open\n\n");

        outputs.set_image(Mouth::Open, Arc::from(&b"o"[..]), "image/png");
        assert_eq!(
            next().await,
            "event: images\ndata: {\"blink\":[false,false],\"closed\":true,\"offset\":[0,0],\"open\":true,\"rev\":2,\"sizes\":[null,null]}\n\n"
        );

        outputs.set_scene(Scene {
            sentiment: Some(SceneSentiment {
                quiet: Some("s-q".to_owned()),
                talking: None,
                trigger: 1,
            }),
            ..Scene::default()
        });
        assert_eq!(
            next().await,
            "event: scene\ndata: {\"rev\":1,\"sentiment\":{\"quiet\":\"s-q\",\"talking\":null,\"trigger\":1},\"objects\":[],\"assets\":0}\n\n"
        );

        // Nothing changes: with time paused, the next event is the heartbeat.
        assert_eq!(next().await, "event: mouth\ndata: open\n\n");
    }

    #[tokio::test]
    async fn assets_are_served_by_registered_key_only() {
        let outputs = Outputs::default();
        let mut assets = HashMap::new();
        assets.insert("abc-q".to_owned(), (Arc::from(&b"GIF89a"[..]), "image/gif"));
        outputs.set_assets(assets);
        let resp = get(&outputs, "/asset/abc-q?rev=3").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/gif");
        assert_eq!(body(resp).await, b"GIF89a");
        for uri in [
            "/asset/nope",
            "/asset/..%2Fclosed.png",
            "/asset/",
            "/asset/abc-q/x",
        ] {
            assert_eq!(
                get(&outputs, uri).await.status(),
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
        outputs.set_assets(HashMap::new());
        assert_eq!(
            get(&outputs, "/asset/abc-q").await.status(),
            StatusCode::NOT_FOUND,
            "removed"
        );
    }

    #[test]
    fn scene_rev_bumps_only_on_change() {
        let outputs = Outputs::default();
        let mut rx = outputs.scene.subscribe();
        outputs.set_scene(Scene::default());
        assert!(!rx.has_changed().unwrap(), "same scene");
        let with_object = Scene {
            objects: vec![SceneObject {
                asset: "o".to_owned(),
                x: 0.5,
                y: 0.5,
                scale: 0.3,
            }],
            ..Scene::default()
        };
        outputs.set_scene(with_object.clone());
        assert_eq!(rx.borrow_and_update().rev, 1);
        outputs.set_scene(with_object);
        assert!(!rx.has_changed().unwrap(), "rev ignored when comparing");
    }

    #[tokio::test]
    async fn setting_same_mouth_does_not_wake_subscribers() {
        let outputs = Outputs::default();
        let rx = outputs.mouth.subscribe();
        outputs.set_mouth(Mouth::Closed);
        assert!(!rx.has_changed().unwrap());
        outputs.set_mouth(Mouth::Open);
        assert!(rx.has_changed().unwrap());
    }

    #[tokio::test]
    async fn binds_loopback_only() {
        let listener = bind(0).await.unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    }

    #[tokio::test]
    async fn busy_port_is_an_error() {
        let first = bind(0).await.unwrap();
        let port = first.local_addr().unwrap().port();
        assert!(bind(port).await.is_err());
    }
}
