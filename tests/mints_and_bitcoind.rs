//! status, audit and install against stand-ins for the mints and bitcoind.
//!
//! These are HTTP servers in the test that answer with the replies the real
//! ones give (the shapes are the ones in the psp-mint and esp-mint READMEs).
//! They are not the real mints: a real psp-mint needs a running bitcoind, and
//! the tests of esp-mint's state are not part of a released tag. See README,
//! "Tests".

mod common;

use std::sync::{Arc, Mutex};

use common::{load, scratch, stub};
use serde_json::{Value, json};
use spi::bitcoind::{Chain, Node, Rpc};
use spi::commands::{self, InstallOptions, ServiceFiles};
use spi::run;

fn cookie_in(dir: &std::path::Path) -> String {
    let c = dir.join("cookie");
    std::fs::write(&c, "__cookie__:secret\n").unwrap();
    c.display().to_string()
}

/// What a stand-in bitcoind returns: its URL, the wallets it has loaded and the
/// wallets on disk.
type Served = (String, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>);

/// A bitcoind stand-in: JSON-RPC by method, with wallets that can be loaded
/// and created. `infos` are the `getblockchaininfo` results, one per call (the
/// last repeats). Returns the address, the loaded wallets and the wallets on disk.
fn bitcoind_seq(infos: Vec<Value>, on_disk: &[&str]) -> Served {
    let loaded = Arc::new(Mutex::new(Vec::<String>::new()));
    let disk = Arc::new(Mutex::new(
        on_disk.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    ));
    let (l, d) = (Arc::clone(&loaded), Arc::clone(&disk));
    let calls = Arc::new(Mutex::new(0usize));
    let (addr, _) = stub(move |_, _, body| {
        let req: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let method = req["method"].as_str().unwrap_or("");
        let name = req["params"][0].as_str().unwrap_or("").to_string();
        let result = match method {
            "getblockchaininfo" => {
                let mut n = calls.lock().unwrap();
                let info = infos[(*n).min(infos.len() - 1)].clone();
                *n += 1;
                info
            }
            "listwallets" => json!(*l.lock().unwrap()),
            "listwalletdir" => {
                json!({"wallets": d.lock().unwrap().iter().map(|n| json!({"name": n})).collect::<Vec<_>>()})
            }
            "loadwallet" => {
                l.lock().unwrap().push(name.clone());
                json!({"name": name})
            }
            "createwallet" => {
                l.lock().unwrap().push(name.clone());
                d.lock().unwrap().push(name.clone());
                json!({"name": name})
            }
            _ => {
                return (
                    404,
                    json!({"result": null, "error": {"code": -32601, "message": "Method not found"}, "id": "spi"})
                        .to_string(),
                );
            }
        };
        (
            200,
            json!({"result": result, "error": null, "id": "spi"}).to_string(),
        )
    });
    (addr, loaded, disk)
}

/// A node that has finished syncing, on `chain`.
fn bitcoind(chain: &'static str, on_disk: &[&str]) -> Served {
    bitcoind_seq(vec![synced(chain)], on_disk)
}

fn synced(chain: &str) -> Value {
    json!({"chain": chain, "blocks": 1000, "headers": 1000, "verificationprogress": 0.99999, "initialblockdownload": false})
}

fn syncing(chain: &str, blocks: u64, headers: u64, progress: f64) -> Value {
    json!({"chain": chain, "blocks": blocks, "headers": headers, "verificationprogress": progress, "initialblockdownload": true})
}

fn mint(health: u16, health_body: Value, audit_body: Value) -> String {
    let (addr, _) = stub(move |_, path, _| match path {
        "/health" => (health, health_body.to_string()),
        "/audit" => (200, audit_body.to_string()),
        _ => (404, "{}".into()),
    });
    addr
}

fn config(dir: &std::path::Path, btc: &str, psp: &str, esp: &str) -> spi::config::Spi {
    let text = format!(
        "network = \"regtest\"\n[bitcoind]\nrpc_url = \"http://{btc}\"\ncookie = \"{}\"\n[psp]\nbind = \"{psp}\"\n[esp]\nbind = \"{esp}\"\nadmin_bind = \"127.0.0.1:1\"\n",
        cookie_in(dir)
    );
    load(dir, Some(&text)).unwrap()
}

#[test]
fn audit_reads_both_mints_and_itemizes_matured_and_unmatured() {
    let dir = scratch("audit");
    let psp = mint(
        200,
        json!({"status": "ok", "network": "regtest"}),
        json!({"issued": 100, "redeemed": 40, "outstanding": 60, "network": "regtest"}),
    );
    let esp = mint(
        200,
        json!({"status": "ok", "network": "regtest"}),
        json!({
            "issued": 3_000_000_000_000u64,
            "redeemed": 1_000_000_000_000u64,
            "outstanding": 2_000_000_000_000u64,
            "outstanding_matured": 500_000_000_000u64,
            "outstanding_unmatured": 1_500_000_000_000u64,
            "requests_unfinished": 0,
            "as_of_day": 20_000,
        }),
    );
    let spi = config(&dir, "127.0.0.1:1", &psp, &esp);
    let r = commands::audit(&spi).unwrap();
    assert_eq!(r.psp_outstanding.units, "60");
    assert_eq!(r.psp_outstanding.usd, "6.000000000000");
    assert_eq!(r.esp_matured.as_ref().unwrap().psp, "5.00000000000");
    assert_eq!(r.esp_unmatured.as_ref().unwrap().psp, "15.00000000000");
    // 60 PSP + 20 PSP of ESP = 80 PSP = $8.
    assert_eq!(r.total.psp, "80.00000000000");
    assert_eq!(r.total.usd, "8.000000000000");
    assert!(r.warnings.is_empty());
}

#[test]
fn audit_without_the_split_gives_the_total_and_says_so() {
    let dir = scratch("audit-nosplit");
    let psp = mint(200, json!({"status": "ok"}), json!({"outstanding": 10}));
    let esp = mint(
        200,
        json!({"status": "ok"}),
        json!({"outstanding": 100_000_000_000u64}),
    );
    let spi = config(&dir, "127.0.0.1:1", &psp, &esp);
    let r = commands::audit(&spi).unwrap();
    assert!(r.esp_matured.is_none());
    assert_eq!(r.esp_total.psp, "1.00000000000");
    assert_eq!(r.total.psp, "11.00000000000");
    assert_eq!(r.warnings.len(), 1);
}

#[test]
fn audit_refuses_a_mint_on_another_network_a_bad_status_and_a_mint_that_is_down() {
    let dir = scratch("audit-bad");
    let good = mint(200, json!({"status": "ok"}), json!({"outstanding": 1}));
    let other = mint(
        200,
        json!({"status": "ok"}),
        json!({"outstanding": 1, "network": "main"}),
    );
    let spi = config(&dir, "127.0.0.1:1", &other, &good);
    assert!(commands::audit(&spi).unwrap_err().contains("is on main"));
    // Nothing listens on port 1.
    let spi = config(&dir, "127.0.0.1:1", &good, "127.0.0.1:1");
    assert!(commands::audit(&spi).unwrap_err().contains("cannot reach"));
}

#[test]
fn status_reports_each_part_and_a_network_mismatch() {
    let dir = scratch("status");
    let (btc, _, _) = bitcoind("regtest", &["psp-regtest"]);
    // The wallet is on disk but not loaded yet: status says so.
    let psp = mint(
        200,
        json!({"status": "ok", "network": "regtest"}),
        json!({}),
    );
    let esp = mint(
        200,
        json!({"status": "ok", "network": "regtest"}),
        json!({}),
    );
    let spi = config(&dir, &btc, &psp, &esp);
    let parts = commands::status(&spi);
    assert_eq!(
        parts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["bitcoind", "psp-mint", "esp-mint"]
    );
    assert!(
        !parts[0].ok && parts[0].detail.contains("NOT loaded"),
        "{parts:?}"
    );
    assert!(parts[1].ok && parts[2].ok);

    let (btc, loaded, _) = bitcoind("regtest", &[]);
    loaded.lock().unwrap().push("psp-regtest".into());
    let spi = config(&dir, &btc, &psp, &esp);
    assert!(commands::status(&spi).iter().all(|c| c.ok));

    // bitcoind on another chain, the ESP mint unhealthy, the PSP mint on another network.
    let (btc_main, loaded, _) = bitcoind("main", &[]);
    loaded.lock().unwrap().push("psp-regtest".into());
    let sick = mint(
        503,
        json!({"status": "broken", "reason": "locks poisoned"}),
        json!({}),
    );
    let wrong = mint(200, json!({"status": "ok", "network": "main"}), json!({}));
    let spi = config(&dir, &btc_main, &wrong, &sick);
    let parts = commands::status(&spi);
    assert!(!parts[0].ok && parts[0].detail.contains("config says regtest"));
    assert!(!parts[1].ok && parts[1].detail.contains("network main"));
    assert!(!parts[2].ok && parts[2].detail.contains("locks poisoned"));
}

/// Install without service files: these tests are about bitcoind and the
/// wallet. The service files have their own tests (`service_files.rs`).
fn options(_dir: &std::path::Path) -> InstallOptions {
    InstallOptions {
        service_files: ServiceFiles::Skip,
        ops_dir: None,
    }
}

#[test]
fn install_makes_the_folders_writes_the_configs_and_creates_the_wallet_once() {
    let dir = scratch("install");
    let (btc, loaded, disk) = bitcoind("regtest", &[]);
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    let out = commands::install(&spi, &options(&dir));
    assert!(out.ok, "{:?}", out.lines);
    for f in ["generated/psp-mint.toml", "generated/esp-mint.toml"] {
        let text = std::fs::read_to_string(dir.join(f)).unwrap();
        assert!(text.starts_with("# Generated by spi"), "{f}");
    }
    for d in ["psp-mint", "esp-mint", "run", "logs"] {
        assert!(dir.join(d).is_dir(), "{d}");
    }
    assert!(out.lines.iter().any(|l| l.contains("created it")));
    assert_eq!(*loaded.lock().unwrap(), ["psp-regtest"]);
    assert_eq!(*disk.lock().unwrap(), ["psp-regtest"]);
    // Again: the wallet is loaded, nothing is created twice.
    let out = commands::install(&spi, &options(&dir));
    assert!(
        out.ok && out.lines.iter().any(|l| l.contains("is loaded")),
        "{:?}",
        out.lines
    );
    assert_eq!(disk.lock().unwrap().len(), 1);
}

#[test]
fn install_loads_a_wallet_that_is_on_disk() {
    let dir = scratch("install-load");
    let (btc, loaded, _) = bitcoind("regtest", &["psp-regtest"]);
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    let out = commands::install(&spi, &options(&dir));
    assert!(
        out.ok && out.lines.iter().any(|l| l.contains("was loaded")),
        "{:?}",
        out.lines
    );
    assert_eq!(*loaded.lock().unwrap(), ["psp-regtest"]);
}

#[test]
fn install_will_not_create_a_wallet_when_told_not_to() {
    let dir = scratch("install-nocreate");
    let (btc, _, disk) = bitcoind("regtest", &[]);
    let text = format!(
        "[bitcoind]\nrpc_url = \"http://{btc}\"\ncookie = \"{}\"\ncreate_wallet = false\n",
        cookie_in(&dir)
    );
    let spi = load(&dir, Some(&text)).unwrap();
    let out = commands::install(&spi, &options(&dir));
    assert!(
        !out.ok
            && out
                .lines
                .iter()
                .any(|l| l.contains("create_wallet is false"))
    );
    assert!(disk.lock().unwrap().is_empty());
}

#[test]
fn install_fails_when_bitcoind_is_down_or_on_the_wrong_chain_but_still_writes_the_files() {
    let dir = scratch("install-down");
    let spi = config(&dir, "127.0.0.1:1", "127.0.0.1:3000", "127.0.0.1:3100");
    let out = commands::install(&spi, &options(&dir));
    assert!(!out.ok);
    assert!(
        out.lines
            .iter()
            .any(|l| l.starts_with("FAIL: bitcoind is unreachable")),
        "{:?}",
        out.lines
    );
    assert!(dir.join("generated/psp-mint.toml").exists());

    let dir = scratch("install-chain");
    let (btc, _, _) = bitcoind("main", &[]);
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    let out = commands::install(&spi, &options(&dir));
    assert!(
        !out.ok
            && out
                .lines
                .iter()
                .any(|l| l.contains("bitcoind is on `main`"))
    );
}

#[test]
fn install_refuses_to_write_either_file_when_one_is_wrong() {
    let dir = scratch("install-conflict");
    let text = "[esp.settings.psp]\nurl = \"http://elsewhere\"\n";
    let spi = load(&dir, Some(text)).unwrap();
    let out = commands::install(&spi, &options(&dir));
    assert!(!out.ok);
    assert!(!dir.join("generated/psp-mint.toml").exists());
}

// ---- bitcoind sync ----

fn healthy_mints() -> (String, String) {
    (
        mint(
            200,
            json!({"status": "ok", "network": "regtest"}),
            json!({}),
        ),
        mint(
            200,
            json!({"status": "ok", "network": "regtest"}),
            json!({}),
        ),
    )
}

fn loaded_wallet(loaded: &Arc<Mutex<Vec<String>>>) {
    loaded.lock().unwrap().push("psp-regtest".into());
}

#[test]
fn syncing_makes_psp_not_ready_and_the_service_degraded_and_says_why() {
    let dir = scratch("syncing");
    let (btc, loaded, _) = bitcoind_seq(vec![syncing("regtest", 630, 1000, 0.634)], &[]);
    loaded_wallet(&loaded);
    let (psp, esp) = healthy_mints();
    let spi = config(&dir, &btc, &psp, &esp);
    let parts = commands::status(&spi);
    assert_eq!(parts[0].state, "syncing");
    assert!(!parts[0].ok);
    assert!(
        parts[0].detail.contains("bitcoind syncing 63%")
            && parts[0].detail.contains("blocks 630 of 1000 headers"),
        "{}",
        parts[0].detail
    );
    // The PSP mint answers /health, but is not ready, and the reason is the node.
    assert_eq!((parts[1].state.as_str(), parts[1].ok), ("not_ready", false));
    assert!(
        parts[1].detail.contains("NOT READY: bitcoind syncing 63%"),
        "{}",
        parts[1].detail
    );
    // The ESP mint is up.
    assert!(parts[2].ok);
    assert_eq!(commands::service_state(&parts), "degraded");
    assert!(!parts[0].detail.contains("unreachable"));
}

#[test]
fn synced_is_ok_all_through() {
    let dir = scratch("synced");
    let (btc, loaded, _) = bitcoind_seq(vec![synced("regtest")], &[]);
    loaded_wallet(&loaded);
    let (psp, esp) = healthy_mints();
    let spi = config(&dir, &btc, &psp, &esp);
    let parts = commands::status(&spi);
    assert!(parts.iter().all(|c| c.ok && c.state == "ok"), "{parts:?}");
    assert!(parts[0].detail.contains("synced, 1000 blocks"));
    assert_eq!(commands::service_state(&parts), "ok");
}

#[test]
fn unreachable_is_reported_apart_from_syncing() {
    let dir = scratch("unreachable");
    let (psp, esp) = healthy_mints();
    // Nothing listens on port 1.
    let spi = config(&dir, "127.0.0.1:1", &psp, &esp);
    let parts = commands::status(&spi);
    assert_eq!(parts[0].state, "unreachable");
    assert!(
        parts[0].detail.contains("bitcoind unreachable"),
        "{}",
        parts[0].detail
    );
    assert!(!parts[0].detail.contains("syncing"));
    assert_eq!(parts[1].state, "not_ready");
    assert!(parts[1].detail.contains("NOT READY: bitcoind unreachable"));
    assert!(!parts[1].detail.contains("syncing"));
    assert_eq!(commands::service_state(&parts), "down");

    // A cookie that cannot be read is also unreachable, not syncing.
    let (btc, _, _) = bitcoind("regtest", &[]);
    let mut spi = config(&dir, &btc, &psp, &esp);
    spi.cookie = dir.join("no-such-cookie");
    let parts = commands::status(&spi);
    assert_eq!(parts[0].state, "unreachable");
    assert!(parts[0].detail.contains("cannot read the bitcoind cookie"));
}

#[test]
fn a_node_that_is_behind_is_syncing_and_a_quiet_one_is_not() {
    let rpc_for = |info: Value| {
        let (addr, _, _) = bitcoind_seq(vec![info], &[]);
        let dir = scratch("rule");
        Rpc::new(&format!("http://{addr}"), &{
            let c = dir.join("cookie");
            std::fs::write(&c, "__cookie__:x").unwrap();
            c
        })
    };
    // The flag, and headers ahead of blocks.
    assert!(matches!(
        rpc_for(syncing("regtest", 5, 10, 0.5)).node("regtest"),
        Node::Syncing(_)
    ));
    // The flag, no headers ahead, but little verified.
    assert!(matches!(
        rpc_for(syncing("regtest", 10, 10, 0.5)).node("regtest"),
        Node::Syncing(_)
    ));
    // A fresh regtest node keeps the flag until its first block: caught up, so synced.
    assert!(matches!(
        rpc_for(syncing("regtest", 0, 0, 1.0)).node("regtest"),
        Node::Synced(_)
    ));
    // No flag, behind by a few: synced (the node says it is out of IBD).
    let mut info = synced("regtest");
    info["headers"] = json!(1003);
    assert!(matches!(rpc_for(info).node("regtest"), Node::Synced(_)));
    // Another chain.
    assert!(matches!(
        rpc_for(synced("main")).node("regtest"),
        Node::WrongChain { .. }
    ));
    // Missing fields read as a finished node.
    assert!(matches!(
        rpc_for(json!({"chain": "regtest", "blocks": 3})).node("regtest"),
        Node::Synced(_)
    ));
    // The numbers.
    let c = Chain {
        chain: "regtest".into(),
        blocks: 5,
        headers: 10,
        progress: 0.999999,
        ibd: true,
    };
    assert!(c.syncing() && c.percent() == 99);
}

#[test]
fn install_while_syncing_still_works_and_says_the_mints_will_wait() {
    let dir = scratch("install-syncing");
    let (btc, loaded, _) = bitcoind_seq(vec![syncing("regtest", 10, 100, 0.1)], &[]);
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    let out = commands::install(&spi, &options(&dir));
    assert!(out.ok, "{:?}", out.lines);
    assert!(
        out.lines
            .iter()
            .any(|l| l.contains("bitcoind syncing 10%") && l.contains("will not be started until"))
    );
    assert_eq!(*loaded.lock().unwrap(), ["psp-regtest"]);
}

#[test]
fn start_waits_for_the_sync_to_finish_and_reports_each_step_once() {
    let dir = scratch("wait");
    let (btc, _, _) = bitcoind_seq(
        vec![
            syncing("regtest", 10, 100, 0.10),
            syncing("regtest", 10, 100, 0.10),
            syncing("regtest", 50, 100, 0.50),
            synced("regtest"),
        ],
        &[],
    );
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    let mut said = Vec::new();
    let chain = run::wait_for_sync(&spi, 30, std::time::Duration::from_millis(10), &mut |l| {
        said.push(l.to_string())
    })
    .unwrap();
    assert!(!chain.syncing());
    assert_eq!(said, ["bitcoind syncing 10%", "bitcoind syncing 50%"]);
}

#[test]
fn start_gives_up_with_the_progress_and_starts_neither_mint() {
    let dir = scratch("wait-timeout");
    let (btc, _, _) = bitcoind_seq(vec![syncing("regtest", 630, 1000, 0.63)], &[]);
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    let err =
        run::wait_for_sync(&spi, 0, std::time::Duration::from_millis(10), &mut |_| {}).unwrap_err();
    assert!(err.contains("bitcoind syncing 63%"), "{err}");
    assert!(err.contains("were NOT started"), "{err}");

    // Unreachable gives a different message.
    let spi = config(&dir, "127.0.0.1:1", "127.0.0.1:3000", "127.0.0.1:3100");
    let err =
        run::wait_for_sync(&spi, 0, std::time::Duration::from_millis(10), &mut |_| {}).unwrap_err();
    assert!(
        err.contains("bitcoind unreachable") && !err.contains("syncing"),
        "{err}"
    );

    // The wrong chain stops at once, whatever the timeout.
    let (btc, _, _) = bitcoind_seq(vec![synced("main")], &[]);
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    let err = run::wait_for_sync(&spi, 30, std::time::Duration::from_millis(10), &mut |_| {})
        .unwrap_err();
    assert!(err.contains("bitcoind is on `main`"), "{err}");
}

#[test]
fn start_does_not_start_a_bitcoind_that_is_already_answering_or_one_it_may_not_manage() {
    let dir = scratch("start-bitcoind");
    let (btc, _, _) = bitcoind_seq(vec![syncing("regtest", 1, 10, 0.1)], &[]);
    let spi = config(&dir, &btc, "127.0.0.1:3000", "127.0.0.1:3100");
    // Answering (even while syncing): left alone, nothing is run.
    let line = run::start_bitcoind(&spi, 1).unwrap();
    assert!(line.contains("already running"), "{line}");
    assert!(!dir.join("run/bitcoind.started-by-spi").exists());
    // Not answering, and spi may not start it.
    let mut down = config(&dir, "127.0.0.1:1", "127.0.0.1:3000", "127.0.0.1:3100");
    down.manage_bitcoind = false;
    let err = run::start_bitcoind(&down, 1).unwrap_err();
    assert!(err.contains("bitcoind.manage is false"), "{err}");
    // A node on another chain is never started over.
    let (other, _, _) = bitcoind_seq(vec![synced("main")], &[]);
    let spi = config(&dir, &other, "127.0.0.1:3000", "127.0.0.1:3100");
    assert!(
        run::start_bitcoind(&spi, 1)
            .unwrap_err()
            .contains("is on `main`")
    );
}

#[test]
fn the_bitcoind_command_line_follows_the_network_and_the_config() {
    let dir = scratch("cmdline");
    let spi = load(&dir, None).unwrap();
    assert_eq!(run::bitcoind_command_line(&spi), ["-regtest", "-daemon"]);
    let spi = load(&dir, Some("network = \"main\"")).unwrap();
    assert_eq!(run::bitcoind_command_line(&spi), ["-daemon"]);
    let text = "network = \"signet\"\n[bitcoind]\ndatadir = \"/d\"\nargs = [\"-txindex\", \"-dbcache=2000\"]\nmanage = false\nsync_wait_secs = 5\nbin = \"/opt/bitcoind\"";
    let spi = load(&dir, Some(text)).unwrap();
    assert_eq!(
        run::bitcoind_command_line(&spi),
        [
            "-signet",
            "-datadir=/d",
            "-daemon",
            "-txindex",
            "-dbcache=2000"
        ]
    );
    assert!(!spi.manage_bitcoind);
    assert_eq!(
        (spi.sync_wait_secs, spi.bitcoind_bin.as_str()),
        (5, "/opt/bitcoind")
    );
    assert_eq!(load(&dir, None).unwrap().sync_wait_secs, 600);
    let bad = "[bitcoind]\nargs = [1]\nmanage = \"x\"\nsync_wait_secs = -1";
    let all = load(&dir, Some(bad)).unwrap_err().join("\n");
    for needle in [
        "args must be a list of strings",
        "manage must be true or false",
        "sync_wait_secs must be",
    ] {
        assert!(all.contains(needle), "{needle} not in {all}");
    }
}
