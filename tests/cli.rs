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

#[test]
fn version_names_spi_and_the_library_it_is_built_on_and_reads_nothing() {
    let dir = scratch("version");
    let d = dir.to_str().unwrap();
    // No HOME: reading the settings would fail, so a pass shows none were read.
    let long = run(&["--dir", d, "--version"], &[]);
    let short = run(&["-V"], &[]);
    assert_eq!((long.code, short.code), (Some(0), Some(0)));
    assert_eq!(long.stdout, short.stdout);
    assert_eq!((long.stderr.as_str(), short.stderr.as_str()), ("", ""));
    let lines: Vec<&str> = long.stdout.lines().collect();
    assert_eq!(lines[0], format!("spi {}", env!("CARGO_PKG_VERSION")));
    let sp_wire = lines[1..]
        .iter()
        .find(|l| l.starts_with("sp-wire "))
        .unwrap();
    assert_eq!(sp_wire.split('.').count(), 3, "{sp_wire}");
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_stray_argument_to_the_new_commands_is_refused_naming_it() {
    let dir = scratch("new-commands");
    let d = dir.to_str().unwrap();
    for (command, bad) in [
        ("reload", "now"),
        ("setup", "--json"),
        ("collect", "all"),
        ("versions", "--jsno"),
        ("upgrade", "--dryrun"),
    ] {
        let ran = run(&["--dir", d, command, bad], &[]);
        assert_refused(
            &ran,
            &format!("error: `{bad}` is not an argument of `spi {command}`. Run with --help\n"),
            command,
        );
    }
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn upgrade_refuses_a_program_spi_does_not_upgrade_and_changes_nothing() {
    let dir = scratch("upgrade-name");
    let d = dir.to_str().unwrap();
    let ran = run(
        &["--dir", d, "upgrade", "fsp-mint"],
        &[("HOME", dir.as_path())],
    );
    assert_eq!(ran.code, Some(1));
    assert_eq!(
        ran.stdout,
        "FAIL: `fsp-mint` is not something spi upgrades (bitcoind, air_server, psp-mint, esp-mint)\n"
    );
    assert_eq!(ran.stderr, "");
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- the upgrade flags, the read-only upgrade commands, restart ----

#[test]
fn upgrade_flags_that_do_not_go_together_or_have_no_value_are_refused_first() {
    let dir = scratch("upgrade-flags");
    let d = dir.to_str().unwrap();
    for (args, message) in [
        (
            &["upgrade", "--at", "03:00", "--in", "6h"][..],
            "error: --at and --in cannot be given together\n",
        ),
        (
            &["upgrade", "--keep-pins", "--reset-pins"][..],
            "error: --keep-pins and --reset-pins cannot be given together\n",
        ),
        (&["upgrade", "--at"][..], "error: --at needs a value\n"),
        (&["upgrade", "--in"][..], "error: --in needs a value\n"),
        (
            &["upgrade", "check", "--dry-run"][..],
            "error: `--dry-run` is not an argument of `spi upgrade check`. Run with --help\n",
        ),
        (
            &["upgrade", "cancel", "now"][..],
            "error: `now` is not an argument of `spi upgrade cancel`. Run with --help\n",
        ),
        (
            &["restart"][..],
            "error: usage: spi restart psp-mint|esp-mint\n",
        ),
        (
            &["restart", "psp-mint", "esp-mint"][..],
            "error: usage: spi restart psp-mint|esp-mint\n",
        ),
        (
            &["snapshot", "now"][..],
            "error: `now` is not an argument of `spi snapshot`. Run with --help\n",
        ),
    ] {
        let mut full = vec!["--dir", d];
        full.extend_from_slice(args);
        let ran = run(&full, &[]);
        assert_refused(&ran, message, &args.join(" "));
    }
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_time_or_duration_spi_cannot_read_is_refused_before_anything_is_asked_or_changed() {
    let dir = scratch("upgrade-bad-time");
    let d = dir.to_str().unwrap();
    let home = [("HOME", dir.as_path())];
    let ran = run(&["--dir", d, "upgrade", "--at", "25:00"], &home);
    assert_refused(
        &ran,
        "error: `--at` takes a time like 03:00 (24-hour clock, this machine's local time), not `25:00`\n",
        "--at 25:00",
    );
    let ran = run(&["--dir", d, "upgrade", "--in", "30s"], &home);
    assert_refused(
        &ran,
        "error: `--in` takes a duration like 6h, 90m or 1h30m (days, hours, minutes), not `30s`\n",
        "--in 30s",
    );
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_dry_run_cannot_be_scheduled_and_the_run_says_so() {
    let dir = scratch("upgrade-dry-at");
    let d = dir.to_str().unwrap();
    let ran = run(
        &["--dir", d, "upgrade", "--dry-run", "--at", "03:00"],
        &[("HOME", dir.as_path())],
    );
    assert_eq!(ran.code, Some(1));
    assert_eq!(
        ran.stdout,
        "FAIL: --dry-run changes nothing, so it cannot be scheduled: leave out --at and --in\n"
    );
    assert_eq!(ran.stderr, "");
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn restart_is_for_the_two_mints_and_says_so() {
    let dir = scratch("restart-name");
    let d = dir.to_str().unwrap();
    let ran = run(
        &["--dir", d, "restart", "bitcoind"],
        &[("HOME", dir.as_path())],
    );
    assert_eq!(ran.code, Some(1));
    assert_eq!(
        ran.stdout,
        "FAIL: `bitcoind` is not a mint: `spi restart` restarts psp-mint or esp-mint (the other programs are restarted by `spi upgrade`)\n"
    );
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn upgrade_status_and_cancel_with_nothing_scheduled_change_nothing() {
    let dir = scratch("upgrade-status");
    let d = dir.to_str().unwrap();
    let home = [("HOME", dir.as_path())];
    let ran = run(&["--dir", d, "upgrade", "status"], &home);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (Some(0), "No upgrade is scheduled.\n")
    );
    assert_eq!(ran.stderr, "");
    let ran = run(&["--dir", d, "upgrade", "cancel"], &home);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (Some(0), "No upgrade is scheduled: nothing to cancel.\n")
    );
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn upgrade_check_with_a_release_source_that_cannot_be_reached_says_unknown_and_why_and_still_exits_0()
 {
    let dir = scratch("upgrade-check");
    let d = dir.to_str().unwrap();
    // Both release addresses name a port nothing listens on, so no test reaches
    // the network.
    let config = scratch("upgrade-check-config").join("spi.toml");
    std::fs::write(
        &config,
        "[release]\nurl = \"http://127.0.0.1:1\"\nbitcoin_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let c = config.to_str().unwrap();
    let home = [("HOME", dir.as_path())];
    let ran = run(&["--dir", d, "--config", c, "upgrade", "check"], &home);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let lines: Vec<&str> = ran.stdout.lines().collect();
    assert_eq!(lines.len(), 5, "{}", ran.stdout);
    assert!(lines[2].contains("unknown ("), "{}", lines[2]);
    assert!(
        lines[1].ends_with("no releases published upstream"),
        "{}",
        lines[1]
    );
    assert_eq!(
        lines[4],
        "summary: 0 up to date, 0 upgrade available, 0 pinned, 3 unknown, 1 without upstream releases"
    );
    // --json is the same lines as data.
    let ran = run(
        &["--dir", d, "--config", c, "upgrade", "check", "--json"],
        &home,
    );
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let json: serde_json::Value = serde_json::from_str(&ran.stdout).unwrap();
    let names: Vec<&str> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["bitcoind", "air_server", "psp-mint", "esp-mint"]);
    assert_eq!(json[1]["status"], "no_releases");
    assert_eq!(json[2]["status"], "unknown");
    assert!(snapshot(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}
