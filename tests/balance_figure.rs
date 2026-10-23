//! The SPI wallet's balance as psp-mint's burn threshold needs it: what `spi`
//! publishes, in which order it reads the two figures, when it publishes, and
//! where the file goes in a run and in a service.
//!
//! psp-mint is a stand-in HTTP server answering `/audit`; the wallet is the
//! fake program runner. The file format is pinned here as literal JSON, the
//! same text psp-mint's own tests write (`psp_mint::balance::Figure`).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{fake, load, ran, scratch, stub, wallet_holding};
use serde_json::{Value, json};
use spi::balance_figure;
use spi::commands;
use spi::config::{Spi, Which};
use spi::service;
use spi::wallet::Balance;

fn did() -> String {
    format!("03{}", "cd".repeat(32))
}

/// psp-mint's network folder, which psp-mint makes when it first starts, and
/// the figure's default place.
fn network_folder(dir: &Path) -> PathBuf {
    let d = dir.join("psp-mint/regtest");
    std::fs::create_dir_all(d.join("revenue")).unwrap();
    d
}

/// A config with burning on and the PSP's DID written down (the wallet needs it
/// to collect), psp-mint at `bind`.
fn burning(dir: &Path, bind: &str) -> Spi {
    load(
        dir,
        Some(&format!(
            "[psp]\nbind = \"{bind}\"\ndid = \"{}\"\nburn = true\nburn_threshold_psp = 50\n",
            did()
        )),
    )
    .unwrap()
}

/// A stand-in psp-mint whose `/audit` says `reissued`, and a log that every
/// reading of it is written to.
fn psp_mint(reissued: u64, log: &Arc<Mutex<Vec<&'static str>>>) -> String {
    let log = Arc::clone(log);
    let (addr, _) = stub(move |_, path, _| match path {
        "/audit" => {
            log.lock().unwrap().push("audit");
            (
                200,
                json!({"outstanding": 60, "paid": 40, "burned": 10, "reissued": reissued,
                    "network": "regtest"})
                .to_string(),
            )
        }
        _ => (404, "{}".into()),
    });
    addr
}

/// A stand-in esp-mint whose `/audit` has the matured split, so that the audit
/// has no warning of its own.
fn esp_mint() -> String {
    let (addr, _) = stub(|_, path, _| match path {
        "/audit" => (
            200,
            json!({
                "outstanding": 2_000_000_000_000u64,
                "outstanding_matured": 500_000_000_000u64,
                "outstanding_unmatured": 1_500_000_000_000u64,
            })
            .to_string(),
        ),
        _ => (404, "{}".into()),
    });
    addr
}

/// A wallet holding `psp`, that logs `balance` and `receive-psp` calls.
fn wallet(psp: u64, log: &Arc<Mutex<Vec<&'static str>>>) -> common::Fake {
    let log = Arc::clone(log);
    fake(move |_, args| {
        let last = args.last().map(String::as_str).unwrap_or("");
        if last == "balance" {
            log.lock().unwrap().push("balance");
            ran(0, &format!("psp  {psp}\nesp  0\n"), "")
        } else if args.iter().any(|a| a == "receive-psp") {
            log.lock().unwrap().push("receive-psp");
            ran(0, "received 4 PSP in 1 note(s)\n", "")
        } else {
            Err(format!("unexpected call: {}", args.join(" ")))
        }
    })
}

fn figure_in(folder: &Path) -> Value {
    let text = std::fs::read_to_string(folder.join("wallet_balance.json"))
        .unwrap_or_else(|e| panic!("no figure: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn now() -> u64 {
    balance_figure::now_secs()
}

#[test]
fn collect_reads_the_wallet_then_the_mint_and_publishes_both_when_burning_is_on() {
    let dir = scratch("figure-collect");
    let folder = network_folder(&dir);
    std::fs::write(folder.join("revenue/a.json"), "{}").unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let spi = burning(&dir, &psp_mint(7, &log));
    let before = now();
    let out = commands::collect(&spi, &wallet(40, &log));
    let after = now();
    assert!(out.ok, "{:?}", out.lines);

    // The wallet takes the file in, is read, and only then is the mint asked: a
    // note the mint reissues in between is in `reissued` and not in `psp`,
    // which makes psp-mint keep more and burn less, never the other way.
    assert_eq!(*log.lock().unwrap(), ["receive-psp", "balance", "audit"]);

    let v = figure_in(&folder);
    let at = v["at"].as_u64().expect("at");
    assert!(
        (before..=after).contains(&at),
        "at {at} not in {before}..{after}"
    );
    assert_eq!(v, json!({"psp": 40, "reissued": 7, "at": at}));
    assert_eq!(
        std::fs::metadata(folder.join("wallet_balance.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    // Only the figure is left in the folder: no temporary file.
    let mut names: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["revenue", "wallet_balance.json"]);
    assert!(
        out.lines
            .iter()
            .any(|l| l.contains("published the SPI wallet's balance") && l.contains("40 PSP held")),
        "{:?}",
        out.lines
    );

    // Run again with the wallet holding less (it spent) and the mint having
    // reissued more: the figure is replaced, whole.
    let log = Arc::new(Mutex::new(Vec::new()));
    let spi = burning(&dir, &psp_mint(9, &log));
    assert!(commands::collect(&spi, &wallet(3, &log)).ok);
    let v = figure_in(&folder);
    assert_eq!(
        (v["psp"].as_u64(), v["reissued"].as_u64()),
        (Some(3), Some(9))
    );
}

#[test]
fn with_burning_off_nothing_is_published_and_the_wallet_balance_is_not_read() {
    let dir = scratch("figure-off");
    let folder = network_folder(&dir);
    let spi = load(
        &dir,
        Some(&format!(
            "[psp]\ndid = \"{}\"\nbind = \"127.0.0.1:1\"\n",
            did()
        )),
    )
    .unwrap();
    assert!(!spi.burn);
    let log = Arc::new(Mutex::new(Vec::new()));
    let out = commands::collect(&spi, &wallet(40, &log));
    assert!(out.ok, "{:?}", out.lines);
    assert!(!folder.join("wallet_balance.json").exists());
    assert!(!log.lock().unwrap().contains(&"balance"));
}

#[test]
fn a_figure_that_cannot_be_published_fails_the_collect_and_leaves_the_old_one_whole() {
    let dir = scratch("figure-fail");
    let folder = network_folder(&dir);
    // A valid figure, built from the real type.
    let old = serde_json::to_string(&balance_figure::Figure {
        psp: 1,
        reissued: 1,
        at: 1,
    })
    .unwrap();
    std::fs::write(folder.join("wallet_balance.json"), &old).unwrap();

    // psp-mint is not there: nothing listens on port 1.
    let spi = burning(&dir, "127.0.0.1:1");
    let log = Arc::new(Mutex::new(Vec::new()));
    let out = commands::collect(&spi, &wallet(40, &log));
    assert!(!out.ok);
    let said = out.lines.join("\n");
    assert!(
        said.contains("psp-mint reissues every paid unit until it is published")
            && said.contains("cannot reach"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("wallet_balance.json")).unwrap(),
        old
    );

    // The wallet cannot be read.
    let log = Arc::new(Mutex::new(Vec::new()));
    let spi = burning(&dir, &psp_mint(7, &log));
    let broken = fake(|_, args| {
        if args.last().map(String::as_str) == Some("balance") {
            ran(1, "", "error: air.psp_did is not set\n")
        } else {
            ran(0, "received 4 PSP in 1 note(s)\n", "")
        }
    });
    let out = commands::collect(&spi, &broken);
    assert!(!out.ok);
    assert!(
        out.lines
            .join("\n")
            .contains("`ecash-wallet balance` failed"),
        "{:?}",
        out.lines
    );
    assert!(
        log.lock().unwrap().is_empty(),
        "the mint is not asked first"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("wallet_balance.json")).unwrap(),
        old
    );
}

#[test]
fn audit_publishes_the_figure_from_the_same_two_readings_wallet_first() {
    let dir = scratch("figure-audit");
    let folder = network_folder(&dir);
    let log = Arc::new(Mutex::new(Vec::new()));
    let psp = psp_mint(30, &log);
    let esp = esp_mint();
    let spi = load(
        &dir,
        Some(&format!(
            "[psp]\nbind = \"{psp}\"\nburn = true\n[esp]\nbind = \"{esp}\"\n"
        )),
    )
    .unwrap();
    let report = commands::audit(&spi, &wallet(7, &log)).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(*log.lock().unwrap(), ["balance", "audit"]);
    let v = figure_in(&folder);
    let at = v["at"].as_u64().unwrap();
    assert_eq!(v, json!({"psp": 7, "reissued": 30, "at": at}));

    // Burning off: audit publishes nothing, and does not touch the figure.
    std::fs::remove_file(folder.join("wallet_balance.json")).unwrap();
    let spi = load(
        &dir,
        Some(&format!(
            "[psp]\nbind = \"{psp}\"\n[esp]\nbind = \"{esp}\"\n"
        )),
    )
    .unwrap();
    commands::audit(&spi, &wallet_holding(7, 0)).unwrap();
    assert!(!folder.join("wallet_balance.json").exists());
}

#[test]
fn audit_still_prints_when_the_figure_cannot_be_written_and_says_what_psp_mint_does() {
    // The network folder does not exist: psp-mint has not made it.
    let dir = scratch("figure-audit-nofolder");
    let log = Arc::new(Mutex::new(Vec::new()));
    let psp = psp_mint(30, &log);
    let esp = esp_mint();
    let spi = load(
        &dir,
        Some(&format!(
            "[psp]\nbind = \"{psp}\"\nburn = true\n[esp]\nbind = \"{esp}\"\n"
        )),
    )
    .unwrap();
    let report = commands::audit(&spi, &wallet(7, &log)).unwrap();
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("psp-mint reissues every paid unit")
            && report.warnings[0].contains("does not exist"),
        "{}",
        report.warnings[0]
    );
}

#[test]
fn a_figure_needs_a_balance_that_fits_and_a_psp_mint_that_reports_reissued() {
    let balance = |psp: u128| Balance { psp, esp: 0 };
    assert_eq!(
        balance_figure::figure(&balance(5), &json!({"reissued": 2}), 99),
        Ok(balance_figure::Figure {
            psp: 5,
            reissued: 2,
            at: 99
        })
    );
    let why = balance_figure::figure(
        &balance(u128::from(u64::MAX) + 1),
        &json!({"reissued": 2}),
        99,
    )
    .unwrap_err();
    assert!(why.contains("more than a figure can carry"), "{why}");
    for audit in [json!({}), json!({"reissued": "2"}), json!({"reissued": -1})] {
        let why = balance_figure::figure(&balance(5), &audit, 99).unwrap_err();
        assert!(why.contains("no whole-number `reissued`"), "{audit}: {why}");
    }
}

#[test]
fn the_figure_is_in_psp_mints_folder_in_a_run_and_in_spi_share_for_a_service() {
    // A run: psp-mint's own default place.
    let dir = scratch("figure-place");
    let spi = load(&dir, None).unwrap();
    assert_eq!(
        spi.balance_figure_file(),
        dir.join("psp-mint/regtest/wallet_balance.json")
    );

    // The operator points spi at a service: psp-mint's data is under the
    // service's data folder, and the figure goes in the folder made for it.
    let spi = load(
        &dir,
        Some("[psp]\ndata_dir = \"/var/lib/sp-mint/psp-mint\"\n"),
    )
    .unwrap();
    assert_eq!(
        spi.balance_figure_file(),
        PathBuf::from("/var/lib/sp-mint/spi-share/wallet_balance.json")
    );

    // A setting wins over both; it must be absolute.
    let spi = load(
        &dir,
        Some("[psp]\ndata_dir = \"/var/lib/sp-mint/psp-mint\"\nburn_balance_file = \"/srv/x/b.json\"\n"),
    )
    .unwrap();
    assert_eq!(spi.balance_figure_file(), PathBuf::from("/srv/x/b.json"));
    let problems = load(&dir, Some("[psp]\nburn_balance_file = \"b.json\"\n")).unwrap_err();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("psp.burn_balance_file is `b.json`: it must be an absolute path")),
        "{problems:?}"
    );

    // `config show` says where, and from where.
    let shown = spi
        .shown
        .iter()
        .find(|(k, _, _)| k == "psp.burn_balance_file")
        .unwrap();
    assert_eq!(
        (shown.1.as_str(), shown.2.as_str()),
        ("/srv/x/b.json", "spi.toml")
    );
}

#[test]
fn psp_mint_is_told_where_the_figure_is_only_when_burning_is_on_and_not_by_passthrough() {
    let dir = scratch("figure-generated");
    let off = load(&dir, None).unwrap();
    let t = spi::generate::table(&off, Which::Psp).unwrap();
    assert_eq!(t["burn"]["enabled"].as_bool(), Some(false));
    assert!(t["burn"].as_table().unwrap().get("balance_file").is_none());

    let on = load(&dir, Some("[psp]\nburn = true\nburn_threshold_psp = 9\n")).unwrap();
    let t = spi::generate::table(&on, Which::Psp).unwrap();
    assert_eq!(
        t["burn"]["balance_file"].as_str(),
        Some(
            dir.join("psp-mint/regtest/wallet_balance.json")
                .to_str()
                .unwrap()
        )
    );

    let err = load(&dir, Some("[psp.settings.burn]\nbalance_file = \"/x\"\n"))
        .and_then(|s| spi::generate::table(&s, Which::Psp).map_err(|e| vec![e]))
        .unwrap_err()
        .join("\n");
    assert!(err.contains("psp.settings.burn.balance_file"), "{err}");
}

#[test]
fn the_service_files_make_spi_share_and_point_psp_mint_at_it() {
    let dir = scratch("figure-service");
    let want_group_line = "install -d -m 0770 -o \"$user\" -g \"$group\" \"$data_dir/spi-share\"";
    for os in ["linux", "macos"] {
        let spi = spi_for(&dir, os, "[psp]\nburn = true\nburn_threshold_psp = 9\n");
        let plan = service::plan(&spi, None).unwrap();
        let toml = &plan
            .files
            .iter()
            .find(|(n, _)| n == "psp-mint.toml")
            .unwrap()
            .1;
        assert!(
            toml.contains("balance_file = \"/var/lib/sp-mint/spi-share/wallet_balance.json\""),
            "{os}: {toml}"
        );
        let script = &plan.files.last().unwrap().1;
        assert!(script.contains(want_group_line), "{os}: {script}");
        assert!(
            script.contains("chmod 2770 \"$data_dir/spi-share\""),
            "{os}: {script}"
        );
        // The folder is made after the data folders it sits in.
        let at = |needle: &str| script.find(needle).unwrap_or_else(|| panic!("{needle}"));
        assert!(at("\"$data_dir/psp-mint\"") < at(want_group_line));
    }

    // Burning off: the folder is there, psp-mint is not told about it.
    let spi = spi_for(&dir, "linux", "");
    let plan = service::plan(&spi, None).unwrap();
    let toml = &plan
        .files
        .iter()
        .find(|(n, _)| n == "psp-mint.toml")
        .unwrap()
        .1;
    assert!(!toml.contains("balance_file"), "{toml}");
    assert!(plan.files.last().unwrap().1.contains("spi-share"));

    // A figure file written down is used as written.
    let spi = spi_for(
        &dir,
        "linux",
        "[psp]\nburn = true\nburn_balance_file = \"/srv/x/b.json\"\n",
    );
    let plan = service::plan(&spi, None).unwrap();
    let toml = &plan
        .files
        .iter()
        .find(|(n, _)| n == "psp-mint.toml")
        .unwrap()
        .1;
    assert!(toml.contains("balance_file = \"/srv/x/b.json\""), "{toml}");
}

fn spi_for(dir: &Path, os: &str, text: &str) -> Spi {
    let env: Vec<(String, String)> = Vec::new();
    let lookup = move |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    Spi::load(&spi::config::Inputs {
        dir: dir.to_path_buf(),
        file: Some(text),
        env: &lookup,
        home: "/home/test".into(),
        os,
    })
    .unwrap()
}
