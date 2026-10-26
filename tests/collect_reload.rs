//! Collecting the PSP psp-mint reissues to the SPI, the wallet's balance, and
//! the commands that must never stop a mint: `reload`, and a `start` that is
//! refused before anything runs.

mod common;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use common::{fake, load, ran, scratch};
use spi::config::{Spi, Which};
use spi::wallet::{self, Balance};
use spi::{commands, run};

fn did() -> String {
    format!("03{}", "cd".repeat(32))
}

/// A config with the PSP's DID written down (the wallet needs it) and the
/// reissued-PSP folder made. Returns the folder.
fn with_revenue(name: &str) -> (Spi, PathBuf) {
    let dir = scratch(name);
    let text = format!("[psp]\ndid = \"{}\"\n", did());
    let spi = load(&dir, Some(&text)).unwrap();
    let revenue = dir.join("psp-mint/regtest/revenue");
    std::fs::create_dir_all(&revenue).unwrap();
    (spi, revenue)
}

fn put(revenue: &Path, name: &str) {
    std::fs::write(revenue.join(name), "{}").unwrap();
}

fn receive_call(spi: &Spi, file: &Path) -> String {
    format!(
        "ecash-wallet --wallet {} --config {} --regtest receive-psp {}",
        spi.wallet_file().display(),
        spi.wallet_config().display(),
        file.display()
    )
}

#[test]
fn collect_takes_in_each_reissued_file_once_and_only_the_new_ones_later() {
    let (spi, revenue) = with_revenue("collect");
    put(&revenue, "b.json");
    put(&revenue, "a.json");
    // A file psp-mint is still writing is not a hand-over file.
    put(&revenue, "c.json.tmp");
    let wallet = fake(|_, _| {
        ran(
            0,
            "received 4 PSP in 1 note(s) (handoff abc, swapping at the mint)\n  swapped\n",
            "",
        )
    });
    let got = wallet::collect(&spi, &wallet).unwrap();
    assert!(!got.failed);
    assert_eq!(got.taken, 2);
    assert_eq!(
        got.lines,
        [
            "a.json: received 4 PSP in 1 note(s) (handoff abc, swapping at the mint)",
            "b.json: received 4 PSP in 1 note(s) (handoff abc, swapping at the mint)",
        ]
    );
    // In name order, each with the wallet, its settings and the network.
    assert_eq!(
        *wallet.calls.lock().unwrap(),
        [
            receive_call(&spi, &revenue.join("a.json")),
            receive_call(&spi, &revenue.join("b.json")),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(wallet::received_file(&spi)).unwrap(),
        "a.json\nb.json\n"
    );
    // The wallet's settings were written first, naming the PSP.
    let settings = std::fs::read_to_string(spi.wallet_config()).unwrap();
    assert!(
        settings.contains(&format!("psp_did = \"{}\"", did())),
        "{settings}"
    );

    // Again: nothing is offered twice.
    let again = fake(|bin, _| Err(format!("{bin} must not be run")));
    let got = wallet::collect(&spi, &again).unwrap();
    assert_eq!(got.taken, 0);
    assert_eq!(
        got.lines,
        [format!(
            "nothing to collect: 2 file(s) in {}, all taken in already",
            revenue.display()
        )]
    );
    assert!(again.calls.lock().unwrap().is_empty());

    // A new file is the only one offered.
    put(&revenue, "d.json");
    let got = wallet::collect(&spi, &wallet).unwrap();
    assert_eq!(got.taken, 1);
    assert_eq!(
        wallet.calls.lock().unwrap().last().unwrap(),
        &receive_call(&spi, &revenue.join("d.json"))
    );
}

#[test]
fn a_file_the_wallet_had_taken_in_already_is_recorded_and_not_an_error() {
    let (spi, revenue) = with_revenue("collect-twice");
    put(&revenue, "a.json");
    let wallet = fake(|_, _| {
        ran(
            1,
            "",
            "error: /x/a.json: handoff 0f0f was imported already; nothing was added\n",
        )
    });
    let got = wallet::collect(&spi, &wallet).unwrap();
    assert!(!got.failed);
    assert_eq!(got.taken, 0);
    assert_eq!(got.lines, ["a.json: the wallet had taken it in already"]);
    assert_eq!(
        std::fs::read_to_string(wallet::received_file(&spi)).unwrap(),
        "a.json\n"
    );
}

#[test]
fn a_file_the_wallet_refuses_fails_the_collect_and_is_offered_again_next_time() {
    let (spi, revenue) = with_revenue("collect-refused");
    put(&revenue, "a.json");
    put(&revenue, "b.json");
    let wallet = fake(|_, args| {
        if args.last().is_some_and(|f| f.ends_with("a.json")) {
            ran(1, "", "error: a.json: not a PSP hand-over file: boom\n")
        } else {
            ran(0, "received 2 PSP in 1 note(s)\n", "")
        }
    });
    let got = wallet::collect(&spi, &wallet).unwrap();
    assert!(got.failed);
    assert_eq!(got.taken, 1);
    assert_eq!(
        got.lines,
        [
            "FAIL: a.json: error: a.json: not a PSP hand-over file: boom",
            "b.json: received 2 PSP in 1 note(s)",
        ]
    );
    // Only the file that was taken in is remembered.
    assert_eq!(
        std::fs::read_to_string(wallet::received_file(&spi)).unwrap(),
        "b.json\n"
    );
    // The command says it failed.
    let out = commands::collect(&spi, &wallet);
    assert!(!out.ok);
}

#[test]
fn collect_says_what_to_set_when_the_psps_did_is_unknown_and_when_nothing_was_reissued() {
    let dir = scratch("collect-nodid");
    let spi = load(&dir, None).unwrap();
    let never = fake(|bin, _| Err(format!("{bin} must not be run")));
    let err = wallet::collect(&spi, &never).unwrap_err();
    assert!(err.contains("write it in spi.toml as [psp] did"), "{err}");
    assert!(never.calls.lock().unwrap().is_empty());
    // Nothing was written either.
    assert!(!dir.join("generated").exists());

    // No folder yet: psp-mint has not reissued anything.
    let dir = scratch("collect-nofolder");
    let spi = load(&dir, Some(&format!("[psp]\ndid = \"{}\"\n", did()))).unwrap();
    let got = wallet::collect(&spi, &never).unwrap();
    assert_eq!(
        got.lines,
        [format!(
            "no PSP has been reissued to the SPI yet ({} does not exist)",
            dir.join("psp-mint/regtest/revenue").display()
        )]
    );
    assert!(!got.failed);
}

#[test]
fn the_wallets_balance_is_read_from_its_psp_and_esp_lines_only() {
    // A line that starts with space belongs to the one above and is not read.
    let text = "psp  50\n       psp  99\n       3 PSP pending\nesp  7\n       12 for a service, redeemable after day 20000\nfsp  0\n";
    assert_eq!(
        wallet::parse_balance(text).unwrap(),
        Balance { psp: 50, esp: 7 }
    );
    let err = wallet::parse_balance("psp  many\nesp  1\n").unwrap_err();
    assert!(
        err.contains("the `psp` line of the wallet's balance is not a number"),
        "{err}"
    );
    let err = wallet::parse_balance("fsp  1\n").unwrap_err();
    assert_eq!(err, "the wallet's balance has no `psp` and `esp` lines");
}

// ---- reload, start, stop ----

/// A process that stands in for a running mint and dies of SIGHUP, as a mint
/// that did not handle it would.
fn stand_in() -> Child {
    Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .spawn()
        .unwrap()
}

fn write_pid(spi: &Spi, which: Which, pid: u32) {
    std::fs::create_dir_all(spi.dir.join("run")).unwrap();
    std::fs::write(spi.pid_file(which), format!("{pid}\n")).unwrap();
}

#[test]
fn reload_writes_the_configs_and_sends_sighup_to_the_mint_spi_started_and_stops_nothing_else() {
    let dir = scratch("reload");
    let spi = load(&dir, Some("[psp.settings.settle]\nretention_days = 120\n")).unwrap();
    let mut psp = stand_in();
    let pid = psp.id();
    write_pid(&spi, Which::Psp, pid);
    let lines = run::reload(&spi).unwrap();
    // The stand-in has no SIGHUP handler, so the signal ends it: that is how the
    // test sees it was sent.
    let status = psp.wait().unwrap();
    assert_eq!(status.signal(), Some(1));
    assert_eq!(lines[0], "wrote the config files again");
    assert!(
        lines[1].starts_with(&format!("psp-mint (pid {pid}): sent SIGHUP; ")),
        "{lines:?}"
    );
    // esp-mint was not started by spi: nothing to signal, and it says how a
    // service is reloaded.
    assert!(
        lines[2].starts_with("esp-mint was not started by spi, so nothing is signalled. A service reloads with `systemctl reload esp-mint`"),
        "{lines:?}"
    );
    // The file the mint reads again has the new value.
    let file = std::fs::read_to_string(spi.generated_config(Which::Psp)).unwrap();
    assert!(file.contains("retention_days = 120"), "{file}");
}

#[test]
fn a_reload_with_a_config_that_is_wrong_signals_nothing() {
    let dir = scratch("reload-bad");
    let spi = load(
        &dir,
        Some("[psp.settings.server]\nbind = \"127.0.0.1:1\"\n"),
    )
    .unwrap();
    let mut psp = stand_in();
    write_pid(&spi, Which::Psp, psp.id());
    let err = run::reload(&spi).unwrap_err();
    assert!(err.contains("psp.settings.server.bind"), "{err}");
    // Still running: not signalled.
    assert!(psp.try_wait().unwrap().is_none());
    psp.kill().unwrap();
    psp.wait().unwrap();
}

#[test]
fn start_is_refused_before_anything_runs_when_the_air_server_has_no_name_or_no_identity() {
    let dir = scratch("start-refused");
    let spi = load(&dir, None).unwrap();
    let err = run::start(&spi, 1).unwrap_err();
    assert!(err.starts_with("[air] name is not set: "), "{err}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

    let dir = scratch("start-noidentity");
    let text = format!("[air]\nname = \"02{}\"\n", "ab".repeat(32));
    let spi = load(&dir, Some(&text)).unwrap();
    let err = run::start(&spi, 1).unwrap_err();
    let identity = dir.join("air/identity.json");
    assert!(
        err.starts_with(&format!(
            "the AIR server's identity file {} does not exist.",
            identity.display()
        )),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn the_air_server_is_started_only_where_it_can_be_and_its_state_is_never_guessed() {
    // Not managed by spi: only its health is read.
    let (up, _) = common::stub(|_, _, _| (200, "ok".to_string()));
    let dir = scratch("air-unmanaged");
    let text = format!("[air]\nmanage = false\nhealth_addr = \"{up}\"\n");
    let spi = load(&dir, Some(&text)).unwrap();
    assert_eq!(
        run::start_air(&spi, 1).unwrap(),
        format!(
            "air_server is not managed by spi (air.manage = false); it answers at http://{up}/"
        )
    );
    let text = "[air]\nmanage = false\nhealth_addr = \"127.0.0.1:1\"\n";
    let spi = load(&scratch("air-unmanaged-down"), Some(text)).unwrap();
    assert_eq!(
        run::start_air(&spi, 1).unwrap_err(),
        "air_server does not answer at http://127.0.0.1:1/ and air.manage is false: start it yourself"
    );

    // Something already answers: spi does not start a second one.
    let dir = scratch("air-running");
    let text = format!("[air]\nhealth_addr = \"{up}\"\n");
    let spi = load(&dir, Some(&text)).unwrap();
    assert_eq!(
        run::start_air(&spi, 1).unwrap_err(),
        format!("something already answers at http://{up}/: stop it first (spi did not start it)")
    );
    assert!(!spi.air_pid_file().exists());

    // No identity file: nothing is started and no pid is kept.
    let dir = scratch("air-noidentity");
    let spi = load(&dir, Some("[air]\nhealth_addr = \"127.0.0.1:1\"\n")).unwrap();
    let err = run::start_air(&spi, 1).unwrap_err();
    assert!(
        err.contains("does not exist. Making it, and keeping a copy of it offline, is yours to do"),
        "{err}"
    );
    assert!(!spi.air_pid_file().exists());

    // A binary that cannot be run is said, with the setting to change.
    let dir = scratch("air-nobinary");
    let text = "[air]\nhealth_addr = \"127.0.0.1:1\"\nbin = \"air-server-that-is-not-installed\"\n";
    let spi = load(&dir, Some(text)).unwrap();
    std::fs::create_dir_all(spi.air_dir()).unwrap();
    std::fs::write(spi.air_identity(false), "{}").unwrap();
    let err = run::start_air(&spi, 1).unwrap_err();
    assert!(
        err.starts_with("cannot run `air-server-that-is-not-installed`: ")
            && err.ends_with("Put it on PATH or set air.bin in spi.toml"),
        "{err}"
    );
    assert!(!spi.air_pid_file().exists());
}

#[test]
fn stop_ends_the_air_server_spi_started_and_forgets_its_pid() {
    let dir = scratch("air-stop");
    let spi = load(&dir, None).unwrap();
    // Nothing was started by spi: it says so and stops nothing.
    assert_eq!(
        run::stop_air(&spi, 1).unwrap(),
        "air_server was not started by spi (no pid file)"
    );
    let child = stand_in();
    std::fs::create_dir_all(spi.dir.join("run")).unwrap();
    std::fs::write(spi.air_pid_file(), format!("{}\n", child.id())).unwrap();
    // A parent that reaps, as the init process does for the real one.
    let mut child = child;
    let reaper = std::thread::spawn(move || child.wait().unwrap());
    assert_eq!(run::stop_air(&spi, 10).unwrap(), "air_server stopped");
    assert!(!spi.air_pid_file().exists());
    assert_eq!(reaper.join().unwrap().signal(), Some(15));
}

// ---- reload and the settings that need a restart ----

/// The lines a reload says, after the settings in `text` were written over
/// whatever the folder held before.
fn reload_with(dir: &Path, text: &str) -> Vec<String> {
    let spi = load(dir, Some(text)).unwrap();
    run::reload(&spi).unwrap()
}

const NOT_STARTED_PSP: &str = "psp-mint was not started by spi, so nothing is signalled. A service reloads with `systemctl reload psp-mint` (Linux) or `launchctl kill HUP system/org.servicepoints.psp-mint` (macOS)";
const NOT_STARTED_ESP: &str = "esp-mint was not started by spi, so nothing is signalled. A service reloads with `systemctl reload esp-mint` (Linux) or `launchctl kill HUP system/org.servicepoints.esp-mint` (macOS)";
const DOWNTIME: &str = "BRIEF DOWNTIME: restart. One process holds the mint's data (sled's exclusive lock), so the new build starts only after the old one has stopped; wallets' AIR requests wait in the mailbox slots and are answered when it is back.";

#[test]
fn the_first_reload_has_nothing_to_compare_and_names_no_setting() {
    let dir = scratch("reload-first");
    let lines = reload_with(&dir, "[psp.settings.rotation]\nperiod_hours = 1440\n");
    assert_eq!(
        lines,
        [
            "wrote the config files again".to_string(),
            NOT_STARTED_PSP.to_string(),
            NOT_STARTED_ESP.to_string()
        ]
    );
}

#[test]
fn a_setting_that_needs_a_restart_is_named_and_nothing_is_restarted() {
    let dir = scratch("reload-restart-one");
    reload_with(&dir, "[psp.settings.rotation]\nperiod_hours = 720\n");
    let lines = reload_with(&dir, "[psp.settings.rotation]\nperiod_hours = 1440\n");
    assert_eq!(
        lines,
        [
            "wrote the config files again".to_string(),
            NOT_STARTED_PSP.to_string(),
            "psp-mint: 1 setting changed that needs a restart (a brief downtime), so the running mint keeps the old value until it is restarted; this reload restarts nothing:".to_string(),
            "  rotation.period_hours: 720 -> 1440".to_string(),
            format!(
                "psp-mint: to apply it, run `spi restart psp-mint` (a mint started with `spi start`: `spi stop`, then `spi start`). {DOWNTIME}"
            ),
            NOT_STARTED_ESP.to_string()
        ]
    );
}

#[test]
fn several_settings_and_both_mints_are_each_named_under_their_own_mint() {
    let dir = scratch("reload-restart-many");
    reload_with(&dir, "");
    let lines = reload_with(
        &dir,
        "[psp.settings.rotation]\nperiod_hours = 1440\nactivation_hours = 2\n[esp.settings.admin]\nmax_request_bytes = 4096\n",
    );
    assert_eq!(
        lines,
        [
            "wrote the config files again".to_string(),
            NOT_STARTED_PSP.to_string(),
            "psp-mint: 2 settings changed that need a restart (a brief downtime), so the running mint keeps the old values until it is restarted; this reload restarts nothing:".to_string(),
            "  rotation.activation_hours: (not set) -> 2".to_string(),
            "  rotation.period_hours: (not set) -> 1440".to_string(),
            format!(
                "psp-mint: to apply them, run `spi restart psp-mint` (a mint started with `spi start`: `spi stop`, then `spi start`). {DOWNTIME}"
            ),
            NOT_STARTED_ESP.to_string(),
            "esp-mint: 1 setting changed that needs a restart (a brief downtime), so the running mint keeps the old value until it is restarted; this reload restarts nothing:".to_string(),
            "  admin.max_request_bytes: (not set) -> 4096".to_string(),
            format!(
                "esp-mint: to apply it, run `spi restart esp-mint` (a mint started with `spi start`: `spi stop`, then `spi start`). {DOWNTIME}"
            ),
        ]
    );
}

#[test]
fn a_setting_that_reloads_live_and_one_that_did_not_change_name_nothing() {
    let dir = scratch("reload-live");
    reload_with(&dir, "[psp.settings.rotation]\nperiod_hours = 720\n");
    // The restart-only value is the same; a live setting changed.
    let lines = reload_with(
        &dir,
        "[psp.settings.rotation]\nperiod_hours = 720\n[psp.settings.scan]\nhealth_grace_secs = 90\n",
    );
    assert_eq!(
        lines,
        [
            "wrote the config files again".to_string(),
            NOT_STARTED_PSP.to_string(),
            NOT_STARTED_ESP.to_string()
        ]
    );
    let file = std::fs::read_to_string(load(&dir, Some("")).unwrap().generated_config(Which::Psp))
        .unwrap();
    assert!(file.contains("health_grace_secs = 90"), "{file}");
}

#[test]
fn collect_removes_old_taken_in_files_and_never_one_not_taken_in() {
    let (spi, revenue) = with_revenue("collect-prune");
    for name in ["a.json", "b.json"] {
        put(&revenue, name);
        let file = std::fs::File::options()
            .write(true)
            .open(revenue.join(name));
        file.unwrap().set_modified(std::time::UNIX_EPOCH).unwrap();
    }
    std::fs::create_dir_all(spi.dir.join("wallet")).unwrap();
    std::fs::write(wallet::received_file(&spi), "a.json\n").unwrap();
    wallet::collect(&spi, &fake(|_, _| ran(0, "ok\n", ""))).unwrap();
    assert!(!revenue.join("a.json").exists() && revenue.join("b.json").exists());
    let received = std::fs::read_to_string(wallet::received_file(&spi)).unwrap();
    assert_eq!(received, "b.json\n");
}

#[test]
fn spi_start_moves_a_log_over_the_limit_aside_and_starts_a_fresh_one() {
    // Fails if `open_log` stops rotating (the log would still hold 1.1 MB and no
    // `.bak` would exist), or if it rotates but leaves no log to append to.
    let dir = scratch("rotate");
    let log = dir.join("psp-mint.log");
    std::fs::write(&log, vec![b'x'; 1_100_000]).unwrap();
    drop(run::open_log(&log, 1, 1).unwrap());
    let baks: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("psp-mint.log.") && n.ends_with(".bak"))
        .collect();
    assert_eq!(baks.len(), 1, "{baks:?}");
    assert_eq!(
        std::fs::metadata(dir.join(&baks[0])).unwrap().len(),
        1_100_000
    );
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
}

#[test]
fn spi_start_leaves_a_log_under_the_limit_alone() {
    // Fails if `open_log` rotates a log that is not over the cap.
    let dir = scratch("rotate-small");
    let log = dir.join("psp-mint.log");
    std::fs::write(&log, "short\n").unwrap();
    drop(run::open_log(&log, 1, 1).unwrap());
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "short\n");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}
