//! Small helpers for the tests: a stub HTTP server that answers from a
//! closure, and a `Spi` built from a config text.

// Each test file uses a different part of these helpers, so every file that
// includes this module sees the rest as unused.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use spi::config::{Inputs, Spi};

/// A request as the stub saw it.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub body: String,
}

/// Serve on a free loopback port until the process ends. `answer` gets the
/// method, path and body and returns the status and JSON body. Returns the
/// address (`127.0.0.1:port`) and the requests seen so far.
pub fn stub<F>(answer: F) -> (String, Arc<Mutex<Vec<Seen>>>)
where
    F: Fn(&str, &str, &str) -> (u16, String) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Read the head, then as much body as Content-Length says.
            let (head_end, length) = loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break (buf.len(), 0);
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (p + 4, len);
                }
            };
            while buf.len() < head_end + length {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf[..head_end.min(buf.len())]).to_string();
            let body = String::from_utf8_lossy(&buf[head_end.min(buf.len())..]).to_string();
            let mut first = head.lines().next().unwrap_or("").split_whitespace();
            let method = first.next().unwrap_or("").to_string();
            let path = first.next().unwrap_or("").to_string();
            let (status, reply) = answer(&method, &path, &body);
            log.lock().unwrap().push(Seen { method, path, body });
            let text = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(text.as_bytes());
        }
    });
    (addr, seen)
}

/// A folder of its own for every call.
pub fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("spi-test-{}-{n}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A `Spi` from a config text, with no environment.
pub fn load(dir: &std::path::Path, file: Option<&str>) -> Result<Spi, Vec<String>> {
    load_env(dir, file, &[])
}

pub fn load_env(
    dir: &std::path::Path,
    file: Option<&str>,
    env: &[(&str, &str)],
) -> Result<Spi, Vec<String>> {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let lookup = move |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    Spi::load(&Inputs {
        dir: dir.to_path_buf(),
        file,
        env: &lookup,
        home: "/home/test".into(),
        mac: false,
    })
}
