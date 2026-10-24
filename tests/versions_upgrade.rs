//! `spi versions` and the steps of `spi upgrade`, with the programs stood in
//! for: a fake runner answers `--version` from the content of the file it is
//! asked to run (so the file says what it is), and records the service
//! manager's commands. A fake release source serves the builds that are
//! downloaded.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use air_interface::Managed;
use air_interface::release::{NoSource, Source};
use common::{Fake, FakeSource, fake, load, ran, scratch};
use spi::commands::{self, InstallOptions, ServiceFiles};
use spi::config::{Inputs, Spi};
use spi::upgrade::{Options, Probe, Program, Upgraded};
use spi::versions;

// ---- spi versions ----

#[test]
fn every_program_and_library_is_a_version_or_unknown_with_the_reason() {
    let dir = scratch("versions");
    // The service folder is empty, so no installed copy is asked as well.
    let text = format!("[service]\nbin_dir = \"{}\"\n", dir.join("bin").display());
    let spi = load(&dir, Some(&text)).unwrap();
    let exec = fake(|bin, args| {
        assert!(args.len() == 1 && args[0] == "--version", "{bin} {args:?}");
        match bin {
            "psp-mint" => ran(
                0,
                "psp-mint 0.5.0\nsp-wire 0.5.1\nmint-core 0.1.3\nair 2.0.3\n",
                "",
            ),
            "esp-mint" => ran(2, "", "error: unknown command \"--version\"\n"),
            "ecash-wallet" => ran(0, "something else 1\n", ""),
            "bitcoind" => ran(0, "Bitcoin Core version v29.1.0\nCopyright\n", ""),
            _ => Err(format!("cannot run `{bin}`: not found")),
        }
    });
    let lines = versions::versions(&spi, &exec);
    let get = |name: &str| {
        lines
            .iter()
            .find(|l| l.name == name)
            .unwrap_or_else(|| panic!("{name} is missing in {lines:?}"))
    };
    assert_eq!(get("spi").version, env!("CARGO_PKG_VERSION"));
    // Fails if `spi versions` stops naming the shared code it is built on.
    assert_eq!(get("air-interface").version, "0.1.0");
    assert_eq!(get("psp-mint").version, "0.5.0");
    assert_eq!(get("psp-mint/sp-wire").version, "0.5.1");
    assert_eq!(get("psp-mint/mint-core").version, "0.1.3");
    assert_eq!(get("psp-mint/air").version, "2.0.3");
    assert_eq!(get("bitcoind").version, "29.1.0");
    // A build that cannot say its version, a program that says something else,
    // and a program that is not there: each is `unknown`, with why.
    let esp = get("esp-mint");
    assert_eq!(esp.version, "unknown");
    assert!(
        esp.note.contains("`esp-mint --version` failed")
            && esp.note.contains("unknown command")
            && esp.note.contains("may be older than --version"),
        "{}",
        esp.note
    );
    let wallet = get("ecash-wallet");
    assert_eq!(wallet.version, "unknown");
    assert!(
        wallet.note.contains("printed `something else 1`"),
        "{}",
        wallet.note
    );
    let air = get("air_server");
    assert_eq!(air.version, "unknown");
    assert!(
        air.note.contains("cannot find `air_server` (air.bin)"),
        "{}",
        air.note
    );
    // Every line is one or the other, and says where it came from.
    for l in &lines {
        assert!(
            l.version == "unknown" || l.version.split('.').count() >= 2,
            "{l:?}"
        );
        assert!(!l.note.is_empty(), "{l:?}");
    }
    let text = versions::render(&lines);
    assert!(text.contains("esp-mint"), "{text}");
}

#[test]
fn air_server_has_a_version_only_when_spi_recorded_this_exact_binary() {
    let dir = scratch("air-version");
    let bin = dir.join("air_server");
    std::fs::write(&bin, "binary one").unwrap();
    let text = format!("[air]\nbin = \"{}\"\nversion = \"1.0.1\"\n", bin.display());
    let spi = load(&dir, Some(&text)).unwrap();

    // No record: only what spi.toml says, and it says so.
    let l = versions::air_line(&spi, "air_server", &bin);
    assert_eq!(l.version, "1.0.1");
    assert!(
        l.note.contains("named in spi.toml, not checked"),
        "{}",
        l.note
    );
    assert_eq!(versions::air_recorded(&spi, &bin), None);

    // Recorded: the version of this file, and the record is the version and the
    // file's SHA-256.
    versions::record_air(&spi, &bin, "1.0.1").unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("air/installed.json")).unwrap()).unwrap();
    assert_eq!(
        record,
        serde_json::json!({
            "version": "1.0.1",
            "sha256": "d4e217a0cce0ef65ed50283685edd58725a17c1c6520c7a815b42583497814b2"
        })
    );
    let l = versions::air_line(&spi, "air_server", &bin);
    assert_eq!(l.version, "1.0.1");
    assert!(
        l.note.contains("recorded by spi for this exact binary"),
        "{}",
        l.note
    );
    assert_eq!(versions::air_recorded(&spi, &bin), Some("1.0.1".into()));

    // Another file there now: unknown, and the line says the record is for
    // another binary.
    std::fs::write(&bin, "binary two").unwrap();
    let l = versions::air_line(&spi, "air_server", &bin);
    assert_eq!(l.version, "unknown");
    assert!(
        l.note.contains("is not the binary spi recorded as 1.0.1"),
        "{}",
        l.note
    );
    assert_eq!(versions::air_recorded(&spi, &bin), None);

    // No record and no version in spi.toml: unknown.
    let dir2 = scratch("air-version-none");
    let bin2 = dir2.join("air_server");
    std::fs::write(&bin2, "binary one").unwrap();
    let spi2 = load(
        &dir2,
        Some(&format!("[air]\nbin = \"{}\"\n", bin2.display())),
    )
    .unwrap();
    let l = versions::air_line(&spi2, "air_server", &bin2);
    assert_eq!(l.version, "unknown");
    assert!(l.note.contains("[air] version is not set"), "{}", l.note);
}

#[test]
fn install_records_the_air_server_it_will_install_and_versions_reads_it_back() {
    let dir = scratch("install-record");
    let bin = dir.join("air_server");
    std::fs::write(&bin, "binary one").unwrap();
    let text = format!("[air]\nbin = \"{}\"\nversion = \"1.0.1\"\n", bin.display());
    let spi = load(&dir, Some(&text)).unwrap();
    // bitcoind is nowhere, which is not a failure when the service files start it.
    let out = commands::install(
        &spi,
        &InstallOptions {
            service_files: ServiceFiles::Into(dir.join("svc")),
            ops_dir: None,
        },
    );
    assert!(out.ok, "{:?}", out.lines);
    assert!(
        out.lines.iter().any(|l| l.starts_with(&format!(
            "recorded {} as air_server 1.0.1 in ",
            std::fs::canonicalize(&bin).unwrap().display()
        ))),
        "{:?}",
        out.lines
    );
    assert_eq!(versions::air_recorded(&spi, &bin), Some("1.0.1".into()));
}

#[test]
fn a_bitcoind_version_may_have_two_or_three_numbers() {
    let problems = load(
        &scratch("btc-version"),
        Some("[bitcoind]\nversion = \"29\"\n"),
    )
    .unwrap_err();
    assert!(
        problems[0].contains("bitcoind.version is `29`: a version is two or three numbers"),
        "{problems:?}"
    );
    for text in [
        "[bitcoind]\nversion = \"29.0rc1\"\n",
        "[psp]\nversion = \"0.7.0-rc1\"\n",
    ] {
        let problems = load(&scratch("rc-pin"), Some(text)).unwrap_err();
        assert!(problems[0].contains("release candidate"), "{problems:?}");
    }
    let spi = load(
        &scratch("btc-version-ok"),
        Some("[bitcoind]\nversion = \"29.1\"\n"),
    )
    .unwrap();
    assert_eq!(spi.bitcoind_version.as_deref(), Some("29.1"));
}

// ---- spi upgrade ----

/// What a mint's restart costs, as the lines say it.
const DOWNTIME: &str = "BRIEF DOWNTIME: restart. One process holds the mint's data (sled's exclusive lock), so the new build starts only after the old one has stopped; wallets' AIR requests wait in the mailbox slots and are answered when it is back.";

/// A service install in a folder of its own: the installed binaries, and the
/// new builds spi.toml's `bin` names. Each file's content is what its
/// `--version` prints. Returns the config and the log of what was run.
struct Layout {
    spi: Spi,
    installed: std::path::PathBuf,
    new: std::path::PathBuf,
}

fn layout(name: &str, mac: bool, extra: &str) -> Layout {
    let dir = scratch(name);
    let installed = dir.join("installed");
    let new = dir.join("new");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    for (n, old, newer) in [
        (
            "bitcoind",
            "Bitcoin Core version v29.0.0\n",
            "Bitcoin Core version v29.1.0\n",
        ),
        ("psp-mint", "psp-mint 0.5.0\n", "psp-mint 0.6.0\n"),
        ("esp-mint", "esp-mint 0.7.0\n", "esp-mint 0.8.0\n"),
        ("air_server", "old air", "new air"),
    ] {
        std::fs::write(installed.join(n), old).unwrap();
        std::fs::write(new.join(n), newer).unwrap();
    }
    let n = new.display();
    let text = format!(
        "[service]\nbin_dir = \"{}\"\n[bitcoind]\nbin = \"{n}/bitcoind\"\nversion = \"29.1\"\n[air]\nbin = \"{n}/air_server\"\nversion = \"1.0.2\"\n[psp]\nbin = \"{n}/psp-mint\"\nversion = \"0.6.0\"\n[esp]\nbin = \"{n}/esp-mint\"\nversion = \"0.8.0\"\n{extra}",
        installed.display()
    );
    let no_env = |_: &str| -> Option<String> { None };
    let spi = Spi::load(&Inputs {
        dir: dir.clone(),
        file: Some(&text),
        env: &no_env,
        home: "/home/test".into(),
        os: if mac { "macos" } else { "linux" },
    })
    .unwrap();
    Layout {
        spi,
        installed,
        new,
    }
}

/// A runner that prints the content of the file it is asked to run for
/// `--version`, and accepts the service manager's commands. Everything goes in
/// `log`, in order. `on_command` sees each service-manager command as one line,
/// so a test can make the stood-in mint answer differently after it.
fn runner_with(
    log: &Arc<Mutex<Vec<String>>>,
    on_command: impl Fn(&str) + Send + Sync + 'static,
) -> Fake {
    let log = Arc::clone(log);
    fake(move |bin, args| {
        let line = format!("{bin} {}", args.join(" "));
        log.lock().unwrap().push(line.clone());
        if bin == "systemctl" || bin == "launchctl" {
            on_command(&line);
            ran(0, "", "")
        } else if args.len() == 1 && args[0] == "--version" {
            Ok(spi::exec::Ran {
                code: Some(0),
                stdout: std::fs::read_to_string(bin).map_err(|e| format!("{bin}: {e}"))?,
                stderr: String::new(),
            })
        } else {
            Err(format!("unexpected: {bin} {args:?}"))
        }
    })
}

fn runner(log: &Arc<Mutex<Vec<String>>>) -> Fake {
    runner_with(log, |_| {})
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn systemctl_calls(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|c| c.starts_with("systemctl") || c.starts_with("launchctl"))
        .cloned()
        .collect()
}

/// Run an upgrade with no release source: the builds are the ones `bin` names.
fn upgrade(
    spi: &Spi,
    programs: &[Program],
    dry_run: bool,
    exec: &Fake,
    probe: &dyn Fn(Program) -> Probe,
) -> Upgraded<Program> {
    with_source(spi, programs, dry_run, &NoSource, exec, probe)
}

fn with_source(
    spi: &Spi,
    programs: &[Program],
    dry_run: bool,
    source: &dyn Source<Program>,
    exec: &Fake,
    probe: &dyn Fn(Program) -> Probe,
) -> Upgraded<Program> {
    spi::upgrade::upgrade(
        spi,
        programs,
        &Options {
            dry_run,
            wait_secs: 0,
            source,
        },
        exec,
        probe,
    )
}

/// A mint that reports `version` on /health, and an `up` for the others.
fn serving(version: &'static str) -> impl Fn(Program) -> Probe {
    move |p| {
        if p.is_mint() {
            Probe::reporting(version)
        } else {
            Probe::up()
        }
    }
}

#[test]
fn a_mint_is_swapped_restarted_and_waited_for_until_health_reports_the_new_version() {
    let l = layout("upgrade-ok", false, "");
    let log = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::clone(&log);
    let healthy = move |p: Program| {
        events.lock().unwrap().push(format!("health {}", p.name()));
        Probe::reporting("0.6.0")
    };
    let done = upgrade(&l.spi, &[Program::Psp], false, &runner(&log), &healthy);
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(done.changes, [Program::Psp]);
    assert_eq!(text(&l.installed.join("psp-mint")), "psp-mint 0.6.0\n");
    assert_eq!(
        text(&l.installed.join("psp-mint.previous")),
        "psp-mint 0.5.0\n"
    );
    assert!(!l.installed.join("psp-mint.new").exists());
    // Every mint upgrade is the restart, said in these exact words.
    assert_eq!(
        done.lines[0],
        format!("psp-mint: 0.5.0 -> 0.6.0 by {DOWNTIME}")
    );
    assert_eq!(done.lines.last().unwrap(), "psp-mint: now 0.6.0, healthy");
    // The restart comes after the swap, and the health check after the restart.
    let log = log.lock().unwrap();
    let restart = log
        .iter()
        .position(|c| c == "systemctl restart psp-mint.service");
    let health = log.iter().rposition(|c| c == "health psp-mint");
    assert!(restart.is_some() && health > restart, "{log:?}");
    // The other programs were not touched.
    assert_eq!(text(&l.installed.join("esp-mint")), "esp-mint 0.7.0\n");
    assert!(!l.installed.join("esp-mint.previous").exists());
}

#[test]
fn esp_mint_is_told_the_same_reason() {
    let l = layout("upgrade-esp-line", false, "");
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(
        &l.spi,
        &[Program::Esp],
        true,
        &runner(&log),
        &serving("0.8.0"),
    );
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(
        done.lines[0],
        format!("esp-mint: 0.7.0 -> 0.8.0 by {DOWNTIME}")
    );
    assert_eq!(done.lines[1], "esp-mint: dry run: nothing was changed");
}

#[test]
fn a_new_build_that_is_not_the_version_named_changes_nothing() {
    let l = layout("upgrade-wrong", false, "");
    std::fs::write(l.new.join("psp-mint"), "psp-mint 0.5.9\n").unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(
        &l.spi,
        &[Program::Psp],
        false,
        &runner(&log),
        &serving("0.6.0"),
    );
    assert!(!done.ok);
    assert!(
        done.lines[0].starts_with("FAIL: psp-mint: ")
            && done.lines[0].contains("is 0.5.9, but spi.toml names 0.6.0: nothing was changed"),
        "{:?}",
        done.lines
    );
    assert_eq!(text(&l.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert!(!l.installed.join("psp-mint.previous").exists());
    assert!(
        systemctl_calls(&log).is_empty(),
        "{:?}",
        systemctl_calls(&log)
    );
}

#[test]
fn a_new_build_that_does_not_become_healthy_is_rolled_back_and_the_rest_wait() {
    let l = layout("upgrade-rollback", false, "");
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(
        &l.spi,
        &[Program::Psp, Program::Esp],
        false,
        &runner(&log),
        &|_| Probe::default(),
    );
    assert!(!done.ok);
    // The old binary is back and was started again; the new one was kept out.
    assert_eq!(text(&l.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert!(!l.installed.join("psp-mint.previous").exists());
    let restarts = log
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.as_str() == "systemctl restart psp-mint.service")
        .count();
    assert_eq!(restarts, 2);
    assert!(
        done.lines.contains(
            &"FAIL: psp-mint: the new build did not become healthy within 0 s; the old one is back and restarted"
                .to_string()
        ),
        "{:?}",
        done.lines
    );
    assert_eq!(
        done.lines.last().unwrap(),
        "stopped: the programs after this one were not touched"
    );
    // esp-mint was never reached.
    assert_eq!(text(&l.installed.join("esp-mint")), "esp-mint 0.7.0\n");
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .all(|c| !c.contains("esp-mint.service")),
        "{:?}",
        log.lock().unwrap()
    );
}

#[test]
fn a_restart_whose_health_still_reports_the_old_version_is_rolled_back() {
    let l = layout("upgrade-old-version", false, "");
    let log = Arc::new(Mutex::new(Vec::new()));
    // The mint answers /health, but with the version it had before: the new
    // build is not the one serving.
    let done = upgrade(
        &l.spi,
        &[Program::Psp],
        false,
        &runner(&log),
        &serving("0.5.0"),
    );
    assert!(!done.ok);
    assert!(
        done.lines.contains(
            &"FAIL: psp-mint: after 0 s /health reported version 0.5.0, not 0.6.0, so the new build is not the one serving; the old one is back and restarted"
                .to_string()
        ),
        "{:?}",
        done.lines
    );
    assert!(
        !done.lines.iter().any(|l| l.contains("now 0.6.0, healthy")),
        "{:?}",
        done.lines
    );
    assert_eq!(text(&l.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert!(!l.installed.join("psp-mint.previous").exists());
    assert_eq!(
        systemctl_calls(&log),
        [
            "systemctl restart psp-mint.service",
            "systemctl restart psp-mint.service"
        ]
    );
    // A /health that reports no version at all cannot be told to be the target.
    let l = layout("upgrade-no-version", false, "");
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(&l.spi, &[Program::Psp], false, &runner(&log), &|_| {
        Probe::up()
    });
    assert!(!done.ok);
    assert!(
        done.lines.contains(
            &"FAIL: psp-mint: after 0 s /health reported no version, so it cannot be told to be 0.6.0; the old one is back and restarted"
                .to_string()
        ),
        "{:?}",
        done.lines
    );
}

#[test]
fn a_program_already_at_its_version_is_left_alone_and_a_dry_run_changes_nothing() {
    let l = layout("upgrade-noop", false, "");
    std::fs::write(l.installed.join("psp-mint"), "psp-mint 0.6.0\n").unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(
        &l.spi,
        &[Program::Psp],
        false,
        &runner(&log),
        &serving("0.6.0"),
    );
    assert!(done.ok);
    assert!(done.changes.is_empty());
    assert_eq!(
        done.lines,
        [
            "psp-mint: already 0.6.0: nothing to do",
            "everything up to date: nothing was stopped"
        ]
    );
    assert!(!l.installed.join("psp-mint.previous").exists());
    assert!(systemctl_calls(&log).is_empty());

    // A dry run says what it would do and does none of it.
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(
        &l.spi,
        &[Program::Esp],
        true,
        &runner(&log),
        &serving("0.8.0"),
    );
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(done.changes, [Program::Esp]);
    assert_eq!(done.lines[1], "esp-mint: dry run: nothing was changed");
    assert_eq!(text(&l.installed.join("esp-mint")), "esp-mint 0.7.0\n");
    assert!(!l.installed.join("esp-mint.previous").exists());
    assert!(systemctl_calls(&log).is_empty());
}

#[test]
fn all_programs_go_one_at_a_time_in_start_order_each_healthy_before_the_next() {
    // bitcoind is the build `bin` names and has no version named, so it is
    // skipped; the others are upgraded.
    let l = layout("upgrade-all", false, "");
    let mut spi = l.spi.clone();
    spi.bitcoind_version = None;
    let log = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::clone(&log);
    let healthy = move |p: Program| {
        events.lock().unwrap().push(format!("health {}", p.name()));
        match p {
            Program::Psp => Probe::reporting("0.6.0"),
            Program::Esp => Probe::reporting("0.8.0"),
            _ => Probe::up(),
        }
    };
    let done = upgrade(&spi, &Program::ALL, false, &runner(&log), &healthy);
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(
        done.lines[0],
        "bitcoind: skipped: spi.toml names no version for it (set its `version`)"
    );
    assert_eq!(done.changes, [Program::Air, Program::Psp, Program::Esp]);
    let log = log.lock().unwrap();
    let acts: Vec<&str> = log
        .iter()
        .map(String::as_str)
        .filter(|c| c.starts_with("systemctl") || c.starts_with("health"))
        .collect();
    assert_eq!(
        acts,
        [
            "systemctl restart sp-air-server.service",
            "health air_server",
            "systemctl restart psp-mint.service",
            "health psp-mint",
            "systemctl restart esp-mint.service",
            "health esp-mint",
        ]
    );
}

#[test]
fn air_server_is_upgraded_by_hash_and_the_record_follows() {
    let l = layout("upgrade-air", false, "");
    let installed_air = l.installed.join("air_server");
    versions::record_air(&l.spi, &installed_air, "1.0.1").unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(&l.spi, &[Program::Air], false, &runner(&log), &|_| {
        Probe::up()
    });
    assert!(done.ok, "{:?}", done.lines);
    assert!(
        done.lines[0].starts_with(
            "air_server: 1.0.1 -> 1.0.2 by BRIEF DOWNTIME: restart. air_server 1.0.1 cannot hand over without an upstream change (file:line below are in its src, tag v1.0.1). One process owns the SQLite store"
        ),
        "{:?}",
        done.lines
    );
    assert_eq!(text(&installed_air), "new air");
    assert_eq!(text(&l.installed.join("air_server.previous")), "old air");
    assert_eq!(
        versions::air_recorded(&l.spi, &installed_air),
        Some("1.0.2".into())
    );
    // air_server cannot be asked its version, so it never is.
    assert!(
        log.lock().unwrap().iter().all(|c| !c.contains("--version")),
        "{:?}",
        log.lock().unwrap()
    );
    // The same file as the one installed is not an upgrade to another version.
    std::fs::copy(&installed_air, l.new.join("air_server")).unwrap();
    let mut spi = l.spi.clone();
    spi.air.version = Some("1.0.3".into());
    let done = upgrade(&spi, &[Program::Air], false, &runner(&log), &|_| {
        Probe::up()
    });
    assert!(!done.ok);
    assert!(
        done.lines[0].contains("is the same file as the installed one, not 1.0.3"),
        "{:?}",
        done.lines
    );
}

#[test]
fn what_cannot_be_upgraded_is_said_and_the_others_still_are_not_touched() {
    // Not installed as a service.
    let l = layout("upgrade-missing", false, "");
    std::fs::remove_file(l.installed.join("psp-mint")).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(&l.spi, &[Program::Psp], false, &runner(&log), &|_| {
        Probe::up()
    });
    assert!(!done.ok);
    assert!(
        done.lines[0].contains("does not exist: `spi upgrade` upgrades the service install"),
        "{:?}",
        done.lines
    );
    // The new build is the installed file itself.
    let l = layout("upgrade-same-file", false, "");
    let mut spi = l.spi.clone();
    spi.psp.bin = l.installed.join("psp-mint").display().to_string();
    let done = upgrade(&spi, &[Program::Psp], false, &runner(&log), &|_| {
        Probe::up()
    });
    assert!(!done.ok);
    assert!(
        done.lines[0].contains("names the installed file"),
        "{:?}",
        done.lines
    );
    // A program spi does not run is skipped.
    let l = layout("upgrade-unmanaged", false, "");
    let mut spi = l.spi.clone();
    spi.manage_bitcoind = false;
    let done = upgrade(&spi, &[Program::Bitcoind], false, &runner(&log), &|_| {
        Probe::up()
    });
    assert!(done.ok);
    assert_eq!(done.lines, ["bitcoind: skipped: spi does not manage it"]);
}

#[test]
fn on_macos_the_restart_goes_through_launchctl() {
    let l = layout("upgrade-mac", true, "");
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = upgrade(
        &l.spi,
        &[Program::Air, Program::Psp],
        false,
        &runner(&log),
        &serving("0.6.0"),
    );
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(
        systemctl_calls(&log),
        [
            "launchctl kickstart -k system/org.servicepoints.air-server",
            "launchctl kickstart -k system/org.servicepoints.psp-mint"
        ]
    );
}

#[test]
fn a_mint_that_is_not_running_is_started_by_the_restart() {
    let l = layout("upgrade-notrunning", false, "");
    // Nothing answers until the service manager has been told to restart it.
    let running = Arc::new(Mutex::new(Probe::default()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let m = Arc::clone(&running);
    let sh = runner_with(&log, move |c| {
        if c.starts_with("systemctl restart") {
            *m.lock().unwrap() = Probe::reporting("0.6.0");
        }
    });
    let asked = Arc::clone(&running);
    let done = upgrade(&l.spi, &[Program::Psp], false, &sh, &move |_| {
        asked.lock().unwrap().clone()
    });
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(
        done.lines[0],
        format!("psp-mint: 0.5.0 -> 0.6.0 by {DOWNTIME}")
    );
    assert_eq!(done.lines.last().unwrap(), "psp-mint: now 0.6.0, healthy");
}

// ---- the newest release, a pin, and a download ----

/// The layout with psp-mint unpinned, `bin` pointing nowhere (a download does
/// not use it), and a source that has psp-mint tagged v0.5.0, v0.6.0 and v0.7.0
/// (and a pre-release of the next) with the builds of 0.6.0 and 0.7.0.
fn downloads(name: &str, pin: Option<&str>) -> (Layout, FakeSource) {
    let mut l = layout(name, false, "");
    l.spi.psp.version = pin.map(str::to_string);
    l.spi.psp.bin = "/nowhere/psp-mint".into();
    let source = FakeSource::new()
        .tags(
            Program::Psp,
            &["v0.5.0", "v0.6.0", "v0.7.0", "v0.8.0-rc1", "nightly"],
        )
        .build(Program::Psp, "0.6.0", "psp-mint 0.6.0\n")
        .build(Program::Psp, "0.7.0", "psp-mint 0.7.0\n");
    (l, source)
}

#[test]
fn a_dry_run_downloads_nothing() {
    let (l, source) = downloads("dry-download", None);
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = with_source(
        &l.spi,
        &[Program::Psp],
        true,
        &source,
        &runner(&log),
        &serving("0.7.0"),
    );
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(
        done.lines[0],
        format!("psp-mint: 0.5.0 -> 0.7.0 by {DOWNTIME}")
    );
    assert_eq!(
        done.lines[1],
        "psp-mint: the build would be downloaded from the release source and checked against its SHA-256"
    );
    assert!(source.fetched.lock().unwrap().is_empty());
    assert_eq!(text(&l.installed.join("psp-mint")), "psp-mint 0.5.0\n");
}

#[test]
fn an_unreachable_source_leaves_a_pinned_program_to_its_bin_and_skips_an_unpinned_one() {
    let source = FakeSource::new().unreachable(Program::Psp, "the repository did not answer");
    // Pinned: the build `bin` names is installed, as before.
    let l = layout("source-down-pin", false, "");
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = with_source(
        &l.spi,
        &[Program::Psp],
        false,
        &source,
        &runner(&log),
        &serving("0.6.0"),
    );
    assert!(done.ok, "{:?}", done.lines);
    assert_eq!(text(&l.installed.join("psp-mint")), "psp-mint 0.6.0\n");
    // Not pinned: there is nothing to install, and it says why.
    let mut l = layout("source-down-nopin", false, "");
    l.spi.psp.version = None;
    let done = with_source(
        &l.spi,
        &[Program::Psp],
        false,
        &source,
        &runner(&log),
        &serving("0.6.0"),
    );
    assert!(done.ok);
    assert_eq!(
        done.lines[0],
        "psp-mint: skipped: spi.toml names no version, and the newest release could not be found: the repository did not answer"
    );
    assert_eq!(text(&l.installed.join("psp-mint")), "psp-mint 0.5.0\n");
}

#[test]
fn air_server_has_no_release_to_look_for_whatever_the_source_says() {
    assert!(!Program::Air.publishes());
    let l = layout("air-no-release", false, "");
    // A source that claims to have air_server tags is not asked about them.
    let source = FakeSource::new().tags(Program::Air, &["v9.9.9"]);
    let log = Arc::new(Mutex::new(Vec::new()));
    let done = with_source(
        &l.spi,
        &[Program::Air],
        true,
        &source,
        &runner(&log),
        &|_| Probe::up(),
    );
    assert!(done.ok, "{:?}", done.lines);
    assert!(done.lines[0].starts_with("air_server: an unknown version -> 1.0.2 by BRIEF DOWNTIME"));
    assert!(source.fetched.lock().unwrap().is_empty());
}
