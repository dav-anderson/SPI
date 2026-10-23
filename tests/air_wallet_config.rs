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
        "[air]\nname = \"{name}\"\n[psp]\ndid = \"{did}\"\n[wallet.settings.psp]\nmin_purchase = 100\n"
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
    assert_eq!(t["psp"]["min_purchase"].as_integer(), Some(100));

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
        said.contains("psp.min_purchase = 100   ([wallet.settings] in spi.toml)"),
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

#[test]
fn the_reconciliation_window_is_a_named_key_shown_by_name_and_written_to_psp_mint() {
    let spi = load(
        &scratch("window"),
        Some("[psp]\nreconciliation_window_days = 30\n"),
    )
    .unwrap();
    assert_eq!(
        shown(&spi, "psp.reconciliation_window_days"),
        ("30".into(), "spi.toml".into())
    );
    let t = table(&generate::render(&spi, Which::Psp).unwrap());
    assert_eq!(
        t["settle"]["reconciliation_window_days"].as_integer(),
        Some(30)
    );
    let said = commands::config_show(&spi, None).join("\n");
    assert!(
        said.contains("psp.reconciliation_window_days = 30   (spi.toml)"),
        "{said}"
    );

    // Not set: psp-mint's own default stands, and the file does not name it.
    let spi = load(&scratch("window-default"), None).unwrap();
    assert_eq!(
        shown(&spi, "psp.reconciliation_window_days"),
        ("14 (psp-mint's own)".into(), "default".into())
    );
    let t = table(&generate::render(&spi, Which::Psp).unwrap());
    assert!(t.get("settle").is_none(), "{t:?}");

    // Out of range, and also passed through: refused.
    for bad in ["0", "366", "\"14\""] {
        let file = format!("[psp]\nreconciliation_window_days = {bad}\n");
        let problems = load(&scratch("window-bad"), Some(&file)).unwrap_err();
        assert!(
            problems[0]
                .contains("[psp] reconciliation_window_days must be a whole number from 1 to 365"),
            "{bad}: {problems:?}"
        );
    }
    let spi = load(
        &scratch("window-pass"),
        Some("[psp.settings.settle]\nreconciliation_window_days = 5\n"),
    )
    .unwrap();
    let err = generate::render(&spi, Which::Psp).unwrap_err();
    assert!(
        err.contains("psp.settings.settle.reconciliation_window_days"),
        "{err}"
    );
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
