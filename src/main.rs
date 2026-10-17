//! The `spi` command line. See the README.

use std::path::PathBuf;
use std::process::ExitCode;

use spi::commands::{self, InstallOptions, ServiceFiles};
use spi::config::{Inputs, Spi};
use spi::{audit, run};

const USAGE: &str = "\
spi [--dir DIR] [--config FILE] [--network NAME] <command>

  install [--service-files DIR | --no-service-files] [--ops DIR]
                  Set the machine up from spi.toml (a starter is written first if
                  there is none): make the data folders, write the psp-mint and
                  esp-mint config files, check that bitcoind is reachable and its
                  wallet is loaded (created if not). It also writes everything that
                  runs the mints, and bitcoind unless bitcoind.manage is false, as
                  services that start at boot: the unit files (Linux) or launchd
                  plists (macOS), the env and config files, the health monitors and
                  install-system.sh, into DIR (default <spi folder>/service). It
                  installs nothing itself: it prints the sudo command that runs the
                  script and the commands that start the services. The definitions
                  are inside spi; --ops DIR replaces any of them with a file of the
                  same name from DIR. --no-service-files writes none.
  start           Write the config files again; start bitcoind if it is not running;
                  wait for it to finish syncing (bitcoind.sync_wait_secs); then start
                  psp-mint and esp-mint.
  stop            Stop esp-mint, psp-mint, and bitcoind if spi started it.
  status [--json] Health of bitcoind (unreachable, syncing or synced), psp-mint and
                  esp-mint (also `health`). The service is ok, degraded (bitcoind
                  syncing: PSP not ready) or down. Exit 1 unless ok.
  config show     The settings in use and where each came from.
  audit [--json]  What the PSP and ESP mints owe: PSP outstanding, ESP matured,
                  unmatured and unsplit, each in its unit, in PSP and in USD. FSP
                  is not counted.

Network: regtest unless spi.toml (network = ...) or SPI_NETWORK says otherwise.
Settings: spi.toml in DIR (default ~/.spi; SPI_DIR, SPI_CONFIG), then SPI_* variables.";

fn real_main(args: Vec<String>) -> Result<bool, String> {
    let mut args = args;
    let mut dir_flag = None;
    let mut config_flag = None;
    let mut network_flag = None;
    // Global flags may come before the command.
    loop {
        match args.first().map(String::as_str) {
            Some("--dir") if args.len() > 1 => {
                dir_flag = Some(args[1].clone());
                args.drain(..2);
            }
            Some("--config") if args.len() > 1 => {
                config_flag = Some(args[1].clone());
                args.drain(..2);
            }
            Some("--network") if args.len() > 1 => {
                network_flag = Some(args[1].clone());
                args.drain(..2);
            }
            _ => break,
        }
    }
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(true);
    }
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let env = move |name: &str| -> Option<String> {
        if name == "SPI_NETWORK" {
            if let Some(n) = &network_flag {
                return Some(n.clone());
            }
        }
        std::env::var(name).ok()
    };
    let dir = PathBuf::from(
        dir_flag
            .or_else(|| env("SPI_DIR").filter(|d| !d.is_empty()))
            .unwrap_or_else(|| format!("{home}/.spi")),
    );
    let explicit_config = config_flag.or_else(|| env("SPI_CONFIG").filter(|c| !c.is_empty()));
    let config_path = explicit_config
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("spi.toml"));
    let command = args[0].clone();
    let rest: Vec<String> = args[1..].to_vec();

    if command == "install" && explicit_config.is_none() && !config_path.exists() {
        commands::write_starter(&config_path)?;
        println!("wrote a starter {}", config_path.display());
    }
    let file = match std::fs::read_to_string(&config_path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit_config.is_none() => None,
        Err(e) => return Err(format!("cannot read {}: {e}", config_path.display())),
    };
    let mac = cfg!(target_os = "macos");
    let spi = Spi::load(&Inputs {
        dir,
        file: file.as_deref(),
        env: &env,
        home,
        mac,
    })
    .map_err(|p| format!("the settings are not valid:\n  {}", p.join("\n  ")))?;
    let json = rest.iter().any(|a| a == "--json");
    let flag = |name: &str| -> Option<String> {
        rest.iter()
            .position(|a| a == name)
            .and_then(|i| rest.get(i + 1).cloned())
    };

    match command.as_str() {
        "install" => {
            let skip = rest.iter().any(|a| a == "--no-service-files");
            let service_files = match (skip, flag("--service-files")) {
                (true, Some(_)) => {
                    return Err("--service-files and --no-service-files together".into());
                }
                (true, None) => ServiceFiles::Skip,
                (false, Some(dir)) => ServiceFiles::Into(PathBuf::from(dir)),
                (false, None) => ServiceFiles::Default,
            };
            let out = commands::install(
                &spi,
                &InstallOptions {
                    service_files,
                    ops_dir: flag("--ops")
                        .or_else(|| env("SPI_OPS_DIR").filter(|d| !d.is_empty()))
                        .map(PathBuf::from),
                },
            );
            out.lines.iter().for_each(|l| println!("{l}"));
            Ok(out.ok)
        }
        "start" => {
            let lines = run::start(&spi, 60)?;
            lines.iter().for_each(|l| println!("{l}"));
            Ok(true)
        }
        "stop" => {
            let lines = run::stop(&spi, 30)?;
            lines.iter().for_each(|l| println!("{l}"));
            Ok(true)
        }
        "status" | "health" => {
            let parts = commands::status(&spi);
            let all = parts.iter().all(|c| c.ok);
            let service = commands::service_state(&parts);
            if json {
                let v = serde_json::json!({"network": spi.network.name(), "service": service, "ok": all, "components": parts});
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
                );
            } else {
                println!("network: {}", spi.network.name());
                println!("service: {service}");
                for c in &parts {
                    println!(
                        "{:<4} {:<9} {}",
                        if c.ok { "ok" } else { "DOWN" },
                        c.name,
                        c.detail
                    );
                }
            }
            Ok(all)
        }
        "config" => {
            if rest.first().map(String::as_str) != Some("show") {
                return Err("usage: spi config show".into());
            }
            let shown_file = file.as_ref().map(|_| config_path.as_path());
            commands::config_show(&spi, shown_file)
                .iter()
                .for_each(|l| println!("{l}"));
            Ok(true)
        }
        "audit" => {
            let report = commands::audit(&spi)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                print!("{}", audit::render(&report, spi.network.name()));
            }
            Ok(true)
        }
        other => Err(format!("unknown command `{other}`. Run with --help")),
    }
}

fn main() -> ExitCode {
    match real_main(std::env::args().skip(1).collect()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
