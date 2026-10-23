//! `spi install` as a bootstrap: with nothing but the spi binary, it writes the
//! settings, env files and service definitions for Linux (systemd) and macOS
//! (launchd), and the script that puts them in place. No `sandbox-mint` folder
//! is anywhere in these tests: the definitions are inside the binary.
//!
//! Nothing is installed by these tests. The script is only syntax-checked with
//! `sh -n`; running it needs root and a real machine (README, "Tests").

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::scratch;
use spi::commands::{self, InstallOptions, ServiceFiles};
use spi::config::{Inputs, Spi};
use spi::service;

/// A `Spi` for Linux or macOS from a config text, with an environment.
fn load_os(dir: &Path, text: &str, mac: bool, env: &[(&str, &str)]) -> Result<Spi, Vec<String>> {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let lookup = move |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    Spi::load(&Inputs {
        dir: dir.to_path_buf(),
        file: Some(text),
        env: &lookup,
        home: "/home/test".into(),
        os: if mac { "macos" } else { "linux" },
    })
}

/// Stand-in binaries, so that the script has something to install.
fn binaries(dir: &Path) -> PathBuf {
    let bins = dir.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    for n in ["psp-mint", "esp-mint", "bitcoind"] {
        std::fs::write(bins.join(n), "#!/bin/sh\n").unwrap();
    }
    std::fs::canonicalize(bins).unwrap()
}

/// A config text with the binaries above, `bitcoind` lines, and `[service]`
/// lines. The node is nowhere (port 1) unless `bitcoind` says otherwise.
fn config_text(dir: &Path, network: &str, bitcoind: &str, service: &str) -> String {
    let b = binaries(dir);
    let b = b.display();
    format!(
        "network = \"{network}\"\n\
         [bitcoind]\nbin = \"{b}/bitcoind\"\n{bitcoind}\n\
         [psp]\nbin = \"{b}/psp-mint\"\n\
         [esp]\nbin = \"{b}/esp-mint\"\n\
         [service]\n{service}\n"
    )
}

/// A node that is not there.
const NOWHERE: &str = "rpc_url = \"http://127.0.0.1:1\"";

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn install_into(spi: &Spi, out: &Path) -> commands::Outcome {
    commands::install(
        spi,
        &InstallOptions {
            service_files: ServiceFiles::Into(out.to_path_buf()),
            ops_dir: None,
        },
    )
}

fn shell_syntax_ok(file: &Path) {
    let status = Command::new("sh").arg("-n").arg(file).status().unwrap();
    assert!(status.success(), "sh -n {}", file.display());
}

fn tags_balance(text: &str, tag: &str) {
    assert_eq!(
        text.matches(&format!("<{tag}>")).count(),
        text.matches(&format!("</{tag}>")).count(),
        "<{tag}> in {text}"
    );
}

#[test]
fn linux_install_with_no_sandbox_mint_writes_complete_systemd_units() {
    let dir = scratch("linux");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let out_dir = dir.join("svc");
    let out = install_into(&spi, &out_dir);
    let said = out.lines.join("\n");
    assert!(out.ok, "{said}");
    assert!(!said.contains("sandbox-mint"), "{said}");
    // bitcoind is not there yet and that is not a failure: the files start it.
    assert!(said.contains("bitcoind is not running"), "{said}");
    assert!(said.contains("Linux (systemd)"), "{said}");
    assert!(said.contains("sudo sh "), "{said}");
    assert!(said.contains("install-system.sh"), "{said}");
    assert!(
        said.contains("  sudo systemctl enable --now sp-bitcoind.service\n  spi install --no-service-files   (again"),
        "{said}"
    );
    assert!(
        said.contains(
            "  sudo systemctl enable --now psp-mint.service esp-mint.service psp-mint-health.service esp-mint-health.service"
        ),
        "{said}"
    );
    assert!(said.contains("esp-mint.secret.env"), "{said}");

    for f in [
        "psp-mint.toml",
        "psp-mint.env",
        "psp-mint.secret.env",
        "psp-mint.service",
        "psp-mint-health.conf",
        "psp-mint-health.service",
        "esp-mint.toml",
        "esp-mint.env",
        "esp-mint.secret.env",
        "esp-mint.service",
        "esp-mint-health.conf",
        "esp-mint-health.service",
        "sp-bitcoind.service",
        "sp-health-monitor.sh",
        "logrotate-sp-mint",
        "install-system.sh",
    ] {
        let text = read(&out_dir, f);
        assert!(!text.is_empty(), "{f}");
        assert!(!text.contains("{{") && !text.contains("}}"), "{f}: {text}");
        assert!(!text.contains("sandbox-mint"), "{f}");
    }
    // No property lists on Linux.
    assert!(!out_dir.join("org.servicepoints.psp-mint.plist").exists());

    let psp = read(&out_dir, "psp-mint.service");
    for line in [
        "User=spmint",
        "Group=spmint",
        "After=network-online.target sp-bitcoind.service",
        "Wants=network-online.target sp-bitcoind.service",
        "EnvironmentFile=/etc/sp-mint/psp-mint.env",
        "EnvironmentFile=/etc/sp-mint/psp-mint.secret.env",
        "ExecStart=/usr/local/bin/psp-mint serve",
        "ReadWritePaths=/var/lib/sp-mint/psp-mint /var/log/sp-mint",
        "StandardOutput=append:/var/log/sp-mint/psp-mint.log",
        "ProtectHome=yes",
        "ProtectSystem=strict",
        "KillSignal=SIGTERM",
        "TimeoutStopSec=30",
    ] {
        assert!(psp.lines().any(|l| l == line), "{line} not in\n{psp}");
    }
    let esp = read(&out_dir, "esp-mint.service");
    assert!(esp.contains("After=network-online.target psp-mint.service"));
    assert!(esp.contains("ExecStart=/usr/local/bin/esp-mint serve"));
    assert!(esp.contains("EnvironmentFile=/etc/sp-mint/esp-mint.env"));

    let btc = read(&out_dir, "sp-bitcoind.service");
    assert!(
        btc.contains("ExecStart=/usr/local/bin/bitcoind -regtest -datadir=/var/lib/sp-mint/bitcoind -rpccookieperms=group -rpcport=1"),
        "{btc}"
    );
    assert!(
        !btc.lines()
            .any(|l| l.starts_with("ExecStart=") && l.contains("-daemon"))
    );
    assert!(btc.contains("ReadWritePaths=/var/lib/sp-mint/bitcoind /var/log/sp-mint"));
    assert!(btc.contains("User=spmint"));

    let health = read(&out_dir, "esp-mint-health.service");
    assert!(health.contains("Environment=HEALTH_URL=http://127.0.0.1:3100/health"));
    assert!(
        health.contains("Environment=\"HEALTH_RESTART_CMD=systemctl restart esp-mint.service\"")
    );
    assert!(health.contains(
        "ExecStart=/usr/local/bin/sp-health-monitor.sh --config /etc/sp-mint/esp-mint-health.conf"
    ));

    // The settings and env files the units read.
    let env = read(&out_dir, "esp-mint.env");
    for line in [
        "HOME=/var/lib/sp-mint/esp-mint",
        "ESP_DATA_DIR=/var/lib/sp-mint/esp-mint",
        "ESP_CONFIG=/etc/sp-mint/esp-mint.toml",
        "ESP_NETWORK=regtest",
    ] {
        assert!(env.lines().any(|l| l == line), "{line} not in\n{env}");
    }
    let toml = read(&out_dir, "psp-mint.toml");
    assert!(toml.starts_with("# Generated by spi"));
    // The service's own bitcoind writes the cookie under its data folder.
    assert!(
        toml.contains("cookie = \"/var/lib/sp-mint/bitcoind/regtest/.cookie\""),
        "{toml}"
    );
    assert!(read(&out_dir, "logrotate-sp-mint").contains("/var/log/sp-mint/*.log"));

    // The script: the account, the folders, the binaries it was given, the files.
    let script = read(&out_dir, "install-system.sh");
    let b = std::fs::canonicalize(dir.join("bins")).unwrap();
    for needle in [
        "useradd --system --gid \"$group\"",
        "install -d -m 0750 -o \"$user\" -g \"$group\" \"$data_dir/psp-mint\" \"$data_dir/esp-mint\" \"$data_dir/bitcoind\"",
        &format!(
            "put 0755 root root {}/psp-mint \"$bin_dir/psp-mint\"",
            b.display()
        ),
        &format!(
            "put 0755 root root {}/bitcoind \"$bin_dir/bitcoind\"",
            b.display()
        ),
        "put 0640 root \"$group\" \"$here/psp-mint.toml\" \"$etc_dir/psp-mint.toml\"",
        "put 0644 root root \"$here/esp-mint.service\" \"$unit_dir/esp-mint.service\"",
        "unit_dir=/etc/systemd/system",
        "systemctl daemon-reload",
        "/etc/logrotate.d/sp-mint",
    ] {
        assert!(script.contains(needle), "{needle} not in\n{script}");
    }
    assert!(!script.contains("dscl"));
    shell_syntax_ok(&out_dir.join("install-system.sh"));
    shell_syntax_ok(&out_dir.join("sp-health-monitor.sh"));
}

#[test]
fn macos_install_with_no_sandbox_mint_writes_complete_plists() {
    let dir = scratch("mac");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), true, &[]).unwrap();
    let out_dir = dir.join("svc");
    let out = install_into(&spi, &out_dir);
    let said = out.lines.join("\n");
    assert!(out.ok, "{said}");
    assert!(!said.contains("sandbox-mint"), "{said}");
    assert!(said.contains("macOS (launchd)"), "{said}");
    for plist in [
        "org.servicepoints.bitcoind.plist",
        "org.servicepoints.psp-mint.plist",
        "org.servicepoints.esp-mint.plist",
        "org.servicepoints.psp-mint-health.plist",
        "org.servicepoints.esp-mint-health.plist",
    ] {
        assert!(
            said.contains(&format!(
                "sudo launchctl bootstrap system /Library/LaunchDaemons/{plist}"
            )),
            "{plist} not in\n{said}"
        );
        let text = read(&out_dir, plist);
        assert!(text.starts_with("<?xml"), "{plist}");
        assert!(!text.contains("{{") && !text.contains("}}"), "{plist}");
        for tag in ["dict", "array", "string", "key"] {
            tags_balance(&text, tag);
        }
        assert!(text.trim_end().ends_with("</plist>"));
    }
    // bitcoind first, then the wallet, then the mints.
    let at = |needle: &str| said.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("org.servicepoints.bitcoind.plist") < at("spi install --no-service-files   (again"));
    assert!(
        at("spi install --no-service-files   (again")
            < at("LaunchDaemons/org.servicepoints.psp-mint.plist")
    );
    // No systemd units on macOS.
    assert!(!out_dir.join("psp-mint.service").exists());
    assert!(!out_dir.join("logrotate-sp-mint").exists());

    let psp = read(&out_dir, "org.servicepoints.psp-mint.plist");
    for needle in [
        "<string>org.servicepoints.psp-mint</string>",
        "<key>UserName</key>\n    <string>_spmint</string>",
        "<key>GroupName</key>\n    <string>_spmint</string>",
        ". '/etc/sp-mint/psp-mint.env'; . '/etc/sp-mint/psp-mint.secret.env'; set +a; exec '/usr/local/bin/psp-mint' serve",
        "<key>WorkingDirectory</key>\n    <string>/var/lib/sp-mint/psp-mint</string>",
        "<string>/var/log/sp-mint/psp-mint.log</string>",
        "<key>ExitTimeOut</key>\n    <integer>30</integer>",
    ] {
        assert!(psp.contains(needle), "{needle} not in\n{psp}");
    }
    let btc = read(&out_dir, "org.servicepoints.bitcoind.plist");
    for needle in [
        "<string>/usr/local/bin/bitcoind</string>",
        "<string>-regtest</string>",
        "<string>-datadir=/var/lib/sp-mint/bitcoind</string>",
        "<string>-rpccookieperms=group</string>",
        "<string>-rpcport=1</string>",
        "<string>_spmint</string>",
    ] {
        assert!(btc.contains(needle), "{needle} not in\n{btc}");
    }
    assert!(!btc.contains("<string>-daemon</string>"));
    let health = read(&out_dir, "org.servicepoints.esp-mint-health.plist");
    assert!(health.contains("<string>http://127.0.0.1:3100/health</string>"));
    assert!(
        health
            .contains("<string>launchctl kickstart -k system/org.servicepoints.esp-mint</string>")
    );
    assert!(health.contains("<string>/etc/sp-mint/esp-mint-health.conf</string>"));

    let env = read(&out_dir, "psp-mint.env");
    assert!(env.contains("PSP_DATA_DIR=/var/lib/sp-mint/psp-mint"));
    let toml = read(&out_dir, "psp-mint.toml");
    assert!(toml.contains("cookie = \"/var/lib/sp-mint/bitcoind/regtest/.cookie\""));

    let script = read(&out_dir, "install-system.sh");
    for needle in [
        "user=_spmint",
        "unit_dir=/Library/LaunchDaemons",
        "dscl . -create \"/Users/$user\" UniqueID",
        "put 0644 root wheel \"$here/org.servicepoints.psp-mint.plist\" \"$unit_dir/org.servicepoints.psp-mint.plist\"",
        "keep 0640 root \"$group\" \"$here/esp-mint.secret.env\" \"$etc_dir/esp-mint.secret.env\"",
    ] {
        assert!(script.contains(needle), "{needle} not in\n{script}");
    }
    assert!(!script.contains("useradd") && !script.contains("systemctl"));
    shell_syntax_ok(&out_dir.join("install-system.sh"));
}

#[test]
fn secrets_and_health_settings_are_kept_when_they_are_already_there() {
    let dir = scratch("keep");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let plan = service::plan(&spi, None).unwrap();
    let script = &plan.files.last().unwrap().1;
    for kept in [
        "keep 0640 root \"$group\" \"$here/psp-mint.secret.env\"",
        "keep 0640 root \"$group\" \"$here/esp-mint.secret.env\"",
        "keep 0644 root root \"$here/psp-mint-health.conf\"",
        "keep 0644 root root \"$here/esp-mint-health.conf\"",
    ] {
        assert!(script.contains(kept), "{kept}");
    }
    // What spi writes from spi.toml is replaced every time.
    for put in [
        "put 0640 root \"$group\" \"$here/esp-mint.toml\"",
        "put 0640 root \"$group\" \"$here/esp-mint.env\"",
    ] {
        assert!(script.contains(put), "{put}");
    }
    // The admin token is a secret and is never taken from spi.toml.
    let secret = &plan
        .files
        .iter()
        .find(|(n, _)| n == "esp-mint.secret.env")
        .unwrap()
        .1;
    assert!(secret.contains("# ESP_ADMIN_TOKEN="));
    assert!(!secret.lines().any(|l| l.starts_with("ESP_ADMIN_TOKEN")));
}

#[test]
fn installing_again_writes_the_same_files() {
    let dir = scratch("again");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let out_dir = dir.join("svc");
    assert!(install_into(&spi, &out_dir).ok);
    let first: Vec<String> = std::fs::read_dir(&out_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    let texts: Vec<String> = first.iter().map(|f| read(&out_dir, f)).collect();
    assert!(install_into(&spi, &out_dir).ok);
    for (f, t) in first.iter().zip(&texts) {
        assert_eq!(&read(&out_dir, f), t, "{f}");
    }
}

#[test]
fn the_default_folder_is_under_the_spi_folder_and_no_service_files_writes_none() {
    let dir = scratch("default-folder");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let out = commands::install(
        &spi,
        &InstallOptions {
            service_files: ServiceFiles::Default,
            ops_dir: None,
        },
    );
    assert!(out.ok, "{:?}", out.lines);
    assert!(dir.join("service/install-system.sh").exists());
    assert!(dir.join("generated/psp-mint.toml").exists());

    let dir = scratch("no-service-files");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let out = commands::install(
        &spi,
        &InstallOptions {
            service_files: ServiceFiles::Skip,
            ops_dir: None,
        },
    );
    // Without service files nothing will start bitcoind, so an absent node is
    // reported as it always was.
    assert!(!out.ok);
    assert!(
        out.lines
            .iter()
            .any(|l| l.starts_with("FAIL: bitcoind is unreachable")),
        "{:?}",
        out.lines
    );
    assert!(!dir.join("service").exists());
    assert!(dir.join("generated/psp-mint.toml").exists());
}

#[test]
fn the_service_section_sets_the_account_the_folders_and_the_bitcoind_command() {
    let dir = scratch("custom");
    let bitcoind = "rpc_url = \"http://127.0.0.1:28332\"\ndatadir = \"/srv/btc\"\nargs = [\"-txindex\", \"-dbcache=2000\"]\ncookie = \"/srv/other/.cookie\"";
    let service = "user = \"mints\"\ngroup = \"ops\"\nbin_dir = \"/opt/sp/bin\"\netc_dir = \"/opt/sp/etc\"\ndata_dir = \"/srv/sp\"\nlog_dir = \"/srv/log\"";
    let spi = load_os(
        &dir,
        &config_text(&dir, "main", bitcoind, service),
        false,
        &[],
    )
    .unwrap();
    let plan = service::plan(&spi, None).unwrap();
    let file = |n: &str| -> String {
        plan.files
            .iter()
            .find(|(name, _)| name == n)
            .unwrap_or_else(|| panic!("{n}"))
            .1
            .clone()
    };
    let psp = file("psp-mint.service");
    for line in [
        "User=mints",
        "Group=ops",
        "EnvironmentFile=/opt/sp/etc/psp-mint.env",
        "ExecStart=/opt/sp/bin/psp-mint serve",
        "ReadWritePaths=/srv/sp/psp-mint /srv/log",
        "StandardOutput=append:/srv/log/psp-mint.log",
    ] {
        assert!(psp.lines().any(|l| l == line), "{line} not in\n{psp}");
    }
    // Main has no chain flag. The port is not main's, so it is passed. The
    // datadir is the one written down.
    let btc = file("sp-bitcoind.service");
    assert!(
        btc.contains(
            "ExecStart=/opt/sp/bin/bitcoind -datadir=/srv/btc -rpccookieperms=group -rpcport=28332 -txindex -dbcache=2000"
        ),
        "{btc}"
    );
    assert!(btc.contains("ReadWritePaths=/srv/btc /srv/log"));
    // The cookie was written down, so it is used as it is.
    assert!(file("psp-mint.toml").contains("cookie = \"/srv/other/.cookie\""));
    assert!(file("esp-mint.env").contains("ESP_NETWORK=main"));
    let script = file("install-system.sh");
    assert!(script.contains("user=mints") && script.contains("group=ops"));
    assert!(script.contains("bin_dir=/opt/sp/bin"));
    assert!(script.contains("install -d -m 0750 -o \"$user\" -g \"$group\" \"$data_dir/psp-mint\" \"$data_dir/esp-mint\" /srv/btc"));
    // The cookie is not where the service's bitcoind writes it: said.
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("so psp-mint reads it there")),
        "{:?}",
        plan.warnings
    );
}

#[test]
fn the_cookie_follows_the_chain_folder_of_the_service_datadir() {
    for (network, sub) in [
        ("main", ""),
        ("regtest", "regtest/"),
        ("signet", "signet/"),
        ("testnet", "testnet3/"),
    ] {
        let dir = scratch("cookie");
        let spi = load_os(&dir, &config_text(&dir, network, "", ""), false, &[]).unwrap();
        let plan = service::plan(&spi, None).unwrap();
        let toml = &plan
            .files
            .iter()
            .find(|(n, _)| n == "psp-mint.toml")
            .unwrap()
            .1;
        assert!(
            toml.contains(&format!(
                "cookie = \"/var/lib/sp-mint/bitcoind/{sub}.cookie\""
            )),
            "{network}: {toml}"
        );
        assert!(plan.warnings.is_empty(), "{network}: {:?}", plan.warnings);
    }
}

#[test]
fn a_cookie_written_down_at_the_service_path_is_used_without_a_warning() {
    let dir = scratch("cookie-same");
    let spi = load_os(
        &dir,
        &config_text(
            &dir,
            "regtest",
            "cookie = \"/var/lib/sp-mint/bitcoind/regtest/.cookie\"",
            "",
        ),
        false,
        &[],
    )
    .unwrap();
    let plan = service::plan(&spi, None).unwrap();
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);

    // Left to its default, the cookie is the service's, and the note says how
    // to read it as yourself.
    let dir = scratch("cookie-note");
    let spi = load_os(&dir, &config_text(&dir, "regtest", "", ""), false, &[]).unwrap();
    let plan = service::plan(&spi, None).unwrap();
    assert_eq!(plan.notes.len(), 1);
    let note = &plan.notes[0];
    assert!(
        note.contains("add yourself to the group `spmint`"),
        "{note}"
    );
    assert!(
        note.contains("cookie = \"/var/lib/sp-mint/bitcoind/regtest/.cookie\""),
        "{note}"
    );
}

#[test]
fn a_bitcoind_that_spi_does_not_manage_gets_no_unit_and_a_warning_about_its_cookie() {
    let dir = scratch("unmanaged");
    let spi = load_os(
        &dir,
        &config_text(&dir, "regtest", &format!("{NOWHERE}\nmanage = false"), ""),
        false,
        &[],
    )
    .unwrap();
    let plan = service::plan(&spi, None).unwrap();
    let names: Vec<&str> = plan.files.iter().map(|(n, _)| n.as_str()).collect();
    assert!(!names.contains(&"sp-bitcoind.service"));
    let psp = &plan
        .files
        .iter()
        .find(|(n, _)| n == "psp-mint.service")
        .unwrap()
        .1;
    assert!(psp.contains("After=network-online.target\n"), "{psp}");
    assert!(!psp.contains("sp-bitcoind"));
    // The default cookie is in the home folder of whoever ran spi: a service
    // user can not read it, and ProtectHome would hide it.
    assert!(psp.contains("ProtectHome=read-only"));
    assert!(
        plan.warnings.iter().any(|w| w.contains("home folder")),
        "{:?}",
        plan.warnings
    );
    assert!(!plan.start[0].contains("bitcoind"));
    // An absent node is a failure of install, as it always was, when nothing
    // here will start it.
    let out = install_into(&spi, &dir.join("svc"));
    assert!(!out.ok);
    assert!(dir.join("svc/install-system.sh").exists());
}

#[test]
fn without_health_monitors_there_are_none() {
    let dir = scratch("nohealth");
    let spi = load_os(
        &dir,
        &config_text(&dir, "regtest", "", "health = false"),
        false,
        &[],
    )
    .unwrap();
    let plan = service::plan(&spi, None).unwrap();
    let names: Vec<&str> = plan.files.iter().map(|(n, _)| n.as_str()).collect();
    for gone in [
        "sp-health-monitor.sh",
        "psp-mint-health.service",
        "psp-mint-health.conf",
        "esp-mint-health.service",
    ] {
        assert!(!names.contains(&gone), "{gone}");
    }
    assert_eq!(plan.start.len(), 3);
    assert_eq!(
        plan.start[0],
        "sudo systemctl enable --now sp-bitcoind.service"
    );
    assert!(plan.start[1].starts_with("spi install"));
    assert_eq!(
        plan.start[2],
        "sudo systemctl enable --now psp-mint.service esp-mint.service"
    );
}

#[test]
fn a_binary_that_is_not_found_is_a_warning_and_a_guard_in_the_script_and_not_a_failure() {
    let dir = scratch("nobinary");
    let text = "[psp]\nbin = \"/nonexistent/psp-mint\"\n[esp]\nbin = \"esp-mint-that-is-not-installed\"\n[bitcoind]\nbin = \"bitcoind-that-is-not-installed\"\n";
    let spi = load_os(&dir, text, false, &[]).unwrap();
    let plan = service::plan(&spi, None).unwrap();
    assert_eq!(plan.warnings.len(), 3, "{:?}", plan.warnings);
    assert!(plan.warnings[0].contains("/nonexistent/psp-mint"));
    assert!(plan.warnings[0].contains("/usr/local/bin/psp-mint"));
    let script = &plan.files.last().unwrap().1;
    for n in ["psp-mint", "esp-mint", "bitcoind"] {
        assert!(
            script.contains(&format!("need \"$bin_dir/{n}\" {n}")),
            "{n}"
        );
    }
    assert!(!script.contains("put 0755 root root /nonexistent"));
}

#[test]
fn ops_replaces_a_template_and_is_not_needed_for_the_rest() {
    let dir = scratch("ops");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let ops = dir.join("my-ops");
    std::fs::create_dir_all(ops.join("systemd")).unwrap();
    std::fs::write(
        ops.join("systemd/esp-mint.service"),
        "[Service]\nExecStart={{bin_dir}}/{{name}} serve --mine\n",
    )
    .unwrap();
    std::fs::write(ops.join("sp-health-monitor.sh"), "#!/bin/sh\necho mine\n").unwrap();
    let plan = service::plan(&spi, Some(&ops)).unwrap();
    let file = |n: &str| -> String {
        plan.files
            .iter()
            .find(|(name, _)| name == n)
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(
        file("esp-mint.service"),
        "[Service]\nExecStart=/usr/local/bin/esp-mint serve --mine\n"
    );
    assert_eq!(file("sp-health-monitor.sh"), "#!/bin/sh\necho mine\n");
    // Everything else is the embedded one.
    assert!(file("psp-mint.service").contains("ExecStart=/usr/local/bin/psp-mint serve"));
    assert!(file("sp-bitcoind.service").contains("bitcoind -regtest"));
}

#[test]
fn ops_that_is_not_a_folder_or_has_a_bad_placeholder_stops_before_anything_is_written() {
    let dir = scratch("ops-bad");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();

    let e = service::plan(&spi, Some(&dir.join("not-there"))).unwrap_err();
    assert!(e.contains("is not a folder"), "{e}");

    let ops = dir.join("ops");
    std::fs::create_dir_all(ops.join("systemd")).unwrap();
    std::fs::write(ops.join("systemd/psp-mint.service"), "X={{no_such_key}}\n").unwrap();
    let e = service::plan(&spi, Some(&ops)).unwrap_err();
    assert!(
        e.contains("{{no_such_key}}") && e.contains("psp-mint.service"),
        "{e}"
    );
    std::fs::write(ops.join("systemd/psp-mint.service"), "X={{bin_dir\n").unwrap();
    let e = service::plan(&spi, Some(&ops)).unwrap_err();
    assert!(e.contains("never closed"), "{e}");

    // Through install: a failure, with the folder untouched.
    let out_dir = dir.join("svc");
    let out = commands::install(
        &spi,
        &InstallOptions {
            service_files: ServiceFiles::Into(out_dir.clone()),
            ops_dir: Some(ops),
        },
    );
    assert!(!out.ok);
    assert!(
        out.lines
            .iter()
            .any(|l| l.starts_with("FAIL: cannot prepare the service files")),
        "{:?}",
        out.lines
    );
    assert!(!out_dir.exists());
}

#[test]
fn definitions_with_no_placeholders_still_work_as_an_override() {
    // A folder laid out like the earlier definitions: fixed text, no placeholders.
    let dir = scratch("ops-old");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let ops = dir.join("ops");
    std::fs::create_dir_all(ops.join("systemd")).unwrap();
    std::fs::write(ops.join("systemd/psp-mint.service"), "[Unit] fixed\n").unwrap();
    let plan = service::plan(&spi, Some(&ops)).unwrap();
    let psp = &plan
        .files
        .iter()
        .find(|(n, _)| n == "psp-mint.service")
        .unwrap()
        .1;
    assert_eq!(psp, "[Unit] fixed\n");
}

#[test]
fn a_value_that_would_break_a_unit_is_refused_and_named() {
    let cases: [(&str, &str, &str); 5] = [
        ("", "bin_dir = \"/opt/my bin\"", "service.bin_dir"),
        ("", "data_dir = \"/srv/it's\"", "service.data_dir"),
        ("datadir = \"/srv/my btc\"", "", "bitcoind data folder"),
        ("args = [\"-rpcauth=a b\"]", "", "bitcoind argument"),
        ("args = [\"-daemon\"]", "", "foreground"),
    ];
    for (bitcoind, service_text, needle) in cases {
        let dir = scratch("refused");
        let spi = load_os(
            &dir,
            &config_text(
                &dir,
                "regtest",
                &format!("{NOWHERE}\n{bitcoind}"),
                service_text,
            ),
            false,
            &[],
        )
        .unwrap();
        let e = service::plan(&spi, None).unwrap_err();
        assert!(e.contains(needle), "{needle} not in {e}");
        let out_dir = dir.join("svc");
        let out = install_into(&spi, &out_dir);
        assert!(!out.ok, "{needle}");
        assert!(!out_dir.exists(), "{needle}");
        // The configs for `spi start` are still written.
        assert!(dir.join("generated/psp-mint.toml").exists());
    }
    // A datadir with a space is fine when spi does not put it in a unit.
    let dir = scratch("space-ok");
    let spi = load_os(
        &dir,
        &config_text(
            &dir,
            "regtest",
            "manage = false\ndatadir = \"/srv/my btc\"",
            "",
        ),
        false,
        &[],
    )
    .unwrap();
    assert!(service::plan(&spi, None).is_ok());
}

#[test]
fn a_mint_setting_that_spi_names_still_stops_the_service_files() {
    let dir = scratch("conflict");
    let text = format!(
        "{}\n[esp.settings.psp]\nurl = \"http://elsewhere\"\n",
        config_text(&dir, "regtest", NOWHERE, "")
    );
    let spi = load_os(&dir, &text, false, &[]).unwrap();
    let out_dir = dir.join("svc");
    let out = install_into(&spi, &out_dir);
    assert!(!out.ok);
    assert!(!out_dir.exists());
}

#[test]
fn the_service_settings_have_defaults_per_system_and_are_checked() {
    let dir = scratch("service-config");
    let linux = load_os(&dir, "", false, &[]).unwrap();
    let s = &linux.service;
    assert_eq!(
        (s.user.as_str(), s.group.as_str(), s.bin_dir.as_str()),
        ("spmint", "spmint", "/usr/local/bin")
    );
    assert_eq!(
        (s.etc_dir.as_str(), s.data_dir.as_str(), s.log_dir.as_str()),
        ("/etc/sp-mint", "/var/lib/sp-mint", "/var/log/sp-mint")
    );
    assert!(s.health);
    let mac = load_os(&dir, "", true, &[]).unwrap();
    assert_eq!(
        (mac.service.user.as_str(), mac.service.group.as_str()),
        ("_spmint", "_spmint")
    );

    // The environment wins, like every other setting.
    let from_env = load_os(
        &dir,
        "[service]\nuser = \"a\"",
        false,
        &[
            ("SPI_SERVICE_USER", "b"),
            ("SPI_SERVICE_LOG_DIR", "/var/log/x/"),
        ],
    )
    .unwrap();
    assert_eq!(from_env.service.user, "b");
    assert_eq!(from_env.service.group, "b");
    assert_eq!(from_env.service.log_dir, "/var/log/x");
    assert!(
        from_env
            .shown
            .iter()
            .any(|(k, v, f)| k == "service.user" && v == "b" && f == "env SPI_SERVICE_USER")
    );

    let bad = "[service]\nuser = \"Bad User\"\nbin_dir = \"relative/bin\"\ndata_dir = \"/\"\nhealth = \"yes\"\nshell = \"x\"";
    let all = load_os(&dir, bad, false, &[]).unwrap_err().join("\n");
    for needle in [
        "service.user is `Bad User`",
        "service.bin_dir is `relative/bin`",
        "service.data_dir is `/`",
        "[service] health must be true or false",
        "[service] shell is not a setting of spi",
    ] {
        assert!(all.contains(needle), "{needle} not in {all}");
    }
}

#[test]
fn the_embedded_monitor_is_the_one_in_the_repository() {
    let dir = scratch("monitor");
    let spi = load_os(&dir, &config_text(&dir, "regtest", NOWHERE, ""), false, &[]).unwrap();
    let plan = service::plan(&spi, None).unwrap();
    let monitor = &plan
        .files
        .iter()
        .find(|(n, _)| n == "sp-health-monitor.sh")
        .unwrap()
        .1;
    assert_eq!(monitor, service::HEALTH_MONITOR);
    assert!(monitor.starts_with("#!/bin/sh\n"));
}
