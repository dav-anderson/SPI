//! `spi logs rotate`, the command the Linux service units run to keep a log
//! under its size cap: run as a process the way a service manager runs it, with
//! no `HOME`, no `spi.toml` and no terminal. (The rotation itself, its naming
//! and its pruning are air-minterface's tests; the units that call this command
//! are checked in `service_files.rs`.)

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spi-logs-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `spi logs rotate args`, with an empty environment: no HOME, as under systemd.
fn rotate(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spi"))
        .args(["logs", "rotate"])
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn big(path: &Path, bytes: usize) {
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

/// The `<name>.<epoch>.bak` files beside `log`, by file name.
fn baks(log: &Path) -> Vec<String> {
    let name = log.file_name().unwrap().to_string_lossy().into_owned();
    let mut found: Vec<String> = std::fs::read_dir(log.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!("{name}.")) && n.ends_with(".bak"))
        .collect();
    found.sort();
    found
}

#[test]
fn a_log_over_the_cap_is_moved_aside_with_no_home_and_no_spi_toml() {
    // Fails if the command reads HOME or spi.toml before it rotates (a service
    // manager runs it with neither), or moves a log that is not over the cap.
    let dir = scratch("move");
    let (over, under) = (dir.join("psp-mint.log"), dir.join("esp-mint.log"));
    big(&over, 1_100_000);
    big(&under, 900_000);
    let out = rotate(&[
        "--max-mb",
        "1",
        &over.display().to_string(),
        &under.display().to_string(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let said = text(&out.stdout);
    assert_eq!(said.lines().count(), 1, "{said}");
    assert!(
        said.starts_with(&format!(
            "{} is over 1 MB: moved to {}.",
            over.display(),
            over.display()
        )),
        "{said}"
    );
    assert!(!over.exists());
    assert_eq!(baks(&over).len(), 1);
    assert_eq!(std::fs::metadata(&under).unwrap().len(), 900_000);
    assert!(baks(&under).is_empty());
}

#[test]
fn running_copies_the_log_aside_and_empties_it_in_place() {
    // Fails if `--running` is ignored: the log would be renamed, and a service
    // still holding it open would go on writing to the moved file.
    let dir = scratch("running");
    let log = dir.join("air_server.log");
    big(&log, 1_100_000);
    let out = rotate(&["--running", "--max-mb", "1", &log.display().to_string()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let said = text(&out.stdout);
    assert!(
        said.starts_with(&format!("{} is over 1 MB: copied to ", log.display()))
            && said.trim_end().ends_with(" and emptied"),
        "{said}"
    );
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
    let kept = baks(&log);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        std::fs::metadata(dir.join(&kept[0])).unwrap().len(),
        1_100_000
    );
}

#[test]
fn only_the_newest_keep_backups_stay_and_the_defaults_are_50_and_4() {
    // Fails if `--keep` is not passed on, or the defaults change from the
    // `[service]` defaults (50 MB, 4 kept).
    let dir = scratch("keep");
    let log = dir.join("bitcoind.log");
    for epoch in 1..=6 {
        std::fs::write(dir.join(format!("bitcoind.log.{epoch}.bak")), "old").unwrap();
    }
    big(&log, 52 * 1024 * 1024);
    let out = rotate(&[&log.display().to_string()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains(" is over 50 MB: moved to "),
        "{}",
        text(&out.stdout)
    );
    // The new one and the three newest old ones: four in all.
    let kept = baks(&log);
    assert_eq!(kept.len(), 4, "{kept:?}");
    assert!(kept.iter().all(|n| n != "bitcoind.log.1.bak"
        && n != "bitcoind.log.2.bak"
        && n != "bitcoind.log.3.bak"));
    let out = rotate(&["--keep", "1", &log.display().to_string()]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(baks(&log).len(), 1);
}

#[test]
fn a_log_that_is_not_there_is_not_a_failure_and_the_next_log_is_still_rotated() {
    // Fails if a log that does not exist yet (a service not yet started) or one
    // that is not a file makes the job fail or stop before the logs after it.
    let dir = scratch("missing");
    let missing = dir.join("not-there.log");
    let folder = dir.join("x.log");
    std::fs::create_dir(&folder).unwrap();
    let fine = dir.join("y.log");
    big(&fine, 1_100_000);
    let out = rotate(&[
        "--max-mb",
        "1",
        &missing.display().to_string(),
        &folder.display().to_string(),
        &fine.display().to_string(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    assert_eq!(
        text(&out.stdout).lines().count(),
        1,
        "{}",
        text(&out.stdout)
    );
    assert!(!missing.exists() && folder.is_dir());
    assert_eq!(baks(&fine).len(), 1);
}

#[test]
fn a_wrong_argument_is_refused_naming_it() {
    // Fails if the command guesses at an argument it does not know.
    let dir = scratch("refused");
    let log = dir.join("a.log").display().to_string();
    for (args, message) in [
        (vec![], "`spi logs rotate` needs at least one log file"),
        (
            vec!["--forever", log.as_str()],
            "`--forever` is not an argument of `spi logs rotate`. Run with --help",
        ),
        (
            vec!["--max-mb", "lots", log.as_str()],
            "--max-mb is `lots`: it must be a whole number",
        ),
        (vec![log.as_str(), "--keep"], "--keep needs a value"),
    ] {
        let out = rotate(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stdout)
        );
        assert!(
            text(&out.stderr).contains(message),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    let other = Command::new(env!("CARGO_BIN_EXE_spi"))
        .args(["logs", "trim", &log])
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(other.status.code(), Some(2));
    assert!(text(&other.stderr).contains("`spi logs` takes `rotate`, not `trim`. Run with --help"));
}

#[test]
fn a_keep_of_0_and_a_max_of_0_mean_no_limit() {
    // Fails if `--keep 0` removes every backup, or `--max-mb 0` moves a log that
    // is any size (0 keeps everything: air-minterface 0.3.0).
    let dir = scratch("zero");
    let log = dir.join("psp-mint.log");
    for epoch in 1..=6 {
        std::fs::write(dir.join(format!("psp-mint.log.{epoch}.bak")), "old").unwrap();
    }
    big(&log, 2_100_000);
    let name = log.display().to_string();
    let out = rotate(&["--max-mb", "0", "--keep", "0", &name]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 2_100_000);
    assert_eq!(baks(&log).len(), 6);
    let out = rotate(&["--running", "--max-mb", "0", "--keep", "0", &name]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 2_100_000);
    assert_eq!(baks(&log).len(), 6);
    // With a size cap but keep 0, the log is moved aside and all 7 stay.
    let out = rotate(&["--max-mb", "1", "--keep", "0", &name]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(baks(&log).len(), 7);
}
