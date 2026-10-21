//! `omatuber reagir <nome>` and `omatuber base`: tell the running app to show
//! a sentiment or return to the base avatar, e.g. from a Hyprland `bind`.
//!
//! A plain HTTP/1.1 request over `std::net`, so the command starts instantly
//! and needs no async runtime.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::server::TOOL_HEADER;

/// Percent-encodes one path segment (RFC 3986 unreserved characters pass).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Sends `POST path` to the app on loopback; returns the HTTP status code.
pub fn post(port: u16, path: &str) -> io::Result<u16> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{TOOL_HEADER}: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::other("resposta inválida do OmaTuber"))
}

/// Runs a subcommand if `args` names one; returns the process exit code.
pub fn run(args: &[String], port: u16) -> Option<i32> {
    let (path, what) = match args {
        [cmd, name] if cmd == "reagir" => {
            (format!("/react/{}", encode_segment(name)), name.as_str())
        }
        [cmd] if cmd == "base" => ("/base".to_owned(), "base"),
        [cmd, ..] if cmd == "reagir" || cmd == "base" => {
            eprintln!("uso: omatuber reagir <nome> | omatuber base");
            return Some(2);
        }
        _ => return None,
    };
    let code = match post(port, &path) {
        Ok(204) => 0,
        Ok(404) => {
            eprintln!("nenhum sentimento ativo chamado {what:?}");
            1
        }
        Ok(503) => {
            eprintln!("o OmaTuber está abrindo; tente de novo");
            1
        }
        Ok(status) => {
            eprintln!("o OmaTuber respondeu {status}");
            1
        }
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            eprintln!("o OmaTuber não está aberto");
            1
        }
        Err(e) => {
            eprintln!("falha ao falar com o OmaTuber: {e}");
            1
        }
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_names_safely() {
        assert_eq!(encode_segment("Animado"), "Animado");
        assert_eq!(encode_segment("Não sei/..?"), "N%C3%A3o%20sei%2F..%3F");
    }

    #[test]
    fn unrelated_args_are_not_subcommands() {
        assert_eq!(run(&[], 1), None);
        assert_eq!(run(&["--help".to_owned()], 1), None);
        assert_eq!(run(&["reagir".to_owned()], 1), Some(2));
    }
}
