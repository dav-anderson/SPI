//! The `spi` binary, run as a process: what it prints and how it exits when it
//! is given a command, a flag or a setting it refuses.
//!
//! Every run gives `--dir` a scratch folder of its own and clears the
//! environment. None of these commands starts, stops or installs anything:
//! each is refused before spi reads its settings, or as it reads them, so each
//! test also checks that nothing was written into the folder. A run that has
//! not ended after a minute is killed and fails the test.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spi-cli-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What a run printed and how it ended.
struct Ran {
    /// The exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `spi args` with an empty environment but the `env` given.
fn run(args: &[&str], env: &[(&str, &Path)]) -> Ran {
    let mut child = Command::new(env!("CARGO_BIN_EXE_spi"))
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut text = String::new();
        out.read_to_string(&mut text).unwrap();
        text
    });
    let err = thread::spawn(move || {
        let mut text = String::new();
        err.read_to_string(&mut text).unwrap();
        text
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("spi {args:?} did not end within a minute");
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ran {
        code: status.code(),
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

/// Every file and folder under `dir`, with the bytes of each file.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(dir: &Path, found: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.insert(path.clone(), None);
                walk(&path, found);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                found.insert(path, Some(bytes));
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(dir, &mut found);
    found
}

/// Assert that `ran` was refused: status 2, nothing on stdout, and this message
/// on stderr.
fn assert_refused(ran: &Ran, message: &str, what: &str) {
    assert_eq!(ran.code, Some(2), "{what}");
    assert_eq!(ran.stdout, "", "{what}");
    assert_eq!(ran.stderr, message, "{what}");
}

#[test]
fn an_unknown_command_is_refused_with_status_2() {
    let dir = scratch("unknown");
    let d = dir.to_str().unwrap();
    let ran = run(&["--dir", d, "frobnicate"], &[]);
    assert_refused(
        &ran,
        "error: unknown command `frobnicate`. Run with --help\n",
        "frobnicate",
    );
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_flag_that_status_or_audit_does_not_take_is_refused_naming_it() {
    let dir = scratch("flags");
    let d = dir.to_str().unwrap();
    for command in ["status", "audit"] {
        // A misspelt --json used to be ignored, and the text report printed.
        let ran = run(&["--dir", d, command, "--jsno"], &[]);
        assert_refused(
            &ran,
            &format!("error: `--jsno` is not an argument of `spi {command}`. Run with --help\n"),
            command,
        );
        // A word that is not a flag is refused the same way.
        let ran = run(&["--dir", d, command, "--json", "extra"], &[]);
        assert_refused(
            &ran,
            &format!("error: `extra` is not an argument of `spi {command}`. Run with --help\n"),
            command,
        );
    }
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn install_with_service_files_both_ways_and_config_without_show_are_refused() {
    let dir = scratch("install-config");
    let d = dir.to_str().unwrap();
    let service_files = dir.join("service");
    let ran = run(
        &[
            "--dir",
            d,
            "install",
            "--service-files",
            service_files.to_str().unwrap(),
            "--no-service-files",
        ],
        &[],
    );
    assert_refused(
        &ran,
        "error: --service-files and --no-service-files cannot be given together\n",
        "install",
    );
    // Refused before the starter spi.toml is written, and before any folder is made.
    assert!(snapshot(&dir).is_empty());

    let ran = run(&["--dir", d, "config"], &[]);
    assert_refused(&ran, "error: usage: spi config show\n", "config");
    let ran = run(&["--dir", d, "config", "hide"], &[]);
    assert_refused(&ran, "error: usage: spi config show\n", "config hide");
    let ran = run(&["--dir", d, "config", "show", "extra"], &[]);
    assert_refused(
        &ran,
        "error: `extra` is not an argument of `spi config show`. Run with --help\n",
        "config show extra",
    );
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_bad_network_and_a_global_flag_with_no_value_are_refused() {
    let dir = scratch("network-values");
    let d = dir.to_str().unwrap();

    // The settings are read, found wrong, and nothing is contacted.
    let ran = run(
        &["--dir", d, "--network", "moon", "status"],
        &[("HOME", dir.as_path())],
    );
    assert_refused(
        &ran,
        "error: the settings are not valid:\n  network: `moon` is not a network: use main, regtest, signet or testnet\n",
        "--network moon",
    );

    // A global flag at the end of the line has nothing to take.
    for flag in ["--dir", "--config", "--network"] {
        let ran = run(&[flag], &[]);
        assert_refused(&ran, &format!("error: {flag} needs a value\n"), flag);
    }
    let ran = run(&["--dir", d, "--config"], &[]);
    assert_refused(&ran, "error: --config needs a value\n", "--dir D --config");
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}
