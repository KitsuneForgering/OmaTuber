//! Local HTTP server for the OBS browser source.
//!
//! Serves a transparent page, the two avatar PNGs on fixed routes, and an SSE
//! stream with the mouth state. It listens on loopback only.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use futures_util::Stream;
use futures_util::stream;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::{Instant, Interval, interval_at};

use crate::mouth::Mouth;

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
}

/// Handle the app uses to publish state to connected pages.
#[derive(Clone)]
pub struct Outputs {
    mouth: watch::Sender<Mouth>,
    images: watch::Sender<Images>,
}

impl Default for Outputs {
    fn default() -> Self {
        Self {
            mouth: watch::Sender::new(Mouth::Closed),
            images: watch::Sender::new(Images::default()),
        }
    }
}

impl Outputs {
    pub fn mouth(&self) -> Mouth {
        *self.mouth.borrow()
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
}

pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await
}

pub async fn serve(listener: TcpListener, outputs: Outputs) -> std::io::Result<()> {
    axum::serve(listener, router(outputs)).await
}

pub fn router(outputs: Outputs) -> Router {
    Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/closed.png", get(|s| image(s, Mouth::Closed)))
        .route("/open.png", get(|s| image(s, Mouth::Open)))
        .route("/events", get(events))
        .with_state(outputs)
}

async fn image(State(outputs): State<Outputs>, mouth: Mouth) -> Response {
    let images = outputs.images.borrow();
    let png = match mouth {
        Mouth::Closed => images.closed.clone(),
        Mouth::Open => images.open.clone(),
    };
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

async fn events(
    State(outputs): State<Outputs>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(event_stream(outputs))
}

struct Subscriber {
    mouth: watch::Receiver<Mouth>,
    images: watch::Receiver<Images>,
    heartbeat: Interval,
    /// Initial snapshot, popped from the end before waiting for changes.
    pending: Vec<Event>,
}

fn event_stream(outputs: Outputs) -> impl Stream<Item = Result<Event, Infallible>> {
    let mut mouth = outputs.mouth.subscribe();
    let mut images = outputs.images.subscribe();
    // Images go out first (last in the vec) so the page has something to
    // show when the state arrives.
    let pending = vec![
        mouth_event(*mouth.borrow_and_update()),
        images_event(&images.borrow_and_update()),
    ];
    let sub = Subscriber {
        mouth,
        images,
        heartbeat: interval_at(Instant::now() + HEARTBEAT, HEARTBEAT),
        pending,
    };
    stream::unfold(sub, |mut sub| async move {
        if let Some(event) = sub.pending.pop() {
            return Some((Ok(event), sub));
        }
        let event = tokio::select! {
            Ok(()) = sub.images.changed() => images_event(&sub.images.borrow_and_update()),
            Ok(()) = sub.mouth.changed() => mouth_event(*sub.mouth.borrow_and_update()),
            _ = sub.heartbeat.tick() => mouth_event(*sub.mouth.borrow()),
        };
        Some((Ok(event), sub))
    })
}

fn mouth_event(mouth: Mouth) -> Event {
    let data = match mouth {
        Mouth::Closed => "closed",
        Mouth::Open => "open",
    };
    Event::default().event("mouth").data(data)
}

fn images_event(images: &Images) -> Event {
    let data = format!(
        r#"{{"rev":{},"closed":{},"open":{}}}"#,
        images.rev,
        images.closed.is_some(),
        images.open.is_some()
    );
    Event::default().event("images").data(data)
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
            "event: images\ndata: {\"rev\":1,\"closed\":true,\"open\":false}\n\n"
        );
        assert_eq!(next().await, "event: mouth\ndata: closed\n\n");

        outputs.set_mouth(Mouth::Open);
        assert_eq!(next().await, "event: mouth\ndata: open\n\n");

        outputs.set_image(Mouth::Open, Arc::from(&b"o"[..]), "image/png");
        assert_eq!(
            next().await,
            "event: images\ndata: {\"rev\":2,\"closed\":true,\"open\":true}\n\n"
        );

        // Nothing changes: with time paused, the next event is the heartbeat.
        assert_eq!(next().await, "event: mouth\ndata: open\n\n");
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
