//! Gate worker → outputs → HTTP server → SSE, over a real loopback socket.

use std::time::Duration;

use omatuber::obs::{self, Command};
use omatuber::server::{self, Outputs};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Appends from `stream` to `buf` until `needle` appears in it.
async fn read_until(stream: &mut TcpStream, buf: &mut String, needle: &str) {
    let mut chunk = [0u8; 1024];
    while !buf.contains(needle) {
        let n = timeout(Duration::from_secs(3), stream.read(&mut chunk))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; got {buf:?}"))
            .unwrap();
        assert!(
            n > 0,
            "connection closed waiting for {needle:?}; got {buf:?}"
        );
        buf.push_str(std::str::from_utf8(&chunk[..n]).unwrap());
    }
}

#[tokio::test]
async fn simulated_speech_reaches_the_browser_stream() {
    let outputs = Outputs::default();
    let listener = server::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::serve(listener, outputs.clone()));
    let worker = obs::spawn(outputs, || {});

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut buf = String::new();
    read_until(&mut stream, &mut buf, "data: closed").await;

    worker
        .commands
        .send(Command::Simulate(Some(-10.0)))
        .unwrap();
    read_until(&mut stream, &mut buf, "data: open").await;

    buf.clear();
    worker.commands.send(Command::Simulate(None)).unwrap();
    read_until(&mut stream, &mut buf, "data: closed").await;
}
