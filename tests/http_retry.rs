//! `spi`'s GET and a connection that a mint loses.
//!
//! A mint that is snapshotting or stopping closes the connections it accepted
//! and had not yet read, so a GET (`/health`, `/audit`) can see a connection
//! closed with no answer. A GET changes nothing, so it is tried again, up to
//! three times in all. A refused connection (nothing is listening) is not.
//!
//! The stand-in closes the connections the script says to and answers the rest.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// For the n-th connection accepted (from 0): `None` closes it unanswered,
/// `Some(body)` answers `200` with that JSON. Past the end of the script every
/// connection is closed.
fn stand_in(script: Vec<Option<&'static str>>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/health", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let n = count.fetch_add(1, Ordering::SeqCst);
            let Some(body) = script.get(n).copied().flatten() else {
                drop(stream);
                continue;
            };
            let mut stream = stream;
            let mut seen = Vec::new();
            let mut chunk = [0u8; 4096];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => seen.extend_from_slice(&chunk[..k]),
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (url, accepted)
}

#[test]
fn a_connection_closed_with_no_answer_is_asked_again() {
    let (url, accepted) = stand_in(vec![None, Some(r#"{"version":"0.6.0"}"#)]);
    let (status, body) = spi::http::get_json(&url).unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, serde_json::json!({"version": "0.6.0"}));
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
}

#[test]
fn two_lost_connections_in_a_row_are_still_asked_again() {
    let (url, accepted) = stand_in(vec![None, None, Some(r#"{"ok":true}"#)]);
    let (status, body) = spi::http::get_json(&url).unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, serde_json::json!({"ok": true}));
    assert_eq!(accepted.load(Ordering::SeqCst), 3);
}

#[test]
fn a_connection_that_keeps_closing_is_given_up_on_after_three_tries() {
    let (url, accepted) = stand_in(vec![]);
    let why = spi::http::get_json(&url).unwrap_err();
    assert!(why.starts_with(&format!("cannot reach {url}:")), "{why}");
    assert_eq!(accepted.load(Ordering::SeqCst), 3);
    assert_eq!(spi::http::TRIES, 3);
}

#[test]
fn nothing_listening_is_reported_at_once() {
    // A port that was free a moment ago and has nobody on it.
    let url = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/health", l.local_addr().unwrap())
    };
    let why = spi::http::get_json(&url).unwrap_err();
    assert!(why.starts_with(&format!("cannot reach {url}:")), "{why}");
}
