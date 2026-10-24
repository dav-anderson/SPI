//! spi's side of the scheduled upgrade: the job's names, files and log, which are
//! spi's own (`Spi::UPGRADE_JOB`). The time, the commands, the record and the
//! rules behind them are air-minterface's tests.

mod common;

use std::path::PathBuf;

use common::{load_env, scratch};
use jiff::civil;
use spi::config::{Inputs, Spi};
use spi::schedule::{self, Where};

fn spi_on(dir: &std::path::Path, os: &'static str) -> Spi {
    let no_env = |_: &str| -> Option<String> { None };
    Spi::load(&Inputs {
        dir: dir.to_path_buf(),
        file: Some(""),
        env: &no_env,
        home: "/home/test".into(),
        os,
    })
    .unwrap()
}

#[test]
fn the_job_has_spis_unit_label_log_and_command_on_both_systems() {
    // Fails if spi's job is renamed (a unit or label an installed machine already
    // has), loses its log, or stops running `upgrade run-scheduled`.
    let dir = scratch("job");
    let due = civil::date(2026, 10, 24)
        .at(3, 0, 0, 0)
        .in_tz("America/New_York")
        .unwrap();
    let w = Where {
        exe: PathBuf::from("/usr/local/bin/spi"),
        dir: dir.clone(),
        config: dir.join("spi.toml"),
        unit_dir: dir.join("units"),
    };
    let linux = schedule::render(&spi_on(&dir, "linux"), &w, &due).unwrap();
    assert_eq!(linux[0].0, w.unit_dir.join("sp-upgrade.service"));
    assert_eq!(linux[1].0, w.unit_dir.join("sp-upgrade.timer"));
    let run = format!(
        "ExecStart=/usr/local/bin/spi --dir {} --config {} upgrade run-scheduled",
        dir.display(),
        dir.join("spi.toml").display()
    );
    assert!(linux[0].1.lines().any(|l| l == run), "{}", linux[0].1);
    assert!(
        linux[0]
            .1
            .contains("StandardOutput=append:/var/log/sp-mint/spi-upgrade.log")
    );
    assert!(linux[1].1.lines().any(|l| l == "Unit=sp-upgrade.service"));
    let mac = schedule::render(&spi_on(&dir, "macos"), &w, &due).unwrap();
    assert_eq!(mac[0].0, w.unit_dir.join("org.servicepoints.upgrade.plist"));
    assert!(
        mac[0]
            .1
            .contains("<string>org.servicepoints.upgrade</string>")
    );
    assert!(mac[0].1.contains("<string>run-scheduled</string>"));
}

#[test]
fn the_log_follows_the_service_log_folder_and_all_four_programs_are_named() {
    // Fails if the job's log stops following `[service] log_dir`, or the
    // all-programs sentence of `upgrade status` loses a program.
    let dir = scratch("log");
    let spi = load_env(
        &dir,
        Some("[service]\nlog_dir = \"/var/log/elsewhere\"\n"),
        &[],
    )
    .unwrap();
    assert_eq!(
        schedule::log_file(&spi),
        PathBuf::from("/var/log/elsewhere/spi-upgrade.log")
    );
    assert_eq!(
        schedule::programs_text::<Spi>(&[]),
        "bitcoind, air_server, psp-mint and esp-mint (all four)"
    );
}
