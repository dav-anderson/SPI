//! The AIR server and the SPI's own ecash-wallet in `spi.toml`: what the mints
//! and the wallet are told, what is refused, and what `spi config` shows.

mod common;

use common::{fake, load, scratch};
use spi::commands;
use spi::config::{Spi, Which};
use spi::generate;

fn air_name() -> String {
    format!("02{}", "ab".repeat(32))
}

fn psp_did() -> String {
    format!("03{}", "cd".repeat(32))
}

fn table(text: &str) -> toml::Table {
    text.parse().unwrap()
}

fn shown(spi: &Spi, key: &str) -> (String, String) {
    spi.shown
        .iter()
        .find(|(k, _, _)| k == key)
        .map(|(_, v, from)| (v.clone(), from.clone()))
        .unwrap_or_else(|| panic!("{key} is not shown: {:?}", spi.shown))
}

#[test]
fn a_fresh_config_with_the_air_server_set_gives_both_mints_working_air_settings() {
    let name = air_name();
    let text = format!("[air]\nname = \"{name}\"\nhost = \"air.example.org\"\n");
    let spi = load(&scratch("air-mints"), Some(&text)).unwrap();
    for which in [Which::Psp, Which::Esp] {
        let t = table(&generate::render(&spi, which).unwrap());
        assert_eq!(
            t["air"]["server"].as_str(),
            Some(name.as_str()),
            "{which:?}"
        );
        assert_eq!(
            t["air"]["host"].as_str(),
            Some("air.example.org"),
            "{which:?}"
        );
    }
    // Without a name there is nothing to write: no air keys at all.
    let spi = load(&scratch("air-none"), None).unwrap();
    for which in [Which::Psp, Which::Esp] {
        let t = table(&generate::render(&spi, which).unwrap());
        assert!(!t.contains_key("air"), "{which:?}");
    }
    // Settings that the AIR section sets cannot also be passed through.
    for (file, needle) in [
        (
            "[psp.settings.air]\nserver = \"x\"",
            "psp.settings.air.server",
        ),
        ("[psp.settings.air]\nhost = \"x\"", "psp.settings.air.host"),
        (
            "[esp.settings.air]\nserver = \"x\"",
            "esp.settings.air.server",
        ),
        ("[esp.settings.air]\nhost = \"x\"", "esp.settings.air.host"),
    ] {
        let spi = load(&scratch("air-pass"), Some(file)).unwrap();
        let which = if file.starts_with("[psp") {
            Which::Psp
        } else {
            Which::Esp
        };
        let err = generate::render(&spi, which).unwrap_err();
        assert!(err.contains(needle), "{file}: {err}");
    }
}

#[test]
fn the_air_server_gets_its_environment_from_spi_toml() {
    let dir = scratch("air-env");
    let text = "[air]\nws_addr = \"127.0.0.1:5702\"\nhealth_addr = \"127.0.0.1:5801\"\nidentity = \"/keys/air.json\"\nstorage = \"/data/air/STORAGE.db\"\ntls_addr = \"0.0.0.0:8443\"\ntls_cert = \"/tls/c.pem\"\ntls_key = \"/tls/k.pem\"\n";
    let spi = load(&dir, Some(text)).unwrap();
    let env = generate::air_env(&spi, false);
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(
        pairs,
        [
            ("AIR_SECRET_FILE", "/keys/air.json"),
            ("AIR_STORAGE", "/data/air/STORAGE.db"),
            ("AIR_WS_ADDR", "127.0.0.1:5702"),
            ("AIR_HEALTH_ADDR", "127.0.0.1:5801"),
            ("AIR_TLS_ADDR", "0.0.0.0:8443"),
            ("AIR_TLS_CERT", "/tls/c.pem"),
            ("AIR_TLS_KEY", "/tls/k.pem"),
        ]
    );
    // Without TLS settings the server gets none (it would listen on :443 as soon
    // as it had a certificate and a key).
    let spi = load(&scratch("air-env-plain"), None).unwrap();
    let env = generate::air_env(&spi, false);
    assert!(
        env.iter().all(|(k, _)| !k.starts_with("AIR_TLS")),
        "{env:?}"
    );
    assert_eq!(
        env.iter()
            .find(|(k, _)| k == "AIR_WS_ADDR")
            .map(|(_, v)| v.as_str()),
        Some("127.0.0.1:5702")
    );
}

#[test]
fn air_settings_that_would_not_work_are_refused_naming_the_setting() {
    for (file, needle) in [
        ("[air]\nname = \"abc\"", "air.name is `abc`"),
        (
            "[air]\nhost = \"https://air.example\"",
            "air.host is `https://air.example`",
        ),
        (
            "[air]\ntls_cert = \"/c.pem\"",
            "air.tls_cert and air.tls_key go together",
        ),
        (
            "[air]\ntls_addr = \"0.0.0.0:443\"",
            "air.tls_addr is set without air.tls_cert and air.tls_key",
        ),
        ("[air]\nversion = \"1.0\"", "air.version is `1.0`"),
        (
            "[air]\nidentity = \"relative.json\"",
            "air.identity is `relative.json`",
        ),
    ] {
        let problems = load(&scratch("air-bad"), Some(file))
            .unwrap_err()
            .join("\n");
        assert!(problems.contains(needle), "{file}: {problems}");
    }
}

#[test]
fn a_non_zero_air_server_fee_is_refused_naming_the_missing_upstream_hook() {
    for (key, value) in [("fee_read", "1"), ("fee_write", "250")] {
        let file = format!("[air]\n{key} = {value}\n");
        let problems = load(&scratch("air-fee"), Some(&file)).unwrap_err();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].starts_with(&format!(
                "air.{key} = {value}: stock air_server 1.0.1 has no fee hook"
            )) && problems[0].contains("upstream item 1, the air_server fee hook"),
            "{problems:?}"
        );
    }
    let problems = load(
        &scratch("air-fee-text"),
        Some("[air]\nfee_read = \"free\"\n"),
    )
    .unwrap_err();
    assert!(
        problems[0].contains("must be a whole number, and 0 until upstream item 1"),
        "{problems:?}"
    );
    // 0 is the default and is accepted, and shown.
    let spi = load(
        &scratch("air-fee-zero"),
        Some("[air]\nfee_read = 0\nfee_write = 0\n"),
    )
    .unwrap();
    assert_eq!(shown(&spi, "air.fee_read"), ("0".into(), "spi.toml".into()));
    assert_eq!(
        shown(&spi, "air.fee_write"),
        ("0".into(), "spi.toml".into())
    );
    let spi = load(&scratch("air-fee-default"), None).unwrap();
    assert_eq!(shown(&spi, "air.fee_read"), ("0".into(), "default".into()));
}

#[test]
fn the_wallet_config_is_written_from_spi_toml_and_config_shows_each_setting_and_its_source() {
    let dir = scratch("wallet-cfg");
    let (name, did) = (air_name(), psp_did());
    let text = format!(
        "[air]\nname = \"{name}\"\n[psp]\ndid = \"{did}\"\n[wallet.settings.air]\nattempts = 3\n"
    );
    let spi = load(&dir, Some(&text)).unwrap();
    // No program may be run: both DIDs are written down or have no key file yet.
    let never = fake(|bin, _| Err(format!("{bin} must not be run")));
    let written = commands::write_configs_with(&spi, &never).unwrap();
    let path = dir.join("generated/ecash-wallet.toml");
    assert!(written.contains(&path), "{written:?}");
    assert!(never.calls.lock().unwrap().is_empty());
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(file.starts_with("# Generated by spi. Don't edit"), "{file}");
    let t = table(&file);
    assert_eq!(t["air"]["server"].as_str(), Some(name.as_str()));
    assert_eq!(t["air"]["host"].as_str(), Some("127.0.0.1"));
    assert_eq!(t["air"]["psp_did"].as_str(), Some(did.as_str()));
    assert!(t["air"].as_table().unwrap().get("esp_did").is_none());
    assert_eq!(t["air"]["attempts"].as_integer(), Some(3));

    let said = commands::config_show(&spi, None).join("\n");
    assert!(
        said.contains(&format!(
            "air.server = \"{name}\"   (from [air], [psp] did, [esp] did)"
        )),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "air.psp_did = \"{did}\"   (from [air], [psp] did, [esp] did)"
        )),
        "{said}"
    );
    assert!(
        said.contains("air.attempts = 3   ([wallet.settings] in spi.toml)"),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "ecash-wallet: ecash-wallet --wallet {} --config {}",
            dir.join("wallet/wallet.json").display(),
            path.display()
        )),
        "{said}"
    );
    assert_eq!(
        shown(&spi, "wallet.file"),
        (
            format!("{}/wallet/wallet.json", dir.display()),
            "default".into()
        )
    );
}

#[test]
fn a_wallet_setting_that_spi_names_cannot_also_be_passed_through() {
    let file = "[wallet.settings.air]\npsp_did = \"x\"\n";
    let spi = load(&scratch("wallet-conflict"), Some(file)).unwrap();
    let err = generate::render_wallet(&spi, None, None).unwrap_err();
    assert!(
        err.contains("wallet.settings.air.psp_did is set by spi"),
        "{err}"
    );
    // And so no config file is written by an install.
    let never = fake(|bin, _| Err(format!("{bin} must not be run")));
    let dir = scratch("wallet-conflict-dir");
    let spi = load(&dir, Some(file)).unwrap();
    assert!(commands::write_configs_with(&spi, &never).is_err());
    assert!(!dir.join("generated/psp-mint.toml").exists());
}

#[test]
fn a_mints_did_is_asked_of_its_binary_only_where_its_key_file_already_exists() {
    let dir = scratch("did");
    let spi = load(&dir, None).unwrap();
    // No key file yet: asking `pubkey` would make one, so nothing is run.
    let never = fake(|bin, _| Err(format!("{bin} must not be run")));
    assert_eq!(commands::did_of(&spi, &never, Which::Psp).unwrap(), None);
    assert_eq!(commands::did_of(&spi, &never, Which::Esp).unwrap(), None);
    assert!(never.calls.lock().unwrap().is_empty());

    // With the key file there, the binary is asked in that folder.
    let key_dir = dir.join("psp-mint/regtest");
    std::fs::create_dir_all(&key_dir).unwrap();
    std::fs::write(key_dir.join("mint_did_key.json"), "{}").unwrap();
    let did = psp_did();
    let said = did.clone();
    let asked = fake(move |_, _| common::ran(0, &format!("{said}\n"), ""));
    assert_eq!(
        commands::did_of(&spi, &asked, Which::Psp).unwrap(),
        Some(did)
    );
    assert_eq!(
        *asked.calls.lock().unwrap(),
        [format!(
            "psp-mint pubkey env PSP_DATA_DIR={} PSP_NETWORK=regtest",
            dir.join("psp-mint").display()
        )]
    );
    // Something that is not a DID is an error, not a wrong config.
    let bad = fake(|_, _| common::ran(0, "hello\n", ""));
    let err = commands::did_of(&spi, &bad, Which::Psp).unwrap_err();
    assert!(err.contains("printed `hello`, which is not a DID"), "{err}");
    let failed = fake(|_, _| common::ran(1, "", "error: boom\n"));
    let err = commands::did_of(&spi, &failed, Which::Psp).unwrap_err();
    assert_eq!(err, "`psp-mint pubkey` failed: error: boom");
    // A DID written in spi.toml wins, and nothing is run.
    let text = format!("[psp]\ndid = \"{}\"\n", psp_did());
    let spi = load(&scratch("did-set"), Some(&text)).unwrap();
    assert_eq!(
        commands::did_of(&spi, &never, Which::Psp).unwrap(),
        Some(psp_did())
    );
    assert!(never.calls.lock().unwrap().is_empty());
}

/// psp-mint 0.8 has no reconciliation window, and a `settle.reconciliation_window_days`
/// in its file stops it at start. Fails if spi reads the key again, writes it to
/// `psp-mint.toml`, or refuses it with "not a setting" instead of saying it was
/// removed.
#[test]
fn the_reconciliation_window_key_is_refused_as_removed_and_never_written_to_psp_mint() {
    let problems = load(
        &scratch("window-removed"),
        Some("[psp]\nreconciliation_window_days = 30\n"),
    )
    .unwrap_err();
    assert_eq!(
        problems,
        vec![
            "[psp] reconciliation_window_days was removed in spi 0.3.0: psp-mint no longer has a reconciliation window: a payer claims what a payment is owed within 30 days of the owed record being written, and that window is not a setting. Remove it"
                .to_string()
        ]
    );
    // Without it, the generated psp-mint.toml names nothing under settle, and spi
    // shows no such key.
    let spi = load(&scratch("window-default"), None).unwrap();
    let t = table(&generate::render(&spi, Which::Psp).unwrap());
    assert!(t.get("settle").is_none(), "{t:?}");
    assert!(
        !spi.shown
            .iter()
            .any(|(k, _, _)| k.contains("reconciliation")),
        "{:?}",
        spi.shown
    );
}

/// `settle.retention_days` and `settle.prune_horizon_days` are psp-mint's own and
/// pass through as written; `config show` names them, with psp-mint's defaults
/// when they are not set. Fails if spi drops, renames or validates them.
#[test]
fn the_settle_retention_settings_reach_psp_mint_as_written_and_config_show_names_them() {
    let spi = load(
        &scratch("settle-pass"),
        Some("[psp.settings.settle]\nretention_days = 120\nprune_horizon_days = 400\n"),
    )
    .unwrap();
    let t = table(&generate::render(&spi, Which::Psp).unwrap());
    assert_eq!(t["settle"]["retention_days"].as_integer(), Some(120));
    assert_eq!(t["settle"]["prune_horizon_days"].as_integer(), Some(400));
    assert_eq!(
        shown(&spi, "psp.settings.settle.retention_days"),
        ("120".into(), "spi.toml".into())
    );
    assert_eq!(
        shown(&spi, "psp.settings.settle.prune_horizon_days"),
        ("400".into(), "spi.toml".into())
    );
    let said = commands::config_show(&spi, None).join("\n");
    assert!(
        said.contains("psp.settings.settle.retention_days = 120   (spi.toml)"),
        "{said}"
    );
    // spi does not judge the values: 0 (keep everything) and a horizon below the
    // floor psp-mint would refuse both pass.
    let spi = load(
        &scratch("settle-zero"),
        Some("[psp.settings.settle]\nretention_days = 0\nprune_horizon_days = 1\n"),
    )
    .unwrap();
    let t = table(&generate::render(&spi, Which::Psp).unwrap());
    assert_eq!(t["settle"]["retention_days"].as_integer(), Some(0));
    assert_eq!(t["settle"]["prune_horizon_days"].as_integer(), Some(1));
    // Not set: psp-mint's own defaults stand and the file does not name them.
    let spi = load(&scratch("settle-default"), None).unwrap();
    assert_eq!(
        shown(&spi, "psp.settings.settle.retention_days"),
        ("90 (psp-mint's own)".into(), "default".into())
    );
    assert_eq!(
        shown(&spi, "psp.settings.settle.prune_horizon_days"),
        ("210 (psp-mint's own)".into(), "default".into())
    );
}

/// `stream.days_ahead`, `stream.record_max_bytes` and `stream.max_services` are
/// esp-mint's own (esp-mint 0.11.0, the keys it publishes on AIR) and pass
/// through as written; `config show` names them, with esp-mint's defaults when
/// they are not set. Fails if spi drops, renames or validates them, writes them
/// into psp-mint's file, or does not name them.
#[test]
fn the_esp_stream_settings_reach_esp_mint_as_written_and_config_show_names_them() {
    let spi = load(
        &scratch("stream-pass"),
        Some(
            "[esp.settings.stream]\ndays_ahead = 5\nrecord_max_bytes = 2097152\nmax_services = 250\n",
        ),
    )
    .unwrap();
    let t = table(&generate::render(&spi, Which::Esp).unwrap());
    assert_eq!(t["stream"]["days_ahead"].as_integer(), Some(5));
    assert_eq!(
        t["stream"]["record_max_bytes"].as_integer(),
        Some(2_097_152)
    );
    assert_eq!(t["stream"]["max_services"].as_integer(), Some(250));
    let psp = table(&generate::render(&spi, Which::Psp).unwrap());
    assert!(psp.get("stream").is_none(), "{psp:?}");
    for (key, value) in [
        ("days_ahead", "5"),
        ("record_max_bytes", "2097152"),
        ("max_services", "250"),
    ] {
        assert_eq!(
            shown(&spi, &format!("esp.settings.stream.{key}")),
            (value.into(), "spi.toml".into())
        );
    }
    let said = commands::config_show(&spi, None).join("\n");
    assert!(
        said.contains("esp.settings.stream.days_ahead = 5   (spi.toml)"),
        "{said}"
    );
    // spi does not judge the values: esp-mint refuses 0 days and a record under
    // 1 MiB, and spi writes them as they are.
    let spi = load(
        &scratch("stream-odd"),
        Some("[esp.settings.stream]\ndays_ahead = 0\nrecord_max_bytes = 10\n"),
    )
    .unwrap();
    let t = table(&generate::render(&spi, Which::Esp).unwrap());
    assert_eq!(t["stream"]["days_ahead"].as_integer(), Some(0));
    assert_eq!(t["stream"]["record_max_bytes"].as_integer(), Some(10));
    // Not set: esp-mint's own defaults stand and the file does not name them.
    let spi = load(&scratch("stream-default"), None).unwrap();
    let t = table(&generate::render(&spi, Which::Esp).unwrap());
    assert!(t.get("stream").is_none(), "{t:?}");
    for (key, value) in [
        ("days_ahead", "2 (esp-mint's own)"),
        ("record_max_bytes", "4194304 (esp-mint's own)"),
        ("max_services", "1000 (esp-mint's own)"),
    ] {
        assert_eq!(
            shown(&spi, &format!("esp.settings.stream.{key}")),
            (value.into(), "default".into())
        );
    }
}

/// psp-mint needs a rotation period of 0 or at least 720 hours (the claim
/// window). Fails if 1 to 719 is accepted, 720 or 0 is refused, or esp-mint's
/// period is looked at.
#[test]
fn a_psp_rotation_period_of_1_to_719_hours_is_refused() {
    for hours in [1, 24, 719] {
        let file = format!("[psp.settings.rotation]\nperiod_hours = {hours}\n");
        let problems = load(&scratch("period-bad"), Some(&file)).unwrap_err();
        assert_eq!(
            problems,
            vec![format!(
                "psp.settings.rotation.period_hours is {hours}: psp-mint needs 0 (never rotate) or at least 720 hours, so that the keyset a payer claims with outlives the 30-day claim window"
            )]
        );
    }
    for hours in [0, 720, 1440] {
        let file = format!("[psp.settings.rotation]\nperiod_hours = {hours}\n");
        let spi = load(&scratch("period-ok"), Some(&file)).unwrap();
        let t = table(&generate::render(&spi, Which::Psp).unwrap());
        assert_eq!(t["rotation"]["period_hours"].as_integer(), Some(hours));
    }
    // esp-mint's own period is not spi's to judge.
    let spi = load(
        &scratch("period-esp"),
        Some("[esp.settings.rotation]\nperiod_hours = 24\n"),
    )
    .unwrap();
    let t = table(&generate::render(&spi, Which::Esp).unwrap());
    assert_eq!(t["rotation"]["period_hours"].as_integer(), Some(24));
}

#[test]
fn burning_is_off_by_default_and_the_threshold_reaches_psp_mint() {
    // Default: the generated file says that nothing paid is burned.
    let spi = load(&scratch("burn-default"), None).unwrap();
    let t = table(&generate::render(&spi, Which::Psp).unwrap());
    assert_eq!(t["burn"]["enabled"].as_bool(), Some(false));
    assert_eq!(t["burn"]["threshold_psp"].as_integer(), Some(0));
    assert_eq!(shown(&spi, "psp.burn"), ("false".into(), "default".into()));

    let spi = load(
        &scratch("burn-on"),
        Some("[psp]\nburn = true\nburn_threshold_psp = 1000\n"),
    )
    .unwrap();
    let t = table(&generate::render(&spi, Which::Psp).unwrap());
    assert_eq!(t["burn"]["enabled"].as_bool(), Some(true));
    assert_eq!(t["burn"]["threshold_psp"].as_integer(), Some(1000));
    assert_eq!(shown(&spi, "psp.burn"), ("true".into(), "spi.toml".into()));
    assert_eq!(
        shown(&spi, "psp.burn_threshold_psp"),
        ("1000".into(), "spi.toml".into())
    );

    // Not a choice: refused.
    let problems = load(&scratch("burn-text"), Some("[psp]\nburn = \"yes\"\n")).unwrap_err();
    assert!(
        problems[0].contains("[psp] burn must be true or false"),
        "{problems:?}"
    );
    let problems = load(
        &scratch("burn-big"),
        Some("[psp]\nburn_threshold_psp = 4611686018427387905\n"),
    )
    .unwrap_err();
    assert!(
        problems[0].contains(
            "[psp] burn_threshold_psp must be a whole number from 0 to 4611686018427387904"
        ),
        "{problems:?}"
    );
    // The same key cannot be set twice.
    let spi = load(
        &scratch("burn-pass"),
        Some("[psp.settings.burn]\nenabled = true\n"),
    )
    .unwrap();
    let err = generate::render(&spi, Which::Psp).unwrap_err();
    assert!(err.contains("psp.settings.burn.enabled"), "{err}");
}
