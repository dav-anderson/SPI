//! The `spi` command line. See the README.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use spi::commands::{self, InstallOptions, PinPolicy, ServiceFiles, UpgradeArgs, When};
use spi::config::{Inputs, Spi};
use spi::exec::System;
use spi::fetch::{self, CurlSource};
use spi::release;
use spi::{audit, run, schedule, setup, version, versions};

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
  start           For trying spi out; a machine with services uses them. Write
                  the config files again; start bitcoind if it is not running; wait
                  for it to finish syncing (bitcoind.sync_wait_secs); then start the
                  AIR server, psp-mint and esp-mint. Refuses to start without
                  [air] name and the AIR server's identity file.
  stop            For trying spi out and for decommission only: stops esp-mint,
                  psp-mint and the AIR server (air_server), and bitcoind if spi
                  started it.
  reload          Write the config files again and signal the mints (SIGHUP) so they
                  read them without stopping. A setting that needs a restart keeps
                  its running value: reload names each one that changed, says how to
                  restart (a brief downtime) and restarts nothing.
  restart NAME    Restart psp-mint or esp-mint (a brief downtime), so that a setting
                  that needs a restart takes effect. It warns and asks first.
  snapshot        Ask psp-mint and esp-mint for a live snapshot (SIGUSR1, through the
                  service manager). They keep serving.
  publish-balance Publish the SPI wallet's PSP balance for psp-mint's burn threshold
                  ([psp] burn). The timer `spi install` writes runs it.
  status [--json] Health of bitcoind (unreachable, syncing or synced), the AIR
                  server, psp-mint and esp-mint (also `health`), then the setup
                  steps and whether the SPI is ready. The service is ok, degraded
                  (bitcoind syncing: PSP not ready) or down. Exit 1 unless every
                  component is ok.
  setup           The steps to a working SPI (bitcoind, AIR server, PSP, ESP, PSP in
                  the SPI wallet, FSP), which are done, and what to run next. It
                  starts nothing.
  collect         Take the PSP psp-mint reissued to the SPI into the SPI's wallet.
  versions [--json]
                  The version of every program and library, or `unknown` and why.
  upgrade [NAME...] [--dry-run] [--at HH:MM | --in DURATION]
          [--keep-pins | --reset-pins]
                  Bring bitcoind, air_server, psp-mint and esp-mint (all four with no
                  name) to the newest release, or to the `version` spi.toml pins,
                  one at a time, on a service install. Each is stopped and the new
                  build started (a brief downtime; the line for each says why).
                  It warns that the AIR service and every mint it runs are shut
                  down, and asks before it changes anything. A pinned program that
                  is behind the newest release is warned about and asked about
                  (keep the pin, or reset it: --keep-pins, --reset-pins).
                  --dry-run says what it would do and changes nothing. --at HH:MM
                  (the next HH:MM, local clock) or --in DURATION (6h, 90m, 1h30m)
                  schedules it instead: you confirm now, the job asks nothing.
  upgrade check [--json]
                  For each program: installed version, spi.toml version, newest
                  release, and whether it is up to date. Changes nothing.
  upgrade status  The pending scheduled upgrade, or that none is.
  upgrade cancel  Cancel the pending scheduled upgrade.
  config show     The settings in use and where each came from.
  audit [--json]  What the PSP and ESP mints owe: PSP outstanding, ESP matured
                  and unmatured, each in its unit, in PSP and in USD, the PSP paid to
                  the SPI, and the SPI wallet's balance. FSP is not counted.
  --version, -V   spi's version and the libraries it was built with.

Network: regtest unless spi.toml (network = ...) or SPI_NETWORK says otherwise.
Settings: spi.toml in DIR (default ~/.spi; SPI_DIR, SPI_CONFIG), then SPI_* variables.";

fn unknown_command(command: &str) -> String {
    format!("unknown command `{command}`. Run with --help")
}

/// Refuse a command, or an argument of it, that `spi` does not have, before
/// anything is read, written or contacted. `rest` is what follows the command.
fn check_args(command: &str, rest: &[String]) -> Result<(), String> {
    // (flags with no value, flags that take the next argument as their value)
    let (flags, value_flags): (&[&str], &[&str]) = match command {
        "install" => (&["--no-service-files"], &["--service-files", "--ops"]),
        "start" | "stop" | "reload" | "setup" | "collect" => (&[], &[]),
        "status" | "health" | "audit" | "versions" => (&["--json"], &[]),
        "upgrade" => return check_upgrade_args(rest),
        "restart" => return check_restart_args(rest),
        "snapshot" | "publish-balance" => (&[], &[]),
        "config" => {
            return match rest {
                [word] if word == "show" => Ok(()),
                [word, extra, ..] if word == "show" => Err(format!(
                    "`{extra}` is not an argument of `spi config show`. Run with --help"
                )),
                _ => Err("usage: spi config show".into()),
            };
        }
        other => return Err(unknown_command(other)),
    };
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        if flags.contains(&arg.as_str()) {
            continue;
        }
        if value_flags.contains(&arg.as_str()) {
            if args.next().is_none() {
                return Err(format!("{arg} needs a value"));
            }
            continue;
        }
        return Err(format!(
            "`{arg}` is not an argument of `spi {command}`. Run with --help"
        ));
    }
    if rest.iter().any(|a| a == "--service-files") && rest.iter().any(|a| a == "--no-service-files")
    {
        return Err("--service-files and --no-service-files cannot be given together".into());
    }
    Ok(())
}

/// The arguments of `spi upgrade`: a subcommand (`check`, `status`, `cancel`,
/// `run-scheduled`), or program names with the flags.
fn check_upgrade_args(rest: &[String]) -> Result<(), String> {
    let wrong =
        |arg: &str, of: &str| format!("`{arg}` is not an argument of `spi {of}`. Run with --help");
    if let Some(sub) = rest
        .first()
        .filter(|a| ["check", "status", "cancel", "run-scheduled"].contains(&a.as_str()))
    {
        let allowed: &[&str] = if sub == "check" { &["--json"] } else { &[] };
        return match rest[1..].iter().find(|a| !allowed.contains(&a.as_str())) {
            Some(extra) => Err(wrong(extra, &format!("upgrade {sub}"))),
            None => Ok(()),
        };
    }
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" | "--keep-pins" | "--reset-pins" => {}
            "--at" | "--in" => {
                if args.next().is_none() {
                    return Err(format!("{arg} needs a value"));
                }
            }
            other if !other.starts_with('-') => {}
            other => return Err(wrong(other, "upgrade")),
        }
    }
    let has = |f: &str| rest.iter().any(|a| a == f);
    if has("--at") && has("--in") {
        return Err("--at and --in cannot be given together".into());
    }
    if has("--keep-pins") && has("--reset-pins") {
        return Err("--keep-pins and --reset-pins cannot be given together".into());
    }
    Ok(())
}

fn check_restart_args(rest: &[String]) -> Result<(), String> {
    match rest {
        [name] if !name.starts_with('-') => Ok(()),
        _ => Err("usage: spi restart psp-mint|esp-mint".into()),
    }
}

/// Show `lines` and ask whether to go on. Only a typed `yes` (or `y`) is a yes;
/// with no terminal there is nobody to ask, so nothing is changed.
fn terminal_confirm(lines: &[String]) -> bool {
    lines.iter().for_each(|l| println!("{l}"));
    if !std::io::stdin().is_terminal() {
        println!("There is no terminal to ask on, so nothing was changed. Run it from a terminal.");
        return false;
    }
    print!("Type yes to go on: ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "yes" | "y")
}

/// Show `lines` (a pin and the question about it) and ask: `Some(true)` is
/// reset, `Some(false)` keep, `None` when there is no terminal.
fn terminal_pin(lines: &[String]) -> Option<bool> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    lines.iter().for_each(|l| println!("{l}"));
    loop {
        print!("keep or reset? ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => return Some(false),
            Ok(_) => {}
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "keep" | "k" => return Some(false),
            "reset" | "r" => return Some(true),
            _ => println!("Type keep or reset."),
        }
    }
}

fn real_main(args: Vec<String>) -> Result<bool, String> {
    let mut args = args;
    let mut dir_flag = None;
    let mut config_flag = None;
    let mut network_flag = None;
    // Global flags may come before the command.
    while let Some(flag @ ("--dir" | "--config" | "--network")) = args.first().map(String::as_str) {
        if args.len() < 2 {
            return Err(format!("{flag} needs a value"));
        }
        let value = Some(args[1].clone());
        match flag {
            "--dir" => dir_flag = value,
            "--config" => config_flag = value,
            _ => network_flag = value,
        }
        args.drain(..2);
    }
    // Before any setting is read: no HOME or spi.toml is needed.
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        version::lines().iter().for_each(|l| println!("{l}"));
        return Ok(true);
    }
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(true);
    }
    check_args(&args[0], &args[1..])?;
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let env = move |name: &str| -> Option<String> {
        if name == "SPI_NETWORK"
            && let Some(n) = &network_flag
        {
            return Some(n.clone());
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
    let spi = Spi::load(&Inputs {
        dir,
        file: file.as_deref(),
        env: &env,
        home,
        os: std::env::consts::OS,
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
            // `check_args` has refused the two together.
            let skip = rest.iter().any(|a| a == "--no-service-files");
            let service_files = match (skip, flag("--service-files")) {
                (true, _) => ServiceFiles::Skip,
                (false, Some(dir)) => ServiceFiles::Into(PathBuf::from(dir)),
                (false, None) => ServiceFiles::Default,
            };
            let curl = CurlSource::new(&spi, &System);
            let (spi, notes) = if skip {
                (spi.clone(), Vec::new())
            } else {
                fetch::prepare_install(&spi, &System, &curl)?
            };
            notes.iter().for_each(|l| println!("{l}"));
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
            let lines = run::stop(&spi, 120)?;
            lines.iter().for_each(|l| println!("{l}"));
            Ok(true)
        }
        "reload" => {
            let out = commands::reload(&spi);
            out.lines.iter().for_each(|l| println!("{l}"));
            Ok(out.ok)
        }
        "setup" => {
            let out = commands::setup(&spi, &System);
            out.lines.iter().for_each(|l| println!("{l}"));
            Ok(out.ok)
        }
        "collect" => {
            let out = commands::collect(&spi, &System);
            out.lines.iter().for_each(|l| println!("{l}"));
            Ok(out.ok)
        }
        "versions" => {
            let lines = commands::versions(&spi, &System);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&lines).map_err(|e| e.to_string())?
                );
            } else {
                print!("{}", versions::render(&lines));
            }
            Ok(true)
        }
        "upgrade" => {
            // `check_args` has refused any argument that is not one of these.
            let exe =
                std::env::current_exe().map_err(|e| format!("cannot find the spi binary: {e}"))?;
            let config = std::path::absolute(&config_path).unwrap_or(config_path.clone());
            let job = schedule::Where::system(&spi, exe, config.clone());
            let now = jiff::Zoned::now();
            let probe = commands::probe_of(&spi);
            let prompts = commands::Prompts {
                confirm: &terminal_confirm,
                pin: &terminal_pin,
            };
            let curl = CurlSource::new(&spi, &System);
            let around = commands::Around {
                exec: &System,
                source: &curl,
                prompts: &prompts,
                probe: &probe,
                config: &config,
                now: &now,
                job: &job,
            };
            match rest.first().map(String::as_str) {
                Some("check") => {
                    let checked = commands::upgrade_check(&spi, &System, &curl);
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&checked.lines)
                                .map_err(|e| e.to_string())?
                        );
                    } else {
                        print!("{}", release::render(&checked.lines));
                        checked.warning.iter().for_each(|l| println!("{l}"));
                    }
                    Ok(true)
                }
                Some("status") => {
                    let out = commands::upgrade_status(&spi, &around);
                    out.lines.iter().for_each(|l| println!("{l}"));
                    Ok(out.ok)
                }
                Some("cancel") => {
                    let out = commands::upgrade_cancel(&spi, &around);
                    out.lines.iter().for_each(|l| println!("{l}"));
                    Ok(out.ok)
                }
                Some("run-scheduled") => {
                    let out = commands::run_scheduled(&spi, 60, &around);
                    out.lines.iter().for_each(|l| println!("{l}"));
                    Ok(out.ok)
                }
                _ => {
                    let names: Vec<String> = {
                        let mut skip = false;
                        let mut names = Vec::new();
                        for a in &rest {
                            if skip {
                                skip = false;
                            } else if a == "--at" || a == "--in" {
                                skip = true;
                            } else if !a.starts_with('-') {
                                names.push(a.clone());
                            }
                        }
                        names
                    };
                    let when = match (flag("--at"), flag("--in")) {
                        (Some(at), _) => {
                            let (h, m) = schedule::parse_at(&at)?;
                            When::At(h, m)
                        }
                        (None, Some(span)) => When::In(schedule::parse_duration(&span)?),
                        (None, None) => When::Now,
                    };
                    let pins = if rest.iter().any(|a| a == "--keep-pins") {
                        PinPolicy::Keep
                    } else if rest.iter().any(|a| a == "--reset-pins") {
                        PinPolicy::Reset
                    } else {
                        PinPolicy::Ask
                    };
                    let args = UpgradeArgs {
                        names: &names,
                        dry_run: rest.iter().any(|a| a == "--dry-run"),
                        when,
                        pins,
                        wait_secs: 60,
                    };
                    let out = commands::upgrade(&spi, &args, &around);
                    out.lines.iter().for_each(|l| println!("{l}"));
                    Ok(out.ok)
                }
            }
        }
        "restart" => {
            let exe =
                std::env::current_exe().map_err(|e| format!("cannot find the spi binary: {e}"))?;
            let config = std::path::absolute(&config_path).unwrap_or(config_path.clone());
            let job = schedule::Where::system(&spi, exe, config.clone());
            let now = jiff::Zoned::now();
            let probe = commands::probe_of(&spi);
            let prompts = commands::Prompts {
                confirm: &terminal_confirm,
                pin: &terminal_pin,
            };
            let curl = CurlSource::new(&spi, &System);
            let around = commands::Around {
                exec: &System,
                source: &curl,
                prompts: &prompts,
                probe: &probe,
                config: &config,
                now: &now,
                job: &job,
            };
            let out = commands::restart(&spi, &rest[0], 60, &around);
            out.lines.iter().for_each(|l| println!("{l}"));
            Ok(out.ok)
        }
        "snapshot" => {
            let out = commands::snapshot(&spi, &System);
            out.lines.iter().for_each(|l| println!("{l}"));
            Ok(out.ok)
        }
        "publish-balance" => {
            let out = commands::publish_balance(&spi, &System);
            out.lines.iter().for_each(|l| println!("{l}"));
            Ok(out.ok)
        }
        "status" | "health" => {
            let parts = commands::status(&spi);
            let all = parts.iter().all(|c| c.ok);
            let service = commands::service_state(&parts);
            let steps = commands::setup_steps(&spi, &parts, &System);
            let ready = setup::ready(&steps);
            if json {
                let v = serde_json::json!({"network": spi.network.name(), "service": service, "ok": all, "components": parts, "steps": steps, "ready": ready});
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
                for step in &steps {
                    println!(
                        "[{}] {}. {}{}: {}",
                        if step.done { "x" } else { " " },
                        step.id,
                        step.name,
                        if step.required { "" } else { " (optional)" },
                        step.detail
                    );
                }
                println!("ready: {}", if ready { "yes" } else { "NO" });
            }
            Ok(all)
        }
        "config" => {
            let shown_file = file.as_ref().map(|_| config_path.as_path());
            commands::config_show(&spi, shown_file)
                .iter()
                .for_each(|l| println!("{l}"));
            Ok(true)
        }
        "audit" => {
            let report = commands::audit(&spi, &System)?;
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
        // `check_args` has refused any other command.
        other => Err(unknown_command(other)),
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
