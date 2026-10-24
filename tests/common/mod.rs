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

use air_minterface::Managed;
use air_minterface::files::sha256_file;
use air_minterface::release::{Download, Source};
use spi::config::{Inputs, Spi};
use spi::exec::{Exec, Ran};

/// The binary `spi install` puts beside the services (the one that runs): the
/// test binary stands in for it.
pub fn exe() -> PathBuf {
    std::env::current_exe().unwrap()
}

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
    stub_bytes(move |method, path, body| {
        let (status, reply) = answer(method, path, body);
        (status, reply.into_bytes())
    })
}

/// [`stub`] for answers that are bytes (a file to download).
pub fn stub_bytes<F>(answer: F) -> (String, Arc<Mutex<Vec<Seen>>>)
where
    F: Fn(&str, &str, &str) -> (u16, Vec<u8>) + Send + 'static,
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
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&reply);
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
        os: "linux",
    })
}

/// The programs spi runs, answered by a closure: it gets the program and its
/// arguments and returns what the program printed. Every call is kept, as one
/// line (`bin arg arg`), with the extra environment as `KEY=value` words after
/// ` env `.
type Answer = Box<dyn Fn(&str, &[String]) -> Result<Ran, String> + Send + Sync>;

pub struct Fake {
    answer: Answer,
    pub calls: Mutex<Vec<String>>,
}

impl Exec for Fake {
    fn run(&self, bin: &str, args: &[String], env: &[(String, String)]) -> Result<Ran, String> {
        let mut line = format!("{bin} {}", args.join(" "));
        if !env.is_empty() {
            let words: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            line.push_str(&format!(" env {}", words.join(" ")));
        }
        self.calls.lock().unwrap().push(line);
        (self.answer)(bin, args)
    }
}

pub fn fake<F>(answer: F) -> Fake
where
    F: Fn(&str, &[String]) -> Result<Ran, String> + Send + Sync + 'static,
{
    Fake {
        answer: Box::new(answer),
        calls: Mutex::new(Vec::new()),
    }
}

/// A program that ended with `code` and printed `stdout` and `stderr`.
pub fn ran(code: i32, stdout: &str, stderr: &str) -> Result<Ran, String> {
    Ok(Ran {
        code: Some(code),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    })
}

/// An ecash-wallet that holds `psp` PSP and `esp` ESP.
pub fn wallet_holding(psp: u64, esp: u64) -> Fake {
    fake(move |bin, args| {
        if bin == "ecash-wallet" && args.last().map(String::as_str) == Some("balance") {
            ran(0, &format!("psp  {psp}\nesp  {esp}\nfsp  0\n"), "")
        } else {
            Err(format!("unexpected call: {bin} {}", args.join(" ")))
        }
    })
}

/// A release source that serves from memory: the tags each program has, and
/// the file text of each build it can fetch. `fetch` writes the build into the
/// folder it is given and says the SHA-256 of what it wrote.
pub struct FakeSource {
    pub tags: Vec<(spi::upgrade::Program, Result<Vec<String>, String>)>,
    /// (program, version, the file's text).
    pub builds: Vec<(spi::upgrade::Program, String, String)>,
    /// `"<program> <version>"` for each fetch asked for, in order.
    pub fetched: Mutex<Vec<String>>,
}

impl FakeSource {
    pub fn new() -> FakeSource {
        FakeSource {
            tags: Vec::new(),
            builds: Vec::new(),
            fetched: Mutex::new(Vec::new()),
        }
    }

    pub fn tags(mut self, p: spi::upgrade::Program, tags: &[&str]) -> FakeSource {
        self.tags
            .push((p, Ok(tags.iter().map(|t| t.to_string()).collect())));
        self
    }

    pub fn unreachable(mut self, p: spi::upgrade::Program, why: &str) -> FakeSource {
        self.tags.push((p, Err(why.to_string())));
        self
    }

    pub fn build(mut self, p: spi::upgrade::Program, version: &str, text: &str) -> FakeSource {
        self.builds.push((p, version.to_string(), text.to_string()));
        self
    }
}

impl Source<spi::upgrade::Program> for FakeSource {
    fn tags(&self, program: spi::upgrade::Program) -> Result<Vec<String>, String> {
        self.tags
            .iter()
            .find(|(p, _)| *p == program)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| Err("the fake source has no such program".into()))
    }

    fn fetch(
        &self,
        program: spi::upgrade::Program,
        version: &str,
        dir: &std::path::Path,
    ) -> Result<Download, String> {
        self.fetched
            .lock()
            .unwrap()
            .push(format!("{} {version}", program.name()));
        let (_, _, text) = self
            .builds
            .iter()
            .find(|(p, v, _)| *p == program && v == version)
            .ok_or_else(|| format!("no release binary of {} {version}", program.name()))?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let file = dir.join(program.name());
        std::fs::write(&file, text).map_err(|e| e.to_string())?;
        let sha256 = sha256_file(&file)?;
        Ok(Download { file, sha256 })
    }
}

/// A `.tar.gz` holding the one file `path` (text `text`), made with the
/// system's tar.
pub fn tarball(path: &str, text: &str) -> Vec<u8> {
    let dir = scratch("tarball");
    let file = dir.join("in").join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
    let made = std::process::Command::new("tar")
        .current_dir(dir.join("in"))
        .args(["-czf", "../out.tar.gz", "."])
        .status()
        .unwrap();
    assert!(made.success());
    std::fs::read(dir.join("out.tar.gz")).unwrap()
}

/// The SHA-256 of `bytes`.
pub fn sha(bytes: &[u8]) -> String {
    let file = scratch("sha").join("f");
    std::fs::write(&file, bytes).unwrap();
    sha256_file(&file).unwrap()
}

pub const LINUX_ARCHIVE: &str = "psp-mint-0.6.0-linux-x86_64.tar.gz";
pub const MAC_ARCHIVE: &str = "psp-mint-0.6.0-macos-aarch64.tar.gz";

/// A stub GitHub with psp-mint 0.6.0 (and a release candidate): an archive for
/// Linux on x86_64 and one for macOS on aarch64 (the same bytes: the requests
/// say which was asked for), and their SHA256SUMS. With `lie` the Linux
/// archive's checksum is wrong.
pub fn github(lie: bool) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let archive = tarball("psp-mint", "psp-mint 0.6.0\n");
    let linux = if lie { "0".repeat(64) } else { sha(&archive) };
    let sums = format!(
        "{linux}  {LINUX_ARCHIVE}\n{}  {MAC_ARCHIVE}\n",
        sha(&archive)
    );
    let me = Arc::new(Mutex::new(String::new()));
    let here = Arc::clone(&me);
    let (addr, seen) = stub_bytes(move |_, path, _| {
        let at = here.lock().unwrap().clone();
        let asset = |n: &str| serde_json::json!({"name": n, "browser_download_url": format!("http://{at}/dl/{n}")});
        let assets = [LINUX_ARCHIVE, MAC_ARCHIVE, "SHA256SUMS"].map(asset);
        let list = serde_json::json!([
            {"tag_name": "v0.7.0-rc1", "prerelease": true},
            {"tag_name": "v0.6.0", "assets": assets},
        ]);
        match path {
            p if p.starts_with("/repos/dav-anderson/psp-mint/releases") => {
                (200, list.to_string().into_bytes())
            }
            "/dl/SHA256SUMS" => (200, sums.clone().into_bytes()),
            p if p.starts_with("/dl/psp-mint-0.6.0-") => (200, archive.clone()),
            _ => (404, Vec::new()),
        }
    });
    *me.lock().unwrap() = addr.clone();
    (addr, seen)
}

/// The system's curl and tar, with a gpg that answers `--verify` with the status
/// lines in `.0` (any other gpg call succeeds and says nothing).
pub struct GpgSays(pub String);

impl Exec for GpgSays {
    fn run(&self, bin: &str, args: &[String], env: &[(String, String)]) -> Result<Ran, String> {
        match (bin, args.iter().any(|a| a == "--verify")) {
            ("gpg", true) => ran(0, &self.0, ""),
            ("gpg", false) => ran(0, "", ""),
            _ => spi::exec::System.run(bin, args, env),
        }
    }
}

/// gpg's status lines for good signatures by the first `n` builders spi trusts.
pub fn signed_by(n: usize) -> String {
    let line =
        |f: &str| format!("[GNUPG:] GOODSIG K u\n[GNUPG:] VALIDSIG {f} 1 2 3 4 5 6 7 8 {f}\n");
    spi::signature::BUILDERS
        .iter()
        .take(n)
        .map(|b| line(b.0))
        .collect()
}
