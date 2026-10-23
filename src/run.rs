//! Start and stop the AIR server and the two mints as background processes of
//! the current user.
//! (For a machine that boots and restarts them, `spi install` writes the launchd
//! or systemd definitions and a script that installs them; see `service`.)
//!
//! `start` writes the generated configs again first, so the files the mints
//! read are never older than `spi.toml`. A mint's pid is kept in
//! `<dir>/run/<mint>.pid`, its log in `<dir>/logs/<mint>.log`; the AIR
//! server's in `<dir>/run/air_server.pid` and `<dir>/logs/air_server.log`.
//!
//! **Order.** bitcoind, the AIR server, psp-mint, esp-mint; `stop` goes the
//! other way. The mints dial the AIR server, so it answers before they start.
//!
//! **Reload.** `reload` writes the config files again and sends SIGHUP to
//! each mint it started: the mint applies every setting that can change while
//! it runs, and keeps serving. A setting that cannot (the mint's README marks
//! each one "needs a restart") keeps its running value until the mint is
//! restarted, which is a brief downtime. `reload` names each such setting that
//! changed, says how to restart and what that costs, and restarts nothing.
//!
//! **Order, and why psp-mint waits for the node.** `start` starts bitcoind
//! first (unless it is already answering, or `bitcoind.manage = false`), then
//! waits until bitcoind has finished its initial block download, and only then
//! starts psp-mint and esp-mint. The alternative, starting psp-mint at once
//! and letting it report not-ready, was rejected: psp-mint would be scanning a
//! chain that is behind, and would find, lock the price of and settle payments
//! on whatever part of the chain it can see, with the ESP mint and wallets
//! already pointed at it. Waiting means psp-mint never runs against a node
//! that is behind. The wait is bounded (`bitcoind.sync_wait_secs`, 600 by
//! default); when it runs out, `start` stops with the node's progress and
//! starts neither mint. bitcoind keeps syncing; run `start` again later.
//! `status` reports the same thing as "not ready" for a mint that was started
//! some other way.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::bitcoind::{Chain, Node, Rpc};
use crate::config::{Network, Spi, Which, url_of};
use crate::{commands, generate, http};

fn read_pid_file(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| t.trim().parse().ok())
}

fn read_pid(spi: &Spi, which: Which) -> Option<u32> {
    read_pid_file(&spi.pid_file(which))
}

/// Send `signal` (`TERM`, `HUP`) to `pid`.
fn signal(signal: &str, pid: u32) -> Result<(), String> {
    let sent = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .map_err(|e| e.to_string())?;
    if sent.success() {
        Ok(())
    } else {
        Err(format!("cannot signal pid {pid}"))
    }
}

/// Whether a process with this pid is running (`kill -0`).
pub fn alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn healthy(spi: &Spi, which: Which) -> bool {
    matches!(
        http::get_json(&format!("{}/health", spi.url(which))),
        Ok((200, _))
    )
}

/// What a mint's `/health` answers, for `spi upgrade`: whether it answers 200,
/// and the `version` it reports.
pub fn probe(spi: &Spi, which: Which) -> crate::upgrade::Probe {
    match http::get_json(&format!("{}/health", spi.url(which))) {
        Ok((200, body)) => crate::upgrade::Probe {
            up: true,
            version: body
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
        _ => crate::upgrade::Probe::default(),
    }
}

/// What the AIR server's health listener is asked (`AIR_HEALTH_ADDR`): it
/// answers 200 and a bare `ok`.
pub fn air_health_url(spi: &Spi) -> String {
    format!("{}/", url_of(&spi.air.health_addr))
}

pub fn air_healthy(spi: &Spi) -> bool {
    matches!(http::get_json(&air_health_url(spi)), Ok((200, _)))
}

fn wait_healthy(spi: &Spi, which: Which, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if healthy(spi, which) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Start one mint. Returns a line for the operator.
pub fn start_one(spi: &Spi, which: Which, wait_secs: u64) -> Result<String, String> {
    if let Some(pid) = read_pid(spi, which)
        && alive(pid)
    {
        return Ok(format!("{} is already running (pid {pid})", which.name()));
    }
    if healthy(spi, which) {
        return Err(format!(
            "something already answers at {}: stop it first (spi did not start it)",
            spi.url(which)
        ));
    }
    let (keep, max_mb) = (spi.service.log_keep, spi.service.log_max_size_mb);
    let log = open_log(&spi.log_file(which), keep, max_mb)?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&spi.mint(which).bin);
    cmd.arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    for (k, v) in generate::env(spi, which) {
        cmd.env(k, v);
    }
    // Its own process group, so a Ctrl-C at the terminal that ran `spi start`
    // does not stop it.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let child = cmd.spawn().map_err(|e| {
        format!(
            "cannot run `{}`: {e}. Put it on PATH or set {}.bin in spi.toml",
            spi.mint(which).bin,
            which.env_prefix().to_lowercase()
        )
    })?;
    let pid = child.id();
    fs::write(spi.pid_file(which), format!("{pid}\n")).map_err(|e| e.to_string())?;
    if wait_healthy(spi, which, wait_secs) {
        Ok(format!(
            "{} started (pid {pid}), healthy at {}",
            which.name(),
            spi.url(which)
        ))
    } else {
        Err(format!(
            "{} started (pid {pid}) but did not answer /health within {wait_secs} s: see {}",
            which.name(),
            spi.log_file(which).display()
        ))
    }
}

/// Start the AIR server (the stock `air_server`) in its own folder, with the
/// environment `generate::air_env` gives it. Returns a line for the operator.
/// It needs its identity file, which is the operator's to make and to keep a
/// copy of offline: spi does not make one.
pub fn start_air(spi: &Spi, wait_secs: u64) -> Result<String, String> {
    let url = air_health_url(spi);
    if !spi.air.manage {
        return if air_healthy(spi) {
            Ok(format!(
                "air_server is not managed by spi (air.manage = false); it answers at {url}"
            ))
        } else {
            Err(format!(
                "air_server does not answer at {url} and air.manage is false: start it yourself"
            ))
        };
    }
    if let Some(pid) = read_pid_file(&spi.air_pid_file())
        && alive(pid)
    {
        return Ok(format!("air_server is already running (pid {pid})"));
    }
    if air_healthy(spi) {
        return Err(format!(
            "something already answers at {url}: stop it first (spi did not start it)"
        ));
    }
    let identity = spi.air_identity(false);
    if !Path::new(&identity).is_file() {
        return Err(format!(
            "the AIR server's identity file {identity} does not exist. Making it, and keeping a copy of it offline, is yours to do (air.identity in spi.toml names another place); spi does not make a server identity"
        ));
    }
    for dir in [spi.air_dir(), spi.dir.join("logs"), spi.dir.join("run")] {
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let (keep, max_mb) = (spi.service.log_keep, spi.service.log_max_size_mb);
    let log = open_log(&spi.air_log_file(), keep, max_mb)?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&spi.air.bin);
    cmd.current_dir(spi.air_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    for (k, v) in generate::air_env(spi, false) {
        cmd.env(k, v);
    }
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let child = cmd.spawn().map_err(|e| {
        format!(
            "cannot run `{}`: {e}. Put it on PATH or set air.bin in spi.toml",
            spi.air.bin
        )
    })?;
    let pid = child.id();
    fs::write(spi.air_pid_file(), format!("{pid}\n")).map_err(|e| e.to_string())?;
    let end = Instant::now() + Duration::from_secs(wait_secs);
    while Instant::now() < end {
        if air_healthy(spi) {
            return Ok(format!("air_server started (pid {pid}), healthy at {url}"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "air_server started (pid {pid}) but did not answer {url} within {wait_secs} s: see {}",
        spi.air_log_file().display()
    ))
}

/// Move `path` aside if it is over `max_mb` MB: it becomes `<path>.1`, the
/// copies already there move up one, and the one past `keep` is removed. For
/// `spi start`, which opens a log only when nothing is writing to it.
pub fn rotate_log(path: &Path, keep: u64, max_mb: u64) {
    if fs::metadata(path).is_ok_and(|m| m.len() > max_mb * 1024 * 1024) {
        let old = |n: u64| format!("{}.{n}", path.display());
        let _ = fs::remove_file(old(keep));
        for n in (1..keep).rev() {
            let _ = fs::rename(old(n), old(n + 1));
        }
        let _ = fs::rename(path, old(1));
    }
}

/// Open a log for appending, rotating it first if it is over the limit.
fn open_log(path: &Path, keep: u64, max_mb: u64) -> Result<fs::File, String> {
    rotate_log(path, keep, max_mb);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))
}

fn bitcoind_marker(spi: &Spi) -> std::path::PathBuf {
    spi.dir.join("run").join("bitcoind.started-by-spi")
}

/// The option that selects the chain on bitcoind's command line. Main has none.
pub fn chain_flag(network: Network) -> Option<&'static str> {
    match network {
        Network::Main => None,
        Network::Regtest => Some("-regtest"),
        Network::Signet => Some("-signet"),
        Network::Testnet => Some("-testnet"),
    }
}

/// The command line `spi` starts bitcoind with: in the foreground, with its whole
/// log on the console (spi writes that to `logs/bitcoind.log`). Bitcoin Core
/// trims `debug.log` only when it starts, so a node that runs for months would
/// grow it without limit.
pub fn bitcoind_command_line(spi: &Spi) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(flag) = chain_flag(spi.network) {
        args.push(flag.to_string());
    }
    if let Some(d) = &spi.bitcoind_datadir {
        args.push(format!("-datadir={d}"));
    }
    args.push("-nodebuglogfile".to_string());
    args.push("-printtoconsole".to_string());
    args.extend(spi.bitcoind_args.iter().cloned());
    args
}

/// Start bitcoind if it is not answering. Returns a line for the operator.
pub fn start_bitcoind(spi: &Spi, wait_secs: u64) -> Result<String, String> {
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    match rpc.node(spi.network.chain()) {
        Node::Syncing(_) | Node::Synced(_) => {
            return Ok(format!("bitcoind is already running at {}", spi.rpc_url));
        }
        Node::WrongChain { found } => {
            return Err(format!(
                "bitcoind at {} is on `{found}`, but the config says {}",
                spi.rpc_url,
                spi.network.name()
            ));
        }
        Node::Unreachable(_) => {}
    }
    if !spi.manage_bitcoind {
        return Err(format!(
            "bitcoind is unreachable at {} and bitcoind.manage is false: start it yourself",
            spi.rpc_url
        ));
    }
    let args = bitcoind_command_line(spi);
    let log_path = spi.bitcoind_log_file();
    if let Some(dir) = log_path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let (keep, max_mb) = (spi.service.log_keep, spi.service.log_max_size_mb);
    let log = open_log(&log_path, keep, max_mb)?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&spi.bitcoind_bin);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "cannot run `{}`: {e}. Put it on PATH or set bitcoind.bin in spi.toml",
            spi.bitcoind_bin
        )
    })?;
    let _ = fs::write(bitcoind_marker(spi), "started by spi\n");
    let end = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = fs::remove_file(bitcoind_marker(spi));
            return Err(format!(
                "`{} {}` exited ({status}) as it started: see {}",
                spi.bitcoind_bin,
                args.join(" "),
                log_path.display()
            ));
        }
        match rpc.node(spi.network.chain()) {
            Node::Unreachable(e) => {
                if Instant::now() >= end {
                    return Err(format!(
                        "bitcoind was started but is not answering after {wait_secs} s: {e}"
                    ));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            _ => {
                return Ok(format!(
                    "bitcoind started (`{} {}`)",
                    spi.bitcoind_bin,
                    args.join(" ")
                ));
            }
        }
    }
}

/// Wait until bitcoind has finished its initial block download. `Ok` with the
/// node's state once it has; `Err` with the reason if the chain is wrong, or
/// the wait (`timeout_secs`) runs out while it is still syncing or
/// unreachable. `progress` is told each new state, once.
pub fn wait_for_sync(
    spi: &Spi,
    timeout_secs: u64,
    poll: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<Chain, String> {
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    let end = Instant::now() + Duration::from_secs(timeout_secs);
    let mut last = String::new();
    loop {
        let node = rpc.node(spi.network.chain());
        let line = match &node {
            Node::Synced(c) => return Ok(c.clone()),
            Node::WrongChain { found } => {
                return Err(format!(
                    "bitcoind is on `{found}`, but the config says {}",
                    spi.network.name()
                ));
            }
            Node::Syncing(c) => format!("bitcoind syncing {}%", c.percent()),
            Node::Unreachable(e) => format!("bitcoind unreachable: {e}"),
        };
        if line != last {
            progress(&line);
            last = line.clone();
        }
        if Instant::now() >= end {
            return Err(format!(
                "{line} after waiting {timeout_secs} s: psp-mint and esp-mint were NOT started. bitcoind keeps going; run `spi start` again when `spi status` shows it synced"
            ));
        }
        std::thread::sleep(poll);
    }
}

/// Start bitcoind (if needed), wait for it to finish syncing, then the AIR
/// server, then psp-mint and esp-mint (which needs the PSP).
pub fn start(spi: &Spi, wait_secs: u64) -> Result<Vec<String>, String> {
    // Both are checked before anything is started: the mints cannot serve
    // without the AIR server's name, and the AIR server cannot start without
    // its identity.
    if spi.air.name.is_none() {
        return Err(
            "[air] name is not set: the mints reach wallets through the AIR server, so write its name (its public key, 66 hex characters) in spi.toml"
                .into(),
        );
    }
    if spi.air.manage && !Path::new(&spi.air_identity(false)).is_file() {
        return Err(format!(
            "the AIR server's identity file {} does not exist. Making it, and keeping a copy of it offline, is yours to do (air.identity in spi.toml names another place)",
            spi.air_identity(false)
        ));
    }
    commands::write_configs(spi)?;
    let mut lines = vec![format!("network: {}", spi.network.name())];
    lines.push(start_bitcoind(spi, 120)?);
    let mut seen = Vec::new();
    let waited = wait_for_sync(
        spi,
        spi.sync_wait_secs,
        Duration::from_secs(2),
        &mut |l: &str| seen.push(l.to_string()),
    );
    lines.extend(seen);
    match waited {
        Ok(c) => lines.push(format!("bitcoind {}", c.describe())),
        Err(e) => return Err(format!("{}\n{e}", lines.join("\n"))),
    }
    lines.push(start_air(spi, wait_secs)?);
    for which in [Which::Psp, Which::Esp] {
        lines.push(start_one(spi, which, wait_secs)?);
    }
    Ok(lines)
}

/// Stop one process with SIGTERM and wait for it to finish its requests.
fn stop_pid(name: &str, pid_file: &Path, wait_secs: u64) -> Result<String, String> {
    let Some(pid) = read_pid_file(pid_file) else {
        return Ok(format!("{name} was not started by spi (no pid file)"));
    };
    if !alive(pid) {
        let _ = fs::remove_file(pid_file);
        return Ok(format!("{name} was not running (stale pid {pid})"));
    }
    signal("TERM", pid)?;
    let end = Instant::now() + Duration::from_secs(wait_secs);
    while Instant::now() < end {
        if !alive(pid) {
            let _ = fs::remove_file(pid_file);
            return Ok(format!("{name} stopped"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "{name} (pid {pid}) is still running {wait_secs} s after SIGTERM"
    ))
}

/// Stop one mint with SIGTERM and wait for it to finish its requests.
pub fn stop_one(spi: &Spi, which: Which, wait_secs: u64) -> Result<String, String> {
    stop_pid(which.name(), &spi.pid_file(which), wait_secs)
}

/// Stop the AIR server if spi started it.
pub fn stop_air(spi: &Spi, wait_secs: u64) -> Result<String, String> {
    stop_pid("air_server", &spi.air_pid_file(), wait_secs)
}

/// The settings of psp-mint that a reload cannot apply: the ones its README marks
/// "needs a restart" (its `config.rs` `SPECS` with `reload: false`). Kept in step
/// with it by hand.
pub const RESTART_ONLY_PSP: &[&str] = &[
    "server.bind",
    "bitcoin.network",
    "bitcoin.wallet",
    "mint.max_denomination_units",
    "rotation.activation_hours",
    "rotation.period_hours",
    "storage.mint_state",
    "air.server",
    "air.host",
    "air.held_reads",
    "air.timeout_secs",
];

/// The same for esp-mint.
pub const RESTART_ONLY_ESP: &[&str] = &[
    "server.network",
    "server.bind",
    "server.admin_bind",
    "psp.url",
    "rotation.activation_hours",
    "rotation.period_hours",
    "admin.max_request_bytes",
    "storage.mint_state",
    "air.server",
    "air.host",
    "air.held_reads",
    "air.timeout_secs",
];

fn value_at(t: &toml::Table, key: &str) -> Option<String> {
    let mut parts = key.split('.');
    let mut here = t.get(parts.next()?)?;
    for part in parts {
        here = here.as_table()?.get(part)?;
    }
    Some(here.to_string())
}

/// The restart-only settings whose value differs between the generated file as it
/// was (`old`) and as it is now (`new`), as `(key, old value, new value)`. With
/// no earlier file there is nothing to compare, so nothing is named.
pub fn changed_restart_only(
    which: Which,
    old: Option<&str>,
    new: &str,
) -> Vec<(String, String, String)> {
    let (Some(old), Ok(new)) = (old, new.parse::<toml::Table>()) else {
        return Vec::new();
    };
    let Ok(old) = old.parse::<toml::Table>() else {
        return Vec::new();
    };
    let keys = match which {
        Which::Psp => RESTART_ONLY_PSP,
        Which::Esp => RESTART_ONLY_ESP,
    };
    keys.iter()
        .filter_map(|key| {
            let (a, b) = (value_at(&old, key), value_at(&new, key));
            (a != b).then(|| {
                (
                    (*key).to_string(),
                    a.unwrap_or_else(|| "(not set)".into()),
                    b.unwrap_or_else(|| "(not set)".into()),
                )
            })
        })
        .collect()
}

/// The lines that say which restart-only settings of a mint changed, that they
/// wait for a restart, and how to do one. Empty when none changed.
pub fn restart_lines(name: &str, changed: &[(String, String, String)]) -> Vec<String> {
    if changed.is_empty() {
        return Vec::new();
    }
    let one = changed.len() == 1;
    let mut lines = vec![format!(
        "{name}: {} changed that {} a restart (a brief downtime), so the running mint keeps the old {} until it is restarted; this reload restarts nothing:",
        if one {
            "1 setting".to_string()
        } else {
            format!("{} settings", changed.len())
        },
        if one { "needs" } else { "need" },
        if one { "value" } else { "values" }
    )];
    for (key, old, new) in changed {
        lines.push(format!("  {key}: {old} -> {new}"));
    }
    lines.push(format!(
        "{name}: to apply {}, run `spi restart {name}` (a mint started with `spi start`: `spi stop`, then `spi start`). {}",
        if changed.len() == 1 { "it" } else { "them" },
        crate::upgrade::MINT_DOWNTIME
    ));
    lines
}

/// Write the config files again and send SIGHUP to each mint spi started.
/// A mint reads its file again and applies what can change while it runs; it is
/// not stopped. Returns a line for each, and names every restart-only setting
/// whose value changed in the file (it waits for a restart; nothing is
/// restarted here).
pub fn reload(spi: &Spi) -> Result<Vec<String>, String> {
    let before = [Which::Psp, Which::Esp].map(|w| fs::read_to_string(spi.generated_config(w)).ok());
    commands::write_configs(spi)?;
    let mut lines = vec!["wrote the config files again".to_string()];
    for (which, old) in [Which::Psp, Which::Esp].into_iter().zip(before) {
        match read_pid(spi, which) {
            Some(pid) if alive(pid) => {
                signal("HUP", pid)?;
                lines.push(format!(
                    "{} (pid {pid}): sent SIGHUP; the settings that can change while it runs apply now (its log says which), and a setting that needs a restart keeps its running value",
                    which.name()
                ));
            }
            _ => lines.push(format!(
                "{} was not started by spi, so nothing is signalled. A service reloads with `systemctl reload {}` (Linux) or `launchctl kill HUP system/org.servicepoints.{}` (macOS)",
                which.name(),
                which.name(),
                which.name()
            )),
        }
        let now = fs::read_to_string(spi.generated_config(which)).unwrap_or_default();
        let changed = changed_restart_only(which, old.as_deref(), &now);
        lines.extend(restart_lines(which.name(), &changed));
    }
    Ok(lines)
}

/// Stop bitcoind if `spi start` started it (never one it found running).
pub fn stop_bitcoind(spi: &Spi, wait_secs: u64) -> Result<Option<String>, String> {
    if !bitcoind_marker(spi).exists() {
        return Ok(None);
    }
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    if matches!(rpc.node(spi.network.chain()), Node::Unreachable(_)) {
        let _ = fs::remove_file(bitcoind_marker(spi));
        return Ok(Some("bitcoind was not running".into()));
    }
    rpc.call(None, "stop", serde_json::json!([]))?;
    let end = Instant::now() + Duration::from_secs(wait_secs);
    while Instant::now() < end {
        if matches!(rpc.node(spi.network.chain()), Node::Unreachable(_)) {
            let _ = fs::remove_file(bitcoind_marker(spi));
            return Ok(Some("bitcoind stopped".into()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "bitcoind is still answering {wait_secs} s after `stop`"
    ))
}

/// Stop esp-mint, then psp-mint, then the AIR server if spi started it, then
/// bitcoind if spi started it.
pub fn stop(spi: &Spi, wait_secs: u64) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut failed = None;
    for which in [Which::Esp, Which::Psp] {
        match stop_one(spi, which, wait_secs) {
            Ok(l) => lines.push(l),
            Err(e) => {
                lines.push(e.clone());
                failed = Some(e);
            }
        }
    }
    match stop_air(spi, wait_secs) {
        Ok(l) => lines.push(l),
        Err(e) => {
            lines.push(e.clone());
            failed = Some(e);
        }
    }
    match stop_bitcoind(spi, wait_secs) {
        Ok(Some(l)) => lines.push(l),
        Ok(None) => {}
        Err(e) => {
            lines.push(e.clone());
            failed = Some(e);
        }
    }
    match failed {
        None => Ok(lines),
        Some(e) => Err(format!("{}\n{e}", lines.join("\n"))),
    }
}
