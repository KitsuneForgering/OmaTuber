//! Library reactions end to end: HTTP trigger → reactions task → scene.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use omatuber::library::{Ending, Library, Object, Sentiment};
use omatuber::reactions::{self, Command};
use omatuber::server::{self, Outputs, TOOL_HEADER};
use tower::ServiceExt;

fn files(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("omatuber-react-{tag}-{}", std::process::id()));
    let [closed, open] = omatuber::example::write_to(&dir).unwrap();
    (dir, closed, open)
}

fn library(quiet: PathBuf, talking: PathBuf, ms: u32) -> Library {
    let mut s = Sentiment::new("Animado");
    s.quiet = Some(quiet);
    s.talking = Some(talking.clone());
    s.ending = Ending::After { ms };
    let o = Object::new("Caneca", talking);
    s.objects = vec![o.id];
    Library {
        sentiments: vec![s],
        objects: vec![o],
    }
}

async fn post(outputs: &Outputs, uri: &str, host: &str, tool: bool) -> StatusCode {
    let mut req = Request::post(uri).header("host", host);
    if tool {
        req = req.header(TOOL_HEADER, "1");
    }
    let req = req.body(Body::empty()).unwrap();
    server::router(outputs.clone())
        .oneshot(req)
        .await
        .unwrap()
        .status()
}

/// Waits until the reactions task has loaded its first library; requests
/// sent before that get 503 ("o OmaTuber está abrindo"), which is right for
/// the app but would make these tests depend on machine load.
async fn ready(handle: reactions::Handle) -> reactions::Handle {
    let mut status = handle.status.clone();
    tokio::time::timeout(
        Duration::from_secs(10),
        status.wait_for(|s| s.generation >= 1),
    )
    .await
    .expect("reactions task ready")
    .unwrap();
    handle
}

async fn wait_until(outputs: &Outputs, f: impl Fn(&server::Scene) -> bool) -> server::Scene {
    for _ in 0..200 {
        let scene = outputs.scene();
        if f(&scene) {
            return scene;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("scene never matched: {:?}", outputs.scene());
}

#[tokio::test]
async fn shortcut_triggers_by_name_and_returns_to_base() {
    let (dir, closed, open) = files("name");
    let outputs = Outputs::default();
    let lib = library(closed, open, 300);
    let id = lib.sentiments[0].id;
    let handle = ready(reactions::spawn(outputs.clone(), lib, || {})).await;
    wait_until(&outputs, |_| true).await;

    assert_eq!(
        post(&outputs, "/react/animado", "127.0.0.1:39017", true).await,
        StatusCode::NO_CONTENT
    );
    let scene = wait_until(&outputs, |s| s.sentiment.is_some()).await;
    let sentiment = scene.sentiment.unwrap();
    assert_eq!(sentiment.quiet, Some(format!("{id}-q")));
    assert_eq!(sentiment.talking, Some(format!("{id}-t")));
    assert_eq!(
        scene.objects.len(),
        1,
        "object that comes with the sentiment"
    );
    assert_eq!(handle.status.borrow().active.map(|(a, _)| a), Some(id));

    // The asset behind the key is actually served.
    let req = Request::get(format!("/asset/{id}-q"))
        .body(Body::empty())
        .unwrap();
    let resp = server::router(outputs.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Ends by itself after 300 ms.
    wait_until(&outputs, |s| s.sentiment.is_none() && s.objects.is_empty()).await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn base_clears_and_unknown_names_are_404() {
    let (dir, closed, open) = files("base");
    let outputs = Outputs::default();
    let _handle = ready(reactions::spawn(
        outputs.clone(),
        library(closed, open, 60_000),
        || {},
    ))
    .await;
    assert_eq!(
        post(&outputs, "/react/Ninguém", "localhost:39017", true).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(&outputs, "/react/Animado", "localhost", true).await,
        StatusCode::NO_CONTENT
    );
    wait_until(&outputs, |s| s.sentiment.is_some()).await;
    assert_eq!(
        post(&outputs, "/base", "127.0.0.1:39017", true).await,
        StatusCode::NO_CONTENT
    );
    wait_until(&outputs, |s| s.sentiment.is_none()).await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn only_local_tools_may_trigger() {
    let outputs = Outputs::default();
    assert_eq!(
        post(&outputs, "/react/Animado", "127.0.0.1:39017", false).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&outputs, "/react/Animado", "evil.example:39017", true).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&outputs, "/base", "evil.example", true).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&outputs, "/react/Animado", "127.0.0.1:39017", true).await,
        StatusCode::SERVICE_UNAVAILABLE,
        "no reactions task yet"
    );
}

#[tokio::test]
async fn broken_file_keeps_last_valid_version_and_reports() {
    let (dir, closed, open) = files("broken");
    let outputs = Outputs::default();
    let lib = library(closed, open.clone(), 60_000);
    let id = lib.sentiments[0].id;
    let handle = ready(reactions::spawn(outputs.clone(), lib.clone(), || {})).await;

    let mut broken = lib;
    broken.sentiments[0].quiet = Some(dir.join("missing.png"));
    handle.commands.send(Command::Apply(broken)).unwrap();
    for _ in 0..200 {
        if handle.status.borrow().errors.contains_key(&id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(handle.status.borrow().errors[&id].contains("não foi possível ler"));
    let req = Request::get(format!("/asset/{id}-q"))
        .body(Body::empty())
        .unwrap();
    let resp = server::router(outputs.clone()).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "previous quiet image still served"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_reaches_the_running_server() {
    let (dir, closed, open) = files("cli");
    let outputs = Outputs::default();
    let listener = server::bind(0).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(server::serve(listener, outputs.clone()));
    let _handle = ready(reactions::spawn(
        outputs.clone(),
        library(closed, open, 60_000),
        || {},
    ))
    .await;

    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let run =
        move |a: Vec<String>| tokio::task::spawn_blocking(move || omatuber::cli::run(&a, port));
    assert_eq!(run(args(&["reagir", "Animado"])).await.unwrap(), Some(0));
    wait_until(&outputs, |s| s.sentiment.is_some()).await;
    assert_eq!(
        run(args(&["reagir", "Ninguém aqui"])).await.unwrap(),
        Some(1)
    );
    assert_eq!(run(args(&["base"])).await.unwrap(), Some(0));
    wait_until(&outputs, |s| s.sentiment.is_none()).await;
    std::fs::remove_dir_all(dir).unwrap();

    // Nothing listening: refused, reported, exit 1.
    let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let r = tokio::task::spawn_blocking(move || omatuber::cli::run(&args(&["base"]), closed_port));
    assert_eq!(r.await.unwrap(), Some(1));
}

#[tokio::test]
async fn heard_sound_shows_its_sentiment_as_voice() {
    let (dir, closed, open) = files("sound");
    let outputs = Outputs::default();
    let mut lib = library(closed, open, 60_000);
    lib.sentiments[0].sound = Some("risada".to_owned());
    let id = lib.sentiments[0].id;
    let handle = ready(reactions::spawn(outputs.clone(), lib, || {})).await;
    handle
        .commands
        .send(Command::Sound("grito".to_owned()))
        .unwrap();
    handle
        .commands
        .send(Command::Sound("risada".to_owned()))
        .unwrap();
    wait_until(&outputs, |s| s.sentiment.is_some()).await;
    assert_eq!(
        handle.status.borrow().active,
        Some((id, omatuber::library::Source::Voice)),
        "unbound sound ignored, bound one shown as voice"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn retriggering_the_same_sentiment_changes_the_scene() {
    let (dir, closed, open) = files("retrigger");
    let outputs = Outputs::default();
    let _handle = ready(reactions::spawn(
        outputs.clone(),
        library(closed, open, 60_000),
        || {},
    ))
    .await;
    assert_eq!(
        post(&outputs, "/react/Animado", "127.0.0.1", true).await,
        StatusCode::NO_CONTENT
    );
    let first = wait_until(&outputs, |s| s.sentiment.is_some()).await;
    assert_eq!(
        post(&outputs, "/react/Animado", "127.0.0.1", true).await,
        StatusCode::NO_CONTENT
    );
    let first_trigger = first.sentiment.as_ref().unwrap().trigger;
    let second = wait_until(&outputs, |s| {
        s.sentiment
            .as_ref()
            .is_some_and(|x| x.trigger != first_trigger)
    })
    .await;
    assert!(
        second.rev > first.rev,
        "a new scene event goes out, so the page reloads the image"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn replacing_a_file_reaches_the_page_without_retriggering() {
    let (dir, closed, open) = files("replace");
    let outputs = Outputs::default();
    let lib = library(closed.clone(), open.clone(), 60_000);
    let handle = ready(reactions::spawn(outputs.clone(), lib.clone(), || {})).await;
    assert_eq!(
        post(&outputs, "/react/Animado", "127.0.0.1", true).await,
        StatusCode::NO_CONTENT
    );
    let before = wait_until(&outputs, |s| s.sentiment.is_some()).await;

    // Same sentiment, new quiet image file, applied from the editor.
    let mut edited = lib;
    edited.sentiments[0].quiet = Some(open);
    handle.commands.send(Command::Apply(edited)).unwrap();
    let after = wait_until(&outputs, |s| s.rev > before.rev).await;
    assert_eq!(
        after.sentiment.as_ref().unwrap().quiet,
        before.sentiment.as_ref().unwrap().quiet,
        "same key"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
